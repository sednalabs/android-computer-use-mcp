//! Image-bearing Android workflows with durable, non-replayable mutation receipts.

use crate::McpError;
use crate::config::AndroidExecutionTarget;
use crate::execution::{self, Claim, LedgerRecord, ObservationRecord};
use crate::interactive_session::InteractiveSessionInstallBuildArgs;
use crate::server::AndroidEmulatorMcp;
use crate::tools::*;
use crate::ui::{UiSelectorInput, ensure_selector_not_empty, normalize_selector_input};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, RawContent};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::time::timeout;

const MAX_ACTIONS: usize = 16;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECEIPT_BYTES: usize = 16 * 1024;
const OBSERVATION_TTL: Duration = Duration::from_secs(30);
const BATCH_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObserveArgs {
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub target: Option<AndroidExecutionTarget>,
    /// Maximum UI capture time before screenshot fallback (1–15 seconds).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepAction {
    /// One documented Android input, semantic or non-destructive lifecycle tool.
    #[schemars(with = "StepToolSchema")]
    pub tool: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepArgs {
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub target: Option<AndroidExecutionTarget>,
    /// Stable caller-generated identifier. Reuse never executes the batch again.
    pub action_id: String,
    #[schemars(length(min = 1, max = 16))]
    pub actions: Vec<StepAction>,
    /// Required for a coordinate gesture, which must be first in the batch.
    #[serde(default)]
    pub observation_id: Option<String>,
    /// Also reject any pixel change; leave false for animation within an unchanged window.
    #[serde(default)]
    pub strict_frame_match: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstallBuildWorkflowArgs {
    /// Stable caller-generated identifier; preserve it after an uncertain install.
    pub action_id: String,
    pub build: InteractiveSessionInstallBuildArgs,
}

macro_rules! prepared_actions {
    ($($variant:ident($args:ty) => $name:literal => $method:ident),+ $(,)?) => {
        /// Public schema of permitted composite actions.
        #[derive(JsonSchema)]
        pub enum StepToolSchema { $(#[serde(rename = $name)] $variant),+ }
        #[derive(Debug)]
        enum PreparedAction { $($variant($args)),+ }
        impl PreparedAction {
            fn parse(name: &str, arguments: Value) -> Result<Self, McpError> {
                match name {
                    $($name => parse_action_arguments::<$args>(name, arguments).map(Self::$variant),)+
                    _ => Err(McpError::invalid_params("Unsupported step tool; nested workflows, installs and uninstalls are not allowed", None)),
                }
            }
            fn dispatch(self, server: &AndroidEmulatorMcp) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CallToolResult, McpError>> + Send + '_>> {
                match self { $(Self::$variant(args) => Box::pin(server.$method(Parameters(args)))),+ }
            }
        }
    }
}

prepared_actions! {
    Tap(TapArgs) => "android.input.tap" => android_input_tap,
    Text(TextArgs) => "android.input.text" => android_input_text,
    Swipe(SwipeArgs) => "android.input.swipe" => android_input_swipe,
    Keyevent(KeyeventArgs) => "android.input.keyevent" => android_input_keyevent,
    DoubleTap(DoubleTapArgs) => "android.input.double_tap" => android_input_double_tap,
    LongPress(LongPressArgs) => "android.input.long_press" => android_input_long_press,
    MultiTouch(MultiTouchArgs) => "android.input.multi_touch" => android_input_multi_touch,
    Keycombination(KeycombinationArgs) => "android.input.keycombination" => android_input_keycombination,
    TapElement(TapElementArgs) => "android.tap_element" => android_tap_element,
    TypeElement(TypeIntoElementArgs) => "android.type_into_element" => android_type_into_element,
    Scroll(ScrollUntilVisibleArgs) => "android.scroll_until_visible" => android_scroll_until_visible,
    Launch(LaunchAppArgs) => "android.launch_app" => android_launch_app,
    Terminate(PackageNameArgs) => "android.terminate_app" => android_terminate_app,
    OpenUrl(OpenUrlArgs) => "android.open_url" => android_open_url,
    Orientation(SetOrientationArgs) => "android.set_orientation" => android_set_orientation,
}

fn parse_action_arguments<T: serde::de::DeserializeOwned + JsonSchema>(
    name: &str,
    arguments: Value,
) -> Result<T, McpError> {
    let schema = serde_json::to_value(schemars::schema_for!(T))
        .map_err(|error| invalid(&error.to_string()))?;
    let properties = schema["properties"]
        .as_object()
        .ok_or_else(|| invalid("Action schema is unavailable"))?;
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid("Action arguments must be an object"))?;
    for key in object.keys() {
        let compatibility_alias = (name == "android.input.keyevent" && key == "key")
            || (name == "android.input.keycombination" && key == "keys");
        if !properties.contains_key(key) && !compatibility_alias {
            return Err(invalid(&format!("Unknown argument {key} for {name}")));
        }
    }
    serde_json::from_value(arguments)
        .map_err(|error| invalid(&format!("Invalid {name} arguments: {error}")))
}

fn invalid(message: &str) -> McpError {
    McpError::invalid_params(message.to_string(), None)
}

fn reject_install_only_provenance(target: Option<&AndroidExecutionTarget>) -> Result<(), McpError> {
    if target.is_some_and(|target| target.expected_build.is_some()) {
        return Err(invalid(
            "target.expected_build is supported only by hosted install/relaunch; observe and step do not verify active build provenance",
        ));
    }
    Ok(())
}

fn request_hash(value: &impl Serialize) -> Result<String, McpError> {
    let bytes = serde_json::to_vec(value).map_err(|error| invalid(&error.to_string()))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(invalid("Workflow request exceeds 64 KiB"));
    }
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn coordinate_action(tool: &str) -> bool {
    matches!(
        tool,
        "android.input.tap"
            | "android.input.double_tap"
            | "android.input.long_press"
            | "android.input.swipe"
            | "android.input.multi_touch"
    )
}

fn validate_action_arguments(action: &StepAction, serial: &str) -> Result<(), McpError> {
    if let Some(value) = action.arguments.get("serial")
        && !value.is_null()
        && value.as_str().map(str::trim) != Some(serial)
    {
        return Err(invalid(
            "Every action must target the batch's exact device serial",
        ));
    }
    if action.arguments.contains_key("target") || action.arguments.contains_key("execution_target")
    {
        return Err(invalid(
            "Set the exact target on the batch, not individual actions",
        ));
    }
    for field in ["retry_with_adb_on_no_change", "allow_verification_failure"] {
        if action.arguments.get(field).and_then(Value::as_bool) == Some(true) {
            return Err(invalid(
                "Step actions cannot enable replay or ignore verification failure",
            ));
        }
    }
    for field in ["selector", "wait_for_selector", "expect_focus_selector"] {
        if let Some(value) = action.arguments.get(field).filter(|value| !value.is_null()) {
            let input: UiSelectorInput =
                serde_json::from_value(value.clone()).map_err(|_| invalid("Invalid selector"))?;
            ensure_selector_not_empty(&normalize_selector_input(input))?;
        }
    }
    if let Some(seconds) = action.arguments.get("timeout_secs").and_then(Value::as_u64)
        && !(1..=90).contains(&seconds)
    {
        return Err(invalid("Action timeout_secs must be between 1 and 90"));
    }
    if let Some(milliseconds) = action.arguments.get("duration_ms").and_then(Value::as_u64)
        && !(50..=2000).contains(&milliseconds)
    {
        return Err(invalid("Gesture duration_ms must be between 50 and 2000"));
    }
    if let Some(count) = action.arguments.get("max_swipes").and_then(Value::as_u64)
        && !(1..=20).contains(&count)
    {
        return Err(invalid("max_swipes must be between 1 and 20"));
    }
    if let Some(direction) = action.arguments.get("direction").and_then(Value::as_str)
        && !matches!(direction, "up" | "down" | "left" | "right")
    {
        return Err(invalid("Unsupported scroll direction"));
    }
    for key in [
        "package_name",
        "activity",
        "wait_for_package",
        "wait_for_activity",
    ] {
        if let Some(value) = action.arguments.get(key).and_then(Value::as_str)
            && (value.is_empty()
                || !value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '$' | '/')
                }))
        {
            return Err(invalid("Invalid Android package or activity name"));
        }
    }
    if let Some(url) = action.arguments.get("url").and_then(Value::as_str) {
        let parsed = reqwest::Url::parse(url).map_err(|_| invalid("Invalid URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(invalid("Step URL must use http or https and name a host"));
        }
    }
    if let Some(orientation) = action.arguments.get("orientation").and_then(Value::as_str)
        && !matches!(
            orientation,
            "portrait" | "landscape" | "reverse_portrait" | "reverse_landscape"
        )
    {
        return Err(invalid("Unsupported orientation"));
    }
    for key in ["keycode", "key"] {
        if let Some(value) = action.arguments.get(key).and_then(Value::as_str) {
            validate_key(value)?;
        }
    }
    for key in ["keycodes", "keys"] {
        if let Some(values) = action.arguments.get(key).and_then(Value::as_array) {
            if values.is_empty() || values.len() > 8 {
                return Err(invalid("Key combinations require 1–8 keys"));
            }
            for value in values {
                validate_key(value.as_str().ok_or_else(|| invalid("Invalid keycode"))?)?;
            }
        }
    }
    if action.tool == "android.input.keycombination"
        && !["keycodes", "keys", "keycode", "key"].iter().any(|key| {
            action
                .arguments
                .get(*key)
                .is_some_and(|value| !value.is_null())
        })
    {
        return Err(invalid("Key combination requires at least one key"));
    }
    if action.tool == "android.input.multi_touch"
        && action
            .arguments
            .get("pointers")
            .and_then(Value::as_array)
            .is_none_or(|pointers| !(2..=5).contains(&pointers.len()))
    {
        return Err(invalid("Atomic multi-touch requires 2–5 pointers"));
    }
    Ok(())
}

fn validate_key(value: &str) -> Result<(), McpError> {
    if value.is_empty()
        || value.len() > 80
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        Err(invalid("Invalid Android keycode"))
    } else {
        Ok(())
    }
}

fn prepare_actions(args: &StepArgs, serial: &str) -> Result<Vec<PreparedAction>, McpError> {
    request_hash(args)?;
    if args.actions.is_empty() || args.actions.len() > MAX_ACTIONS {
        return Err(invalid("A step requires 1–16 actions"));
    }
    let mut prepared = Vec::with_capacity(args.actions.len());
    for (index, action) in args.actions.iter().enumerate() {
        validate_action_arguments(action, serial)?;
        if coordinate_action(&action.tool)
            && (index != 0 || args.observation_id.as_deref().is_none_or(str::is_empty))
        {
            return Err(invalid(
                "A coordinate gesture must be the first action and requires a fresh observation_id",
            ));
        }
        let mut arguments = action.arguments.clone();
        arguments.insert("serial".to_string(), json!(serial));
        prepared.push(PreparedAction::parse(
            &action.tool,
            Value::Object(arguments),
        )?);
    }
    Ok(prepared)
}

fn receipt_from_result(result: &CallToolResult) -> Result<Value, McpError> {
    if let Some(value) = &result.structured_content {
        return Ok(value.clone());
    }
    for content in &result.content {
        if let RawContent::Text(text) = &content.raw
            && let Ok(value) = serde_json::from_str::<Value>(&text.text)
            && value.is_object()
        {
            return Ok(value);
        }
    }
    Err(McpError::internal_error(
        "Provider returned no JSON receipt",
        None,
    ))
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), McpError> {
    const MAX_DECODED_BYTES: usize = 64 * 1024 * 1024;
    if bytes.len() < 33 || bytes.len() > MAX_IMAGE_BYTES || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(McpError::internal_error(
            "Observation did not contain a bounded PNG image",
            None,
        ));
    }
    let mut decoder = png::Decoder::new_with_limits(
        std::io::Cursor::new(bytes),
        png::Limits {
            bytes: MAX_DECODED_BYTES,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder
        .read_info()
        .map_err(|_| McpError::internal_error("Invalid PNG header or checksum", None))?;
    if reader.info().animation_control.is_some() {
        return Err(invalid("An observation must contain one static PNG frame"));
    }
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= MAX_DECODED_BYTES)
        .ok_or_else(|| invalid("PNG decoded image exceeds the observation budget"))?;
    let mut decoded = vec![0; size];
    let output = reader
        .next_frame(&mut decoded)
        .map_err(|_| McpError::internal_error("Invalid or truncated PNG pixel data", None))?;
    reader
        .finish()
        .map_err(|_| McpError::internal_error("Invalid PNG end marker or checksum", None))?;
    Ok((output.width, output.height))
}

fn image_from_result(result: &CallToolResult) -> Result<Vec<u8>, McpError> {
    let images = result
        .content
        .iter()
        .filter_map(|content| match &content.raw {
            RawContent::Image(image) => Some(image),
            _ => None,
        })
        .collect::<Vec<_>>();
    if images.len() != 1
        || images[0].mime_type != "image/png"
        || images[0].data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
    {
        return Err(McpError::internal_error(
            "Expected one bounded PNG observation",
            None,
        ));
    }
    let bytes = STANDARD
        .decode(&images[0].data)
        .map_err(|_| McpError::internal_error("Invalid PNG encoding", None))?;
    png_dimensions(&bytes)?;
    Ok(bytes)
}

fn json_image_result(receipt: Value, image: Option<Vec<u8>>) -> Result<CallToolResult, McpError> {
    let is_error = receipt.get("ok").and_then(Value::as_bool) != Some(true);
    let text = serde_json::to_string(&receipt)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
    let mut content = vec![Content::text(text)];
    if let Some(bytes) = image {
        png_dimensions(&bytes)?;
        content.push(Content::image(STANDARD.encode(bytes), "image/png"));
    }
    let mut result = CallToolResult::success(content);
    result.is_error = Some(is_error);
    Ok(result)
}

fn bounded_value(mut value: Value) -> Value {
    fn trim_diagnostics(value: &mut Value, remaining: &mut usize) -> bool {
        let mut truncated = false;
        match value {
            Value::Object(object) => {
                for (key, field) in object.iter_mut() {
                    if (key.ends_with("stdout") || key.ends_with("stderr")) && field.is_string() {
                        let text = field.as_str().expect("checked diagnostic string");
                        let limit = (*remaining).min(1024);
                        if text.len() > limit {
                            let mut prefix = String::new();
                            for character in text.chars() {
                                if prefix.len() + character.len_utf8() > limit {
                                    break;
                                }
                                prefix.push(character);
                            }
                            *remaining -= prefix.len();
                            prefix.push_str(" [diagnostic truncated]");
                            *field = json!(prefix);
                            truncated = true;
                        } else {
                            *remaining -= text.len();
                        }
                    } else {
                        truncated |= trim_diagnostics(field, remaining);
                    }
                }
            }
            Value::Array(array) => {
                for field in array {
                    truncated |= trim_diagnostics(field, remaining);
                }
            }
            _ => {}
        }
        truncated
    }
    // Effect and provenance fields remain authoritative even when diagnostics
    // are large. The durable ledger separately bounds the complete record.
    let mut remaining = MAX_RECEIPT_BYTES;
    if trim_diagnostics(&mut value, &mut remaining)
        && let Some(object) = value.as_object_mut()
    {
        object.insert("diagnostics_truncated".into(), json!(true));
    }
    value
}

fn error_receipt(error: &McpError) -> Value {
    json!({ "code": error.code, "message": error.message.chars().take(512).collect::<String>(), "data": error.data.clone().map(bounded_value) })
}

fn verified_receipt(value: &Value) -> bool {
    ["postcondition", "verification", "text_verification"]
        .iter()
        .any(|key| {
            let proof = &value[*key];
            proof["requested"].as_bool() == Some(true) && proof["satisfied"].as_bool() == Some(true)
        })
}

fn validate_action_id(action_id: &str) -> Result<(), McpError> {
    if action_id.is_empty()
        || action_id.len() > 128
        || !action_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
    {
        return Err(invalid(
            "action_id must be 1–128 ASCII letters, digits, periods, colons, underscores or hyphens",
        ));
    }
    Ok(())
}

fn initial_receipt(action_id: &str, serial: &str, epoch: &str) -> Value {
    json!({
        "ok": false, "outcome": "outcome_unknown", "action_id": action_id,
        "serial": serial, "provider_epoch": epoch, "duplicate_action_id": false,
        "executed_action_count": 0, "attempted_action_count": 0,
        "failed_index": null, "actions": [], "retry_safe": false, "do_not_replay": true,
    })
}

impl AndroidEmulatorMcp {
    async fn workflow_boot_id(&self, serial: &str) -> Result<String, McpError> {
        let output = timeout(
            Duration::from_secs(3),
            self.run_adb_shell(serial, ["cat", "/proc/sys/kernel/random/boot_id"]),
        )
        .await
        .map_err(|_| McpError::internal_error("Device boot identity timed out", None))??;
        let value = output.stdout.trim();
        if value.is_empty()
            || value.len() > 128
            || !value
                .chars()
                .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            return Err(McpError::internal_error(
                "Device boot identity is unavailable",
                None,
            ));
        }
        Ok(value.to_owned())
    }

    async fn workflow_coordinate_context(
        &self,
        serial: &str,
    ) -> Result<(String, String), McpError> {
        let result = timeout(
            Duration::from_secs(3),
            self.android_get_orientation(Parameters(SerialArgs {
                serial: Some(serial.into()),
            })),
        )
        .await
        .map_err(|_| invalid("Display orientation unavailable"))??;
        let receipt = receipt_from_result(&result)?;
        let rotation = receipt["raw_rotation"]
            .as_u64()
            .filter(|rotation| *rotation < 4)
            .ok_or_else(|| invalid("Display rotation unavailable"))?;
        let window = timeout(
            Duration::from_secs(3),
            self.window_state_internal_with_timeout(serial, Duration::from_secs(3)),
        )
        .await
        .map_err(|_| invalid("Window identity unavailable"))??;
        if window.current_focus.is_none()
            && window.focused_app.is_none()
            && window.resumed_activity.is_none()
        {
            return Err(invalid("Window identity unavailable"));
        }
        let identity =
            serde_json::to_vec(&window).map_err(|_| invalid("Window identity unavailable"))?;
        Ok((
            rotation.to_string(),
            format!("{:x}", Sha256::digest(identity)),
        ))
    }

    async fn workflow_observation(
        &self,
        serial: &str,
        ui_budget: Duration,
    ) -> (Value, Option<Vec<u8>>) {
        let started = Instant::now();
        let before_boot = self.workflow_boot_id(serial).await.ok();
        let mut errors = Vec::new();
        let mut observation = json!({ "serial": serial, "ui_available": false });
        let mut image = None;
        match timeout(
            ui_budget,
            self.android_inspect_ui(Parameters(InspectUiArgs {
                serial: Some(serial.into()),
                hierarchy_filename: None,
                include_screenshot: true,
                screenshot_filename: None,
            })),
        )
        .await
        {
            Ok(Ok(result)) => match (receipt_from_result(&result), image_from_result(&result)) {
                (Ok(mut receipt), Ok(bytes)) => {
                    if let Some(object) = receipt.as_object_mut() {
                        object.remove("nodes");
                    }
                    observation = receipt;
                    observation["ui_available"] = json!(true);
                    image = Some(bytes);
                }
                _ => errors.push("UI observation did not contain a valid image and receipt"),
            },
            Ok(Err(_)) => errors.push("UI hierarchy capture unavailable; used screenshot fallback"),
            Err(_) => errors.push("UI hierarchy capture timed out; used screenshot fallback"),
        }
        if image.is_none() {
            match timeout(
                Duration::from_secs(10),
                self.android_capture_screenshot(Parameters(ScreenshotArgs {
                    serial: Some(serial.into()),
                    filename: None,
                })),
            )
            .await
            {
                Ok(Ok(result)) => match image_from_result(&result) {
                    Ok(bytes) => {
                        if let Ok(receipt) = receipt_from_result(&result) {
                            observation["screenshot_receipt"] = receipt;
                        }
                        image = Some(bytes);
                    }
                    Err(_) => errors.push("Screenshot fallback returned invalid PNG bytes"),
                },
                _ => errors.push("Screenshot capture unavailable"),
            }
        }
        let coordinate_context = if image.is_some() {
            self.workflow_coordinate_context(serial).await.ok()
        } else {
            None
        };
        let after_boot = self.workflow_boot_id(serial).await.ok();
        let stable_boot = before_boot.filter(|before| Some(before) == after_boot.as_ref());
        let sequence = self
            .workflow
            .next_observation
            .fetch_add(1, Ordering::Relaxed);
        let id = format!(
            "{}:{sequence}",
            self.config.execution_identity.provider_epoch
        );
        observation["serial"] = json!(serial);
        observation["observation_id"] = json!(id);
        observation["provider_epoch"] = json!(self.config.execution_identity.provider_epoch);
        observation["captured_at"] = json!(chrono::Utc::now().to_rfc3339());
        observation["elapsed_ms"] = json!(started.elapsed().as_millis());
        observation["screenshot_present"] = json!(image.is_some());
        let frame_fresh = started.elapsed() <= OBSERVATION_TTL;
        observation["coordinate_actions_available"] = json!(
            image.is_some() && stable_boot.is_some() && coordinate_context.is_some() && frame_fresh
        );
        observation["boot_id"] = json!(stable_boot);
        if stable_boot.is_none() {
            errors.push("Stable device boot identity unavailable; coordinate actions are disabled");
        }
        if coordinate_context.is_none() {
            errors.push("Display or window identity unavailable; coordinate actions are disabled");
        }
        if !frame_fresh {
            errors.push("Observation exceeded coordinate freshness budget; observe again");
        }
        observation["coordinate_policy"] = json!("boot_geometry_window_and_ttl");
        observation["degraded"] = json!(!errors.is_empty());
        observation["notes"] = json!(errors);
        if let Some(bytes) = &image {
            let digest = format!("{:x}", Sha256::digest(bytes));
            let (width, height) = png_dimensions(bytes).expect("validated image");
            observation["screenshot_sha256"] = json!(digest);
            observation["width"] = json!(width);
            observation["height"] = json!(height);
            if let (Some(boot_id), Some((orientation, window_identity))) =
                (stable_boot, coordinate_context)
                && frame_fresh
            {
                observation["rotation_quarter_turns"] =
                    json!(orientation.parse::<u8>().expect("validated rotation"));
                observation["window_identity"] = json!(window_identity);
                let mut observations = self.workflow.observations.lock().await;
                observations.retain(|_, record| record.captured.elapsed() <= OBSERVATION_TTL);
                observations.insert(
                    serial.to_owned(),
                    ObservationRecord {
                        id,
                        captured: started,
                        screenshot_sha256: digest,
                        boot_id,
                        width,
                        height,
                        orientation,
                        window_identity,
                    },
                );
            } else {
                self.workflow.invalidate(serial).await;
            }
        } else {
            self.workflow.invalidate(serial).await;
        }
        (observation, image)
    }

    async fn attach_workflow_observation(
        &self,
        serial: &str,
        mut receipt: Value,
    ) -> Result<CallToolResult, McpError> {
        let (observation, image) = self
            .workflow_observation(serial, Duration::from_secs(10))
            .await;
        receipt["observation_id"] = observation["observation_id"].clone();
        receipt["observation"] = observation;
        if image.is_none() {
            receipt["mutation_outcome"] = receipt["outcome"].clone();
            receipt["ok"] = json!(false);
            receipt["outcome"] = json!("observe_degraded");
        }
        json_image_result(receipt, image)
    }

    async fn duplicate_workflow_receipt(
        &self,
        serial: &str,
        record: LedgerRecord,
    ) -> Result<CallToolResult, McpError> {
        let mut receipt = record.receipt;
        receipt["duplicate_action_id"] = json!(true);
        receipt["retry_safe"] = json!(false);
        receipt["do_not_replay"] = json!(true);
        if record.state != "complete" {
            receipt["ok"] = json!(false);
            receipt["outcome"] = json!("outcome_unknown");
            receipt["reconciliation_required"] = json!(true);
        }
        self.attach_workflow_observation(serial, receipt).await
    }

    async fn verify_coordinate_observation(
        &self,
        args: &StepArgs,
        serial: &str,
    ) -> Result<Option<String>, McpError> {
        let first = &args.actions[0];
        if !coordinate_action(&first.tool) {
            return Ok(None);
        }
        let (
            digest,
            boot_id,
            expected_width,
            expected_height,
            expected_orientation,
            expected_window,
            captured,
        ) = {
            let observations = self.workflow.observations.lock().await;
            let record = observations.get(serial).ok_or_else(|| invalid("No current observation for this device; observe again before using coordinates"))?;
            if Some(record.id.as_str()) != args.observation_id.as_deref()
                || record.captured.elapsed() > OBSERVATION_TTL
            {
                return Err(invalid(
                    "Coordinate observation is stale; observe again before dispatch",
                ));
            }
            (
                record.screenshot_sha256.clone(),
                record.boot_id.clone(),
                record.width,
                record.height,
                record.orientation.clone(),
                record.window_identity.clone(),
                record.captured,
            )
        };
        let current_boot = self.workflow_boot_id(serial).await?;
        if current_boot != boot_id {
            return Err(invalid("Device restarted after observation; observe again"));
        }
        let result = timeout(
            Duration::from_secs(10),
            self.android_capture_screenshot(Parameters(ScreenshotArgs {
                serial: Some(serial.into()),
                filename: None,
            })),
        )
        .await
        .map_err(|_| invalid("Cannot verify coordinate frame freshness"))??;
        let bytes = image_from_result(&result)?;
        if args.strict_frame_match && format!("{:x}", Sha256::digest(&bytes)) != digest {
            return Err(invalid(
                "Screen changed after observation; observe again before using coordinates",
            ));
        }
        if self.workflow_boot_id(serial).await? != boot_id {
            return Err(invalid("Device changed during coordinate validation"));
        }
        let (width, height) = png_dimensions(&bytes)?;
        let (orientation, window_identity) = self.workflow_coordinate_context(serial).await?;
        if (width, height) != (expected_width, expected_height)
            || orientation != expected_orientation
            || window_identity != expected_window
        {
            return Err(invalid(
                "Display geometry, orientation or window changed; observe again before using coordinates",
            ));
        }
        if captured.elapsed() > OBSERVATION_TTL {
            return Err(invalid(
                "Coordinate observation expired during validation; observe again",
            ));
        }
        let paths = if first.tool == "android.input.multi_touch" {
            first.arguments["pointers"]
                .as_array()
                .expect("prevalidated pointers")
                .iter()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let single = Value::Object(first.arguments.clone());
        let points = if paths.is_empty() {
            vec![&single]
        } else {
            paths
        };
        for point in points {
            for (x_key, y_key) in [("x", "y"), ("x1", "y1"), ("x2", "y2")] {
                if let (Some(x), Some(y)) = (point[x_key].as_u64(), point[y_key].as_u64())
                    && (x >= u64::from(width) || y >= u64::from(height))
                {
                    return Err(invalid("Coordinate lies outside the observed screen"));
                }
            }
        }
        Ok(Some(boot_id))
    }
}

impl AndroidEmulatorMcp {
    async fn ledger_failure(
        &self,
        serial: &str,
        mut receipt: Value,
        error: &McpError,
    ) -> Result<CallToolResult, McpError> {
        receipt["ok"] = json!(false);
        receipt["outcome"] = json!("outcome_unknown");
        receipt["retry_safe"] = json!(false);
        receipt["do_not_replay"] = json!(true);
        receipt["ledger_error"] = error_receipt(error);
        self.attach_workflow_observation(serial, receipt).await
    }

    async fn complete_workflow(
        &self,
        serial: &str,
        path: &Path,
        mut record: LedgerRecord,
    ) -> Result<CallToolResult, McpError> {
        record.state = "complete".to_owned();
        if let Err(error) = execution::persist(path, &record).await {
            return self.ledger_failure(serial, record.receipt, &error).await;
        }
        // Mutation truth is durable before fallible screenshot/UI work begins.
        self.attach_workflow_observation(serial, record.receipt)
            .await
    }
}

#[tool_router(router = tool_router_workflow, vis = "pub")]
impl AndroidEmulatorMcp {
    #[tool(
        name = "android.observe",
        description = "Observe the exact Android device as a typed PNG plus compact UI and a short-lived observation ID. Screenshot fallback is explicit; missing pixels are an error.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn android_observe(
        &self,
        Parameters(args): Parameters<ObserveArgs>,
    ) -> Result<CallToolResult, McpError> {
        reject_install_only_provenance(args.target.as_ref())?;
        let seconds = args.timeout_secs.unwrap_or(10);
        if !(1..=15).contains(&seconds) {
            return Err(invalid("Observation timeout_secs must be between 1 and 15"));
        }
        let target = self
            .resolve_android_execution_target(args.target.as_ref(), args.serial.as_deref())
            .await?;
        let serial = &target.device_serial;
        let (observation, image) = self
            .workflow_observation(serial, Duration::from_secs(seconds))
            .await;
        json_image_result(
            json!({
                "ok": image.is_some(), "outcome": if image.is_some() { "observed" } else { "observe_degraded" },
                "serial": serial, "execution_target": target,
                "observation_id": observation["observation_id"], "observation": observation,
                "retry_safe": true, "do_not_replay": false, "executed_action_count": 0,
                "attempted_action_count": 0, "failed_index": null, "actions": [],
            }),
            image,
        )
    }

    #[tool(
        name = "android.step",
        description = "Execute 1–16 prevalidated Android actions serially on one device, stop on first failure, and return truthful action receipts plus a fresh PNG. Stable action_id forbids replay across timeouts or restart. Coordinate gestures require a fresh observation_id and must be first."
    )]
    pub(crate) async fn android_step(
        &self,
        Parameters(args): Parameters<StepArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.step_with_deadline(args, Instant::now() + BATCH_TIMEOUT)
            .await
    }

    async fn step_with_deadline(
        &self,
        mut args: StepArgs,
        deadline: Instant,
    ) -> Result<CallToolResult, McpError> {
        reject_install_only_provenance(args.target.as_ref())?;
        validate_action_id(&args.action_id)?;
        request_hash(&args)?;
        let target = self
            .resolve_android_execution_target(args.target.as_ref(), args.serial.as_deref())
            .await?;
        let serial = target.device_serial.clone();
        args.serial = Some(serial.clone());
        let mut initial = initial_receipt(
            &args.action_id,
            &serial,
            &self.config.execution_identity.provider_epoch,
        );
        initial["execution_target"] = json!(target);
        let digest = request_hash(&json!({ "operation": "android.step", "request": args }))?;
        let _action_lease = self
            .workflow
            .lock(&format!("action:{}", args.action_id))
            .await;
        let (path, mut record) = match execution::claim(
            &self.config.artifact_dir,
            &args.action_id,
            &digest,
            &serial,
            &self.config.execution_identity.provider_epoch,
            initial,
        )
        .await?
        {
            Claim::New(path, record) => (path, record),
            Claim::Existing(record) => {
                return self.duplicate_workflow_receipt(&serial, record).await;
            }
        };
        let prepared = match prepare_actions(&args, &serial) {
            Ok(actions) => actions,
            Err(error) => {
                record.receipt["outcome"] = json!("rejected");
                record.receipt["retry_safe"] = json!(true);
                record.receipt["do_not_replay"] = json!(false);
                record.receipt["error"] = error_receipt(&error);
                return self.complete_workflow(&serial, &path, record).await;
            }
        };
        let observed_boot_id = match self.verify_coordinate_observation(&args, &serial).await {
            Ok(boot_id) => boot_id,
            Err(error) => {
                record.receipt["outcome"] = json!("rejected");
                record.receipt["retry_safe"] = json!(true);
                record.receipt["do_not_replay"] = json!(false);
                record.receipt["error"] = error_receipt(&error);
                return self.complete_workflow(&serial, &path, record).await;
            }
        };
        let boot_id = match self.workflow_boot_id(&serial).await {
            Ok(boot_id) => boot_id,
            Err(error) => {
                record.receipt["outcome"] = json!("provider_unavailable");
                record.receipt["retry_safe"] = json!(true);
                record.receipt["do_not_replay"] = json!(false);
                record.receipt["error"] = error_receipt(&error);
                return self.complete_workflow(&serial, &path, record).await;
            }
        };
        if observed_boot_id
            .as_ref()
            .is_some_and(|observed| observed != &boot_id)
        {
            record.receipt["outcome"] = json!("rejected");
            record.receipt["retry_safe"] = json!(true);
            record.receipt["do_not_replay"] = json!(false);
            record.receipt["error"] =
                json!("Device restarted after coordinate validation; observe again");
            return self.complete_workflow(&serial, &path, record).await;
        }
        record.receipt["boot_id"] = json!(boot_id);
        self.workflow.invalidate(&serial).await;
        let mut all_verified = true;
        let mut completed = 0usize;
        let mut failed = false;
        for (index, (action, request)) in prepared.into_iter().zip(&args.actions).enumerate() {
            if execution::request_cancelled() {
                stop_before_dispatch(&mut record.receipt, index, "cancelled");
                break;
            }
            if Instant::now() >= deadline {
                stop_before_dispatch(&mut record.receipt, index, "deadline_exceeded");
                break;
            }
            if self.workflow_boot_id(&serial).await.ok().as_deref() != Some(boot_id.as_str()) {
                record.receipt["ok"] = json!(false);
                record.receipt["outcome"] = json!("provider_unavailable");
                record.receipt["failed_index"] = json!(index);
                record.receipt["retry_safe"] = json!(index == 0);
                record.receipt["do_not_replay"] = json!(index > 0);
                record.receipt["error"] = json!(
                    "Device identity changed or became unavailable; remaining actions were not dispatched"
                );
                break;
            }
            record.receipt["attempted_action_count"] = json!(index + 1);
            record.receipt["ok"] = json!(false);
            record.receipt["failed_index"] = json!(index);
            record.receipt["outcome"] = json!("outcome_unknown");
            record.receipt["actions"]
                .as_array_mut()
                .expect("receipt actions")
                .push(json!({
                    "index": index, "tool": request.tool, "ok": false,
                    "outcome": "outcome_unknown", "may_have_been_applied": true,
                }));
            if let Err(error) = execution::persist(&path, &record).await {
                record.receipt["attempted_action_count"] = json!(index);
                record.receipt["actions"]
                    .as_array_mut()
                    .expect("receipt actions")
                    .pop();
                return self.ledger_failure(&serial, record.receipt, &error).await;
            }
            let started = Instant::now();
            if started >= deadline || execution::request_cancelled() {
                record.receipt["attempted_action_count"] = json!(index);
                record.receipt["actions"]
                    .as_array_mut()
                    .expect("receipt actions")
                    .pop();
                stop_before_dispatch(
                    &mut record.receipt,
                    index,
                    if execution::request_cancelled() {
                        "cancelled"
                    } else {
                        "deadline_exceeded"
                    },
                );
                break;
            }
            // Once dispatched, await the primitive's bounded execution and cleanup.
            // Dropping a gesture here could strand a pressed pointer or key.
            let row = match action.dispatch(self).await {
                Ok(result) => match receipt_from_result(&result) {
                    Ok(value)
                        if !result.is_error.unwrap_or(false)
                            && value["ok"].as_bool() == Some(true) =>
                    {
                        let verified = verified_receipt(&value);
                        all_verified &= verified;
                        completed += 1;
                        json!({ "index": index, "tool": request.tool, "ok": true,
                            "outcome": if verified { "verified" } else { "dispatched" },
                            "elapsed_ms": started.elapsed().as_millis(), "receipt": bounded_value(value) })
                    }
                    Ok(value) => {
                        failed = true;
                        json!({ "index": index, "tool": request.tool, "ok": false,
                            "outcome": value.get("outcome").and_then(Value::as_str).unwrap_or("postcondition_failed"),
                            "may_have_been_applied": true, "elapsed_ms": started.elapsed().as_millis(), "receipt": bounded_value(value) })
                    }
                    Err(error) => {
                        failed = true;
                        failed_action(index, &request.tool, &error, started)
                    }
                },
                Err(error) => {
                    failed = true;
                    failed_action(index, &request.tool, &error, started)
                }
            };
            record.receipt["actions"][index] = row;
            record.receipt["executed_action_count"] = json!(completed);
            record.receipt["failed_index"] = if failed { json!(index) } else { Value::Null };
            record.receipt["outcome"] = if failed {
                record.receipt["actions"][index]["outcome"].clone()
            } else {
                json!(if all_verified {
                    "verified"
                } else {
                    "dispatched"
                })
            };
            record.receipt["ok"] = json!(!failed && completed == args.actions.len());
            if failed {
                break;
            }
            if let Err(error) = execution::persist(&path, &record).await {
                return self.ledger_failure(&serial, record.receipt, &error).await;
            }
        }
        self.complete_workflow(&serial, &path, record).await
    }

    #[tool(
        name = "android.install_build_from_run",
        description = "Install an exactly identified hosted build once, preserve phase-aware partial-state receipts and return a fresh PNG. Reusing action_id never repeats the installation."
    )]
    pub(crate) async fn android_install_build_from_run(
        &self,
        Parameters(mut args): Parameters<InstallBuildWorkflowArgs>,
    ) -> Result<CallToolResult, McpError> {
        validate_action_id(&args.action_id)?;
        request_hash(&args)?;
        let target = self
            .resolve_android_execution_target(
                args.build.target.as_ref(),
                args.build.serial.as_deref(),
            )
            .await?;
        let serial = target.device_serial.clone();
        args.build.serial = Some(serial.clone());
        let digest = request_hash(
            &json!({ "operation": "android.install_build_from_run", "request": args }),
        )?;
        let _action_lease = self
            .workflow
            .lock(&format!("action:{}", args.action_id))
            .await;
        let mut initial = initial_receipt(
            &args.action_id,
            &serial,
            &self.config.execution_identity.provider_epoch,
        );
        initial["execution_target"] = json!(target);
        initial["attempted_action_count"] = json!(1);
        initial["failed_index"] = json!(0);
        let (path, mut record) = match execution::claim(
            &self.config.artifact_dir,
            &args.action_id,
            &digest,
            &serial,
            &self.config.execution_identity.provider_epoch,
            initial,
        )
        .await?
        {
            Claim::New(path, record) => (path, record),
            Claim::Existing(record) => {
                return self.duplicate_workflow_receipt(&serial, record).await;
            }
        };
        if execution::request_cancelled() {
            record.receipt["attempted_action_count"] = json!(0);
            stop_before_dispatch(&mut record.receipt, 0, "cancelled");
            return self.complete_workflow(&serial, &path, record).await;
        }
        self.workflow.invalidate(&serial).await;
        // Each provider phase already has a bounded deadline. Keep awaiting an
        // in-flight mutation so it can finish cleanup and preserve known effects;
        // request cancellation is checked before the provider starts a new write.
        match self
            .interactive_session_install_build_from_run_json(&args.build)
            .await
        {
            Ok(value) => {
                let ok = value["ok"].as_bool() == Some(true);
                record.receipt["ok"] = json!(ok);
                record.receipt["outcome"] = json!(
                    value
                        .get("outcome")
                        .and_then(Value::as_str)
                        .unwrap_or(if ok { "verified" } else { "outcome_unknown" })
                );
                record.receipt["executed_action_count"] = json!(usize::from(ok));
                record.receipt["failed_index"] = if ok { Value::Null } else { json!(0) };
                for field in ["retry_safe", "do_not_replay", "mutation_dispatched"] {
                    if let Some(value) = value.get(field) {
                        record.receipt[field] = value.clone();
                    }
                }
                record.receipt["install"] = bounded_value(value);
            }
            Err(error) => {
                if let Some(data) = &error.data {
                    for field in [
                        "outcome",
                        "retry_safe",
                        "do_not_replay",
                        "mutation_dispatched",
                    ] {
                        if let Some(value) = data.get(field) {
                            record.receipt[field] = value.clone();
                        }
                    }
                }
                record.receipt["error"] = error_receipt(&error);
            }
        }
        record.receipt["actions"] = json!([{ "index": 0, "tool": "interactive_session.install_build_from_run", "outcome": record.receipt["outcome"], "ok": record.receipt["ok"] }]);
        self.complete_workflow(&serial, &path, record).await
    }
}

fn stop_before_dispatch(receipt: &mut Value, index: usize, outcome: &str) {
    let attempted = receipt["attempted_action_count"].as_u64().unwrap_or(0);
    receipt["ok"] = json!(false);
    receipt["mutation_outcome"] = if attempted > 0 {
        receipt["outcome"].clone()
    } else {
        json!("not_dispatched")
    };
    receipt["outcome"] = json!(outcome);
    receipt["failed_index"] = json!(index);
    receipt["retry_safe"] = json!(attempted == 0);
    receipt["do_not_replay"] = json!(attempted > 0);
    receipt["remaining_actions_dispatched"] = json!(false);
}

fn failed_action(index: usize, tool: &str, error: &McpError, started: Instant) -> Value {
    let outcome = error
        .data
        .as_ref()
        .and_then(|value| value.get("outcome"))
        .and_then(Value::as_str)
        .unwrap_or("outcome_unknown");
    json!({ "index": index, "tool": tool, "ok": false, "outcome": outcome,
        "may_have_been_applied": error.data.as_ref().and_then(|value| value.get("mutation_dispatched")).and_then(Value::as_bool).unwrap_or(true),
        "elapsed_ms": started.elapsed().as_millis(), "error": error_receipt(error) })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn step(id: &str, actions: Value) -> StepArgs {
        serde_json::from_value(
            json!({ "action_id": id, "serial": "workflow-device", "actions": actions }),
        )
        .unwrap()
    }
    fn home() -> Value {
        json!({ "tool": "android.input.keyevent", "arguments": { "keycode": "KEYCODE_HOME" } })
    }
    #[test]
    fn all_actions_are_prevalidated_before_any_dispatch() {
        let args = step(
            "late-invalid",
            json!([home(), { "tool": "android.input.keyevent", "arguments": {} }]),
        );
        assert!(prepare_actions(&args, "workflow-device").is_err());
        for tool in [
            "android.step",
            "android.install_apk",
            "android.uninstall_app",
            "arbitrary.shell",
        ] {
            assert!(
                prepare_actions(
                    &step("forbidden", json!([{ "tool": tool, "arguments": {} }])),
                    "workflow-device"
                )
                .is_err()
            );
        }
        let typo = step(
            "typo",
            json!([home(), { "tool": "android.input.keyevent", "arguments": { "keycode": "KEYCODE_HOME", "timeout_ms": 100 } }]),
        );
        assert!(prepare_actions(&typo, "workflow-device").is_err());
        let alias = step(
            "alias",
            json!([{ "tool": "android.input.keyevent", "arguments": { "key": "KEYCODE_HOME" } }]),
        );
        assert!(prepare_actions(&alias, "workflow-device").is_ok());
        let args = step(
            "wrong-device",
            json!([{ "tool": "android.input.keyevent", "arguments": { "keycode": "KEYCODE_HOME", "serial": "another-device" } }]),
        );
        assert!(prepare_actions(&args, "workflow-device").is_err());
        let args = step(
            "empty-selector",
            json!([home(), { "tool": "android.tap_element", "arguments": { "selector": {} } }]),
        );
        assert!(prepare_actions(&args, "workflow-device").is_err());
    }
    #[test]
    fn bounded_steps_and_coordinate_order_are_enforced() {
        assert!(prepare_actions(&step("empty", json!([])), "workflow-device").is_err());
        assert!(
            prepare_actions(&step("large", json!(vec![home(); 17])), "workflow-device").is_err()
        );
        let mut args = step(
            "coordinates",
            json!([home(), { "tool": "android.input.tap", "arguments": { "x": 0, "y": 0 } }]),
        );
        args.observation_id = Some("frame".into());
        assert!(prepare_actions(&args, "workflow-device").is_err());
        let args = step(
            "too-much-text",
            json!([{ "tool": "android.input.text", "arguments": { "text": "a".repeat(MAX_REQUEST_BYTES) } }]),
        );
        assert!(prepare_actions(&args, "workflow-device").is_err());
        assert!(validate_action_id("").is_err());
        assert!(validate_action_id(&"a".repeat(129)).is_err());
    }
    #[test]
    fn missing_postconditions_cannot_be_reported_as_verified() {
        assert!(!verified_receipt(
            &json!({ "ok": true, "postcondition": { "satisfied": true, "requested": false } })
        ));
        assert!(verified_receipt(
            &json!({ "ok": true, "postcondition": { "satisfied": true, "requested": true } })
        ));
        assert!(!verified_receipt(&json!({ "ok": true })));
    }
    #[test]
    fn image_failure_is_error_and_images_never_get_structured_content() {
        let failed =
            json_image_result(json!({ "ok": false, "outcome": "observe_degraded" }), None).unwrap();
        assert_eq!(failed.is_error, Some(true));
        assert!(failed.structured_content.is_none());
        let png = STANDARD.decode(PNG).unwrap();
        let result = json_image_result(json!({ "ok": true }), Some(png)).unwrap();
        assert!(result.structured_content.is_none());
        assert_eq!(result.content.len(), 2);
        assert!(image_from_result(&result).is_ok());
        assert!(png_dimensions(b"not a png").is_err());
    }
    #[test]
    fn oversized_diagnostics_preserve_install_effects_and_provenance() {
        let value = bounded_value(json!({
            "ok": false, "installed": true, "install_performed": true,
            "mutation_dispatched": true, "phase": "launch", "outcome": "partially_applied",
            "build": { "sha256": "abc", "repository": "fixture/app", "workflow_run_id": 7 },
            "install_receipt": { "installed": true, "stdout": "a".repeat(32_000) },
            "launch_receipt": { "ok": false, "stderr": "b".repeat(32_000) },
        }));
        assert_eq!(value["installed"], true);
        assert_eq!(value["install_performed"], true);
        assert_eq!(value["mutation_dispatched"], true);
        assert_eq!(value["phase"], "launch");
        assert_eq!(value["build"]["workflow_run_id"], 7);
        assert_eq!(value["install_receipt"]["installed"], true);
        assert_eq!(value["launch_receipt"]["ok"], false);
        assert_eq!(value["diagnostics_truncated"], true);
        assert!(value["install_receipt"]["stdout"].as_str().unwrap().len() < 1100);
        assert!(value["launch_receipt"]["stderr"].as_str().unwrap().len() < 1100);
    }
    #[test]
    fn png_decoder_rejects_truncated_or_corrupt_pixels() {
        let mut bytes = STANDARD.decode(PNG).unwrap();
        assert_eq!(png_dimensions(&bytes).unwrap(), (1, 1));
        assert!(png_dimensions(&bytes[..bytes.len() - 12]).is_err());
        bytes[44] ^= 0x80;
        assert!(png_dimensions(&bytes).is_err());
    }
    #[test]
    fn active_build_provenance_is_not_silently_ignored() {
        let target: AndroidExecutionTarget = serde_json::from_value(json!({
            "expected_build": {
                "repository": "fixture/app", "commit_sha": "a".repeat(40),
                "workflow_run_id": 7, "artifact_name": "fixture-apk",
                "artifact_sha256": "b".repeat(64),
            },
        }))
        .unwrap();
        let error = reject_install_only_provenance(Some(&target)).unwrap_err();
        assert!(error.message.contains("hosted install/relaunch"));
        assert!(reject_install_only_provenance(Some(&AndroidExecutionTarget::default())).is_ok());
    }
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC";
    #[cfg(unix)]
    struct Fixture {
        root: std::path::PathBuf,
    }
    #[cfg(unix)]
    impl Fixture {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "android-mcp-workflow-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("screen.png"), STANDARD.decode(PNG).unwrap()).unwrap();
            std::fs::write(root.join("boot-id"), "11111111-1111-4111-8111-111111111111").unwrap();
            std::fs::write(root.join("ui.xml"), "<hierarchy rotation=\"0\"><node index=\"0\" text=\"Home\" resource-id=\"fixture:id/home\" class=\"android.widget.TextView\" package=\"fixture\" clickable=\"false\" enabled=\"true\" bounds=\"[0,0][1,1]\" /></hierarchy>").unwrap();
            std::fs::write(root.join("adb"), r#"#!/bin/sh
set -eu
fixture=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
if [ "$1" = devices ]; then
  printf 'List of devices attached\nworkflow-device device model:fixture\n'
  exit 0
fi
[ "$1" = -s ] && [ "$2" = workflow-device ] || exit 3
shift 2
command=$(printf '%s' "$*" | tr -d "'")
case "$command" in
  'shell cat /proc/sys/kernel/random/boot_id') cat "$fixture/boot-id" ;;
  'exec-out screencap -p') [ ! -f "$fixture/fail-screenshot" ] || exit 1; cat "$fixture/screen.png" ;;
  'exec-out uiautomator dump /dev/tty') cat "$fixture/ui.xml" ;;
  'shell input keyevent '*|'shell input tap '*)
    printf '%s\n' "$command" >> "$fixture/events"
    if [ -f "$fixture/restart-after-input" ]; then
      printf '22222222-2222-4222-8222-222222222222' > "$fixture/boot-id"
    fi
    if [ -f "$fixture/pause-after-input" ]; then sleep 2; fi
    if [ "$command" = 'shell input keyevent KEYCODE_BACK' ] && [ -f "$fixture/fail-back" ]; then
      echo 'fixture command acknowledgement lost' >&2
      exit 1
    fi
    ;;
  'shell dumpsys input')
    rotation=0
    if [ -f "$fixture/rotation" ]; then rotation=$(cat "$fixture/rotation"); fi
    printf 'Viewport INTERNAL displayId=0, orientation=%s, valid=true\n' "$rotation"
    ;;
  'shell wm size') printf 'Physical size: 1x1\n' ;;
  'shell dumpsys window windows'|'shell dumpsys window')
    if [ -f "$fixture/changed-window" ]; then printf 'mCurrentFocus=Window{b u0 other/.Main}\n'
    else printf 'mCurrentFocus=Window{a u0 fixture/.Main}\n'; fi
    ;;
  'shell dumpsys activity activities') printf 'mResumedActivity: ActivityRecord{a u0 fixture/.Main}\n' ;;
  *) : ;;
esac
"#).unwrap();
            std::fs::set_permissions(root.join("adb"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            Self { root }
        }
        fn marker(&self, name: &str) {
            std::fs::write(self.root.join(name), "").unwrap();
        }
        fn screen(&self, width: u32, height: u32, pixel: [u8; 3]) {
            let file = std::fs::File::create(self.root.join("screen.png")).unwrap();
            let mut encoder = png::Encoder::new(file, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&pixel.repeat((width * height) as usize))
                .unwrap();
            writer.finish().unwrap();
        }
        fn events(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("events"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
        fn server(&self, epoch: &str) -> AndroidEmulatorMcp {
            use crate::config::{Config, ProviderExecutionIdentity, StreamableHttpConfig};
            AndroidEmulatorMcp::new(Config {
                sdk_root: self.root.clone(),
                adb_path: self.root.join("adb"),
                emulator_path: self.root.join("adb"),
                avdmanager_path: self.root.join("adb"),
                artifact_dir: self.root.join("artifacts"),
                emulator_grpc_port: None,
                use_sg_kvm: false,
                streamable_http: StreamableHttpConfig {
                    bind_addr: "127.0.0.1:9526".parse().unwrap(),
                    allowed_hosts: vec!["localhost".into()],
                    max_sessions: 4,
                    channel_capacity: 8,
                    allow_resume: false,
                },
                interactive_session: None,
                execution_identity: ProviderExecutionIdentity {
                    environment_id: "fixture".into(),
                    provider_instance_id: "fixture".into(),
                    provider_epoch: epoch.into(),
                    session_id: "fixture".into(),
                },
            })
        }
    }
    #[cfg(unix)]
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn successful_batch_is_dispatched_once_across_duplicate_and_restart() {
        let fixture = Fixture::new();
        let server = fixture.server("epoch-a");
        let result = server
            .android_step(Parameters(step("once", json!([home()]))))
            .await
            .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["ok"], true);
        assert_eq!(receipt["outcome"], "dispatched");
        assert_eq!(receipt["executed_action_count"], 1);
        assert!(image_from_result(&result).is_ok());
        let restarted = fixture.server("epoch-b");
        let duplicate = restarted
            .android_step(Parameters(step("once", json!([home()]))))
            .await
            .unwrap();
        let receipt = receipt_from_result(&duplicate).unwrap();
        assert_eq!(receipt["duplicate_action_id"], true);
        assert_eq!(receipt["executed_action_count"], 1);
        assert_eq!(fixture.events().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failure_after_mutation_stops_the_batch_and_preserves_partial_receipts() {
        let fixture = Fixture::new();
        fixture.marker("fail-back");
        let result = fixture.server("epoch-a").android_step(Parameters(step("partial", json!([
            home(), { "tool": "android.input.keyevent", "arguments": { "keycode": "KEYCODE_BACK" } }, home(),
        ])))).await.unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["ok"], false);
        assert_eq!(receipt["executed_action_count"], 1);
        assert_eq!(receipt["attempted_action_count"], 2);
        assert_eq!(receipt["failed_index"], 1);
        assert_eq!(receipt["do_not_replay"], true);
        assert_eq!(receipt["retry_safe"], false);
        assert_eq!(fixture.events().len(), 2);
        assert!(image_from_result(&result).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn malformed_later_action_prevents_the_first_mutation() {
        let fixture = Fixture::new();
        let result = fixture
            .server("epoch-a")
            .android_step(Parameters(step(
                "bad-later",
                json!([
                    home(), { "tool": "android.input.keyevent", "arguments": {} },
                ]),
            )))
            .await
            .unwrap();
        assert_eq!(receipt_from_result(&result).unwrap()["outcome"], "rejected");
        assert!(fixture.events().is_empty());
        assert!(image_from_result(&result).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn malformed_reuse_of_an_existing_id_cannot_bypass_ledger_mismatch() {
        let fixture = Fixture::new();
        let server = fixture.server("epoch-a");
        server
            .android_step(Parameters(step("claimed", json!([home()]))))
            .await
            .unwrap();
        let result = server
            .android_step(Parameters(step(
                "claimed",
                json!([{
                    "tool": "android.step", "arguments": {},
                }]),
            )))
            .await;
        assert!(result.is_err());
        assert_eq!(fixture.events().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_post_action_image_does_not_erase_durable_mutation_success() {
        let fixture = Fixture::new();
        fixture.marker("fail-screenshot");
        let server = fixture.server("epoch-a");
        let result = server
            .android_step(Parameters(step("no-image", json!([home()]))))
            .await
            .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(receipt["outcome"], "observe_degraded");
        assert_eq!(receipt["mutation_outcome"], "dispatched");
        assert_eq!(receipt["executed_action_count"], 1);
        assert_eq!(receipt["do_not_replay"], true);
        std::fs::remove_file(fixture.root.join("fail-screenshot")).unwrap();
        let retry = server
            .android_step(Parameters(step("no-image", json!([home()]))))
            .await
            .unwrap();
        assert_eq!(receipt_from_result(&retry).unwrap()["ok"], true);
        assert_eq!(fixture.events().len(), 1);
        assert!(image_from_result(&retry).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn device_reboot_stops_even_semantic_or_key_batches() {
        let fixture = Fixture::new();
        fixture.marker("restart-after-input");
        let result = fixture
            .server("epoch-a")
            .android_step(Parameters(step("restart", json!([home(), home()]))))
            .await
            .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["outcome"], "provider_unavailable");
        assert_eq!(receipt["executed_action_count"], 1);
        assert_eq!(receipt["attempted_action_count"], 1);
        assert_eq!(receipt["failed_index"], 1);
        assert_eq!(receipt["do_not_replay"], true);
        assert_eq!(fixture.events().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_coordinate_observation_is_refused_before_input() {
        let fixture = Fixture::new();
        let server = fixture.server("epoch-a");
        let result = server
            .android_observe(Parameters(ObserveArgs {
                serial: Some("workflow-device".into()),
                target: None,
                timeout_secs: None,
            }))
            .await
            .unwrap();
        let id = receipt_from_result(&result).unwrap()["observation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        server
            .workflow
            .observations
            .lock()
            .await
            .get_mut("workflow-device")
            .unwrap()
            .captured = Instant::now() - Duration::from_secs(31);
        let mut args = step(
            "stale",
            json!([{ "tool": "android.input.tap", "arguments": { "x": 0, "y": 0 } }]),
        );
        args.observation_id = Some(id);
        let result = server.android_step(Parameters(args)).await.unwrap();
        assert_eq!(receipt_from_result(&result).unwrap()["outcome"], "rejected");
        assert!(fixture.events().is_empty());
    }

    #[cfg(unix)]
    async fn observed_tap(server: &AndroidEmulatorMcp, action_id: &str) -> StepArgs {
        let result = server
            .android_observe(Parameters(ObserveArgs {
                serial: Some("workflow-device".into()),
                target: None,
                timeout_secs: None,
            }))
            .await
            .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["observation"]["coordinate_actions_available"], true);
        let mut args = step(
            action_id,
            json!([{ "tool": "android.input.tap", "arguments": { "x": 0, "y": 0 } }]),
        );
        args.observation_id = receipt["observation_id"].as_str().map(str::to_owned);
        args
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn animation_with_unchanged_geometry_and_window_allows_coordinates() {
        let fixture = Fixture::new();
        let server = fixture.server("epoch-a");
        let args = observed_tap(&server, "animation").await;
        fixture.screen(1, 1, [255, 0, 0]);
        let result = server.android_step(Parameters(args)).await.unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["ok"], true, "{receipt}");
        assert_eq!(receipt["executed_action_count"], 1);
        assert_eq!(fixture.events().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn strict_pixels_or_changed_geometry_rotation_and_window_refuse_coordinates() {
        for kind in ["strict", "geometry", "rotation", "window"] {
            let fixture = Fixture::new();
            let server = fixture.server("epoch-a");
            let mut args = observed_tap(&server, kind).await;
            match kind {
                "strict" => {
                    args.strict_frame_match = true;
                    fixture.screen(1, 1, [255, 0, 0]);
                }
                "geometry" => fixture.screen(2, 1, [0, 0, 0]),
                "rotation" => std::fs::write(fixture.root.join("rotation"), "2").unwrap(),
                "window" => fixture.marker("changed-window"),
                _ => unreachable!(),
            }
            let result = server.android_step(Parameters(args)).await.unwrap();
            let receipt = receipt_from_result(&result).unwrap();
            assert_eq!(receipt["outcome"], "rejected", "{kind}: {receipt}");
            assert!(fixture.events().is_empty(), "{kind}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_duplicate_requests_share_one_dispatch() {
        let fixture = Fixture::new();
        let server = fixture.server("epoch-a");
        let (first, second) = tokio::join!(
            server.android_step(Parameters(step("concurrent", json!([home()])))),
            server.android_step(Parameters(step("concurrent", json!([home()])))),
        );
        let first = receipt_from_result(&first.unwrap()).unwrap();
        let second = receipt_from_result(&second.unwrap()).unwrap();
        assert_ne!(first["duplicate_action_id"], second["duplicate_action_id"]);
        assert_eq!(fixture.events().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_mcp_cancellation_finishes_current_action_but_stops_the_rest() {
        let fixture = Fixture::new();
        fixture.marker("pause-after-input");
        let server = fixture.server("epoch-a");
        let token = tokio_util::sync::CancellationToken::new();
        let request_token = token.clone();
        let task = tokio::spawn(async move {
            execution::with_cancellation(
                request_token,
                server.android_step(Parameters(step("token-cancelled", json!([home(), home()])))),
            )
            .await
        });
        for _ in 0..200 {
            if !fixture.events().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(fixture.events().len(), 1, "fixture did not reach dispatch");
        token.cancel();
        let result = task.await.unwrap().unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["outcome"], "cancelled", "{receipt}");
        assert_eq!(receipt["executed_action_count"], 1);
        assert_eq!(receipt["attempted_action_count"], 1);
        assert_eq!(receipt["failed_index"], 1);
        assert_eq!(receipt["do_not_replay"], true);
        assert_eq!(receipt["actions"][0]["outcome"], "dispatched");
        assert_eq!(fixture.events().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exhausted_batch_budget_stops_before_any_dispatch() {
        let fixture = Fixture::new();
        let result = fixture
            .server("epoch-a")
            .step_with_deadline(step("no-budget", json!([home()])), Instant::now())
            .await
            .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["outcome"], "deadline_exceeded");
        assert_eq!(receipt["attempted_action_count"], 0);
        assert_eq!(receipt["retry_safe"], true);
        assert!(fixture.events().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn batch_deadline_awaits_current_action_cleanup_and_stops_later_actions() {
        let fixture = Fixture::new();
        fixture.marker("pause-after-input");
        let result = fixture
            .server("epoch-a")
            .step_with_deadline(
                step("finish-current", json!([home(), home()])),
                Instant::now() + Duration::from_millis(500),
            )
            .await
            .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["outcome"], "deadline_exceeded", "{receipt}");
        assert_eq!(receipt["executed_action_count"], 1);
        assert_eq!(receipt["attempted_action_count"], 1);
        assert_eq!(receipt["actions"][0]["outcome"], "dispatched");
        assert_eq!(fixture.events().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_install_never_invokes_the_provider() {
        let fixture = Fixture::new();
        let server = fixture.server("epoch-a");
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let result = execution::with_cancellation(
            token,
            server.android_install_build_from_run(Parameters(
                serde_json::from_value(json!({
                    "action_id": "cancelled-install",
                    "build": { "workflow_run_id": 7, "artifact_name": "fixture-apk", "serial": "workflow-device" },
                }))
                .unwrap(),
            )),
        )
        .await
        .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["outcome"], "cancelled", "{receipt}");
        assert_eq!(receipt["attempted_action_count"], 0);
        assert_eq!(receipt["retry_safe"], true);
        assert!(fixture.events().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_dispatch_remains_unknown_and_is_never_replayed() {
        let fixture = Fixture::new();
        fixture.marker("pause-after-input");
        let server = fixture.server("epoch-a");
        let task = tokio::spawn(async move {
            server
                .android_step(Parameters(step("cancelled", json!([home()]))))
                .await
        });
        for _ in 0..200 {
            if !fixture.events().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(fixture.events().len(), 1, "fixture did not reach dispatch");
        task.abort();
        let _ = task.await;
        let result = fixture
            .server("epoch-b")
            .android_step(Parameters(step("cancelled", json!([home()]))))
            .await
            .unwrap();
        let receipt = receipt_from_result(&result).unwrap();
        assert_eq!(receipt["outcome"], "outcome_unknown");
        assert_eq!(receipt["duplicate_action_id"], true);
        assert_eq!(receipt["executed_action_count"], 0);
        assert_eq!(receipt["attempted_action_count"], 1);
        assert_eq!(receipt["do_not_replay"], true);
        assert_eq!(fixture.events().len(), 1);
    }
}
