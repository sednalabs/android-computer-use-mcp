//! Tool implementations for the local Android emulator harness.
//!
//! ## Rationale
//! Defines the core tool surface for MCP-based Android automation, handling
//! the orchestration between external tool requests and internal Android
//! system interactions (ADB/gRPC).
//!
//! ## Security Boundaries
//! * Shell execution is restricted to configured SDK binary paths.
//! * Artifact outputs are confined to the defined project artifacts directory.
//! * Input selector parameters are normalized to prevent injection.
//!
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::fs;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

use crate::McpError;
use crate::discovery;
use crate::grpc_backend;
use crate::interactive_session::{
    InteractiveSessionInstallBuildArgs, InteractiveSessionRelaunchArgs,
};
use crate::server::AndroidEmulatorMcp;
use crate::ui::{
    NormalizedUiNode, SelectionFailure, SelectorCandidateSummary, UiNodeMatch, UiSelector,
    UiSelectorInput, actionable_center, ensure_selector_not_empty, find_interactive_ui_node,
    find_ui_node_by_label, matches_text, matching_nodes, normalize_optional_selector_input,
    normalize_selector_input, parse_ui_nodes_from_path, parse_ui_nodes_from_xml,
    resolve_node_selection, selection_failure_json, selector_candidate_summary,
    text_verification_target_selector,
};
use crate::verification::{
    InternalNodeTracker, TapVerification, TapVerificationRequest, TextVerification,
    TextVerificationRequest, ToolPostconditionRequest, ToolPostconditionResult,
    VerifiedTextDispatchRequest, ensure_action_outcome_satisfied,
    ensure_tool_postcondition_satisfied, tap_verification_fingerprint, tap_verification_json,
    tap_verification_summary, text_verification_json, text_verification_summary,
    tool_postcondition_json, tracker_matches_node,
};
#[cfg(test)]
use crate::verification::{
    TapVerificationStatus, derive_observed_package, tap_verification_is_confirmed,
    tap_verification_status, text_verification_fingerprint, text_verification_status,
};
use crate::window_views::{
    normalized_visible_window_dump_fingerprint,
    normalized_visible_window_dump_fingerprint_for_package,
};

const DEFAULT_BOOT_TIMEOUT_SECS: u64 = 180;
const DEFAULT_SWIPE_DURATION_MS: u64 = 250;
const DEFAULT_SOLARLAB_ACK_TIMEOUT_SECS: u64 = 8;
const DEFAULT_SOLARLAB_ACK_POLL_MS: u64 = 400;
const DEFAULT_ADB_COMMAND_TIMEOUT_SECS: u64 = 20;
const DEFAULT_EMULATOR_COMMAND_TIMEOUT_SECS: u64 = 20;
pub(crate) const DEFAULT_ACTION_TIMEOUT_SECS: u64 = 5;
const MIN_OBSERVATION_BUDGET_MS: u64 = 500;
const MIN_FAST_UI_FINGERPRINT_BUDGET_MS: u64 = 1000;
const GRPC_TEXT_VERIFICATION_BUDGET_MS: u64 = 1500;
const MIN_TEXT_VERIFICATION_WINDOW_MS: u64 = 4500;
const ADB_TEXT_FALLBACK_RESERVE_MS: u64 = 5000;
const FULL_OBSERVATION_CAPTURE_RESERVE_MS: u64 = 4500;
const UI_DUMP_PULL_MAX_ATTEMPTS: usize = 3;
const UI_DUMP_PULL_RETRY_DELAY_MS: u64 = 100;
static ARTIFACT_NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct EmptyArgs {}

/// Arguments for launching an Android Virtual Device (AVD).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchAvdArgs {
    /// Name of the AVD to launch.
    pub avd_name: String,
    /// Launch the emulator without a UI window.
    #[serde(default)]
    pub no_window: bool,
    /// Hardware acceleration/GPU configuration.
    #[serde(default)]
    pub gpu: Option<String>,
    /// Optional port for the emulator's gRPC control service.
    #[serde(default)]
    pub grpc_port: Option<u16>,
    /// Additional arguments passed directly to the emulator binary.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

/// Arguments for launching an AVD and awaiting successful boot.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchAvdAndWaitArgs {
    /// Name of the AVD to launch.
    pub avd_name: String,
    /// Launch the emulator without a UI window.
    #[serde(default)]
    pub no_window: bool,
    /// Hardware acceleration/GPU configuration.
    #[serde(default)]
    pub gpu: Option<String>,
    /// Optional port for the emulator's gRPC control service.
    #[serde(default)]
    pub grpc_port: Option<u16>,
    /// Additional arguments passed directly to the emulator binary.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Expected ADB serial for the booted emulator.
    #[serde(default)]
    pub expected_serial: Option<String>,
    /// Timeout in seconds to wait for boot completion.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SerialArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
}

/// Arguments for waiting until an emulator reports boot completion.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitForBootArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Timeout in seconds to wait for boot completion.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Arguments for installing an APK onto a device.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstallApkArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Absolute or relative path to the APK file on the host.
    pub apk_path: String,
    /// Whether to replace an existing installation in place.
    #[serde(default)]
    pub reinstall: bool,
}

/// Arguments for launching an installed Android app and optionally waiting for readiness.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchAppArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Android package name to launch.
    pub package_name: String,
    /// Optional fully qualified activity name to launch directly.
    #[serde(default)]
    pub activity: Option<String>,
    /// Optional selector to verify after launch.
    #[serde(default)]
    pub wait_for_selector: Option<UiSelectorInput>,
    /// Optional activity name expected after launch.
    #[serde(default)]
    pub wait_for_activity: Option<String>,
    /// Optional package name expected after launch.
    #[serde(default)]
    pub wait_for_package: Option<String>,
    /// Timeout in seconds for launch and post-launch verification.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Arguments for capturing a device screenshot artifact.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Optional path to save the screenshot artifact.
    #[serde(default)]
    pub filename: Option<String>,
}

/// Arguments for dumping the current UI hierarchy as an artifact.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UiDumpArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Optional path to save the dumped hierarchy.
    #[serde(default)]
    pub filename: Option<String>,
}

/// Arguments for inspecting the current UI hierarchy and screenshot.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InspectUiArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Path to save the extracted UI hierarchy.
    #[serde(default)]
    pub hierarchy_filename: Option<String>,
    /// Whether to include a screenshot of the current state.
    #[serde(default = "default_true")]
    pub include_screenshot: bool,
    /// Path to save the screenshot.
    #[serde(default)]
    pub screenshot_filename: Option<String>,
}

/// Arguments for waiting until the UI state has stabilized.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitForStableUiArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Timeout in seconds to wait for stabilization.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Interval in milliseconds to poll for stability.
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
    /// Number of consecutive stable polls required.
    #[serde(default)]
    pub stable_polls: Option<u32>,
    /// Optional path to save the hierarchy if inspection is desired.
    #[serde(default)]
    pub hierarchy_filename: Option<String>,
    /// Whether to include a screenshot.
    #[serde(default = "default_true")]
    pub include_screenshot: bool,
    /// Path to save the screenshot.
    #[serde(default)]
    pub screenshot_filename: Option<String>,
}

/// Arguments for finding a UI element based on a selector.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindUiElementArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// The selector defining the UI element to locate.
    pub selector: UiSelectorInput,
    /// Optional path to save the hierarchy.
    #[serde(default)]
    pub hierarchy_filename: Option<String>,
    /// The index to match if multiple elements are found.
    #[serde(default)]
    pub match_index: Option<usize>,
}

/// Arguments for tapping on a UI element.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TapElementArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// The selector defining the element to tap.
    pub selector: UiSelectorInput,
    /// Optional path to save the hierarchy.
    #[serde(default)]
    pub hierarchy_filename: Option<String>,
    /// Whether to wait for the element to disappear after the tap.
    #[serde(default)]
    pub wait_until_absent: bool,
    /// Optional selector to verify success after the tap.
    #[serde(default)]
    pub wait_for_selector: Option<UiSelectorInput>,
    /// Timeout in seconds for the operation.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Whether to retry the operation using ADB if UI does not change.
    #[serde(default = "default_true")]
    pub retry_with_adb_on_no_change: bool,
    /// Whether to allow the tool to succeed even if verification fails.
    #[serde(default)]
    pub allow_verification_failure: bool,
    /// The index to match if multiple elements are found.
    #[serde(default)]
    pub match_index: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TypeIntoElementArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Selector for the element that should receive the text.
    pub selector: UiSelectorInput,
    /// Text value that should be set into the target field.
    pub text: String,
    /// Optional path to save the hierarchy captured during verification.
    #[serde(default)]
    pub hierarchy_filename: Option<String>,
    /// Timeout in seconds for focus, dispatch, and text verification.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// The index to match if multiple elements satisfy the selector.
    #[serde(default)]
    pub match_index: Option<usize>,
}

fn default_true() -> bool {
    true
}

/// Arguments for scrolling until a matching element becomes visible.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScrollUntilVisibleArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Selector for the element that should become visible.
    pub selector: UiSelectorInput,
    /// Scroll direction such as `up`, `down`, `left`, or `right`.
    #[serde(default)]
    pub direction: Option<String>,
    /// Maximum number of swipe attempts before giving up.
    #[serde(default)]
    pub max_swipes: Option<u32>,
    /// Optional path to save the final hierarchy snapshot.
    #[serde(default)]
    pub hierarchy_filename: Option<String>,
    /// The index to match if multiple elements satisfy the selector.
    #[serde(default)]
    pub match_index: Option<usize>,
}

/// Arguments for collecting recent logcat output as an artifact.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogcatArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Optional path to save the collected logcat output.
    #[serde(default)]
    pub filename: Option<String>,
    /// Number of recent log lines to capture.
    #[serde(default)]
    pub lines: Option<u32>,
}

/// Arguments for tapping an absolute screen coordinate.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TapArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Horizontal coordinate in device pixels.
    pub x: u32,
    /// Vertical coordinate in device pixels.
    pub y: u32,
    /// Optional selector describing the tapped element for verification.
    #[serde(default)]
    pub tapped_selector: Option<UiSelectorInput>,
    /// Whether to wait for the tapped selector to disappear.
    #[serde(default)]
    pub wait_until_absent: bool,
    /// Optional selector to verify after the tap.
    #[serde(default)]
    pub wait_for_selector: Option<UiSelectorInput>,
    /// Timeout in seconds for tap dispatch and verification.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Whether to retry with ADB if the first backend reports no UI change.
    #[serde(default = "default_true")]
    pub retry_with_adb_on_no_change: bool,
}

/// Arguments for sending text input to the currently focused element.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TextArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Text to dispatch to the focused field.
    pub text: String,
    /// Optional selector that must be focused before dispatching text.
    #[serde(default)]
    pub expect_focus_selector: Option<UiSelectorInput>,
    /// Optional selector to verify after text dispatch.
    #[serde(default)]
    pub wait_for_selector: Option<UiSelectorInput>,
    /// Timeout in seconds for dispatch and verification.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Arguments for waiting for a UI element to appear or disappear.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitUiElementArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// The selector defining the UI element to wait for.
    pub selector: UiSelectorInput,
    /// Optional path to save the hierarchy.
    #[serde(default)]
    pub hierarchy_filename: Option<String>,
    /// The index to match if multiple elements are found.
    #[serde(default)]
    pub match_index: Option<usize>,
    /// Wait for the selector to be absent instead of present.
    #[serde(default)]
    pub absent: bool,
    /// Timeout in seconds for the operation.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Arguments for swiping between two screen coordinates.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SwipeArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Starting horizontal coordinate in device pixels.
    pub x1: u32,
    /// Starting vertical coordinate in device pixels.
    pub y1: u32,
    /// Ending horizontal coordinate in device pixels.
    pub x2: u32,
    /// Ending vertical coordinate in device pixels.
    pub y2: u32,
    /// Optional swipe duration in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Optional selector to verify after the swipe.
    #[serde(default)]
    pub wait_for_selector: Option<UiSelectorInput>,
    /// Whether the caller expects the swipe to change scroll position.
    #[serde(default)]
    pub expect_scroll_change: bool,
    /// Timeout in seconds for swipe dispatch and verification.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Arguments for sending a key event to the device.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KeyeventArgs {
    /// ADB serial of the target device.
    #[serde(default)]
    pub serial: Option<String>,
    /// Android keycode name to dispatch, such as `KEYCODE_ENTER`.
    pub keycode: String,
    /// Optional selector to verify after the key event.
    #[serde(default)]
    pub wait_for_selector: Option<UiSelectorInput>,
    /// Optional activity name expected after the key event.
    #[serde(default)]
    pub wait_for_activity: Option<String>,
    /// Optional package name expected after the key event.
    #[serde(default)]
    pub wait_for_package: Option<String>,
    /// Timeout in seconds for dispatch and postcondition verification.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SolarLabScenarioArgs {
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub package_name: Option<String>,
    #[serde(default)]
    pub activity: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SolarLabSemanticActionArgs {
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub package_name: Option<String>,
    #[serde(default)]
    pub activity: Option<String>,
    pub action: String,
    #[serde(default)]
    pub body_query: Option<String>,
    #[serde(default = "default_true")]
    pub capture_state: bool,
}

#[derive(Debug, Clone)]
enum SolarLabSemanticCommand {
    FocusBody { body_query: String },
    ResetCamera,
    OpenImmersive,
    ReturnToSandbox,
}

#[derive(Debug, Serialize)]
struct SolarLabSemanticAck {
    acknowledged: bool,
    matcher: String,
    observed_ui_dump: String,
}

#[derive(Debug, Clone)]
struct SolarLabSemanticAckMatcher {
    description: String,
}

impl SolarLabSemanticCommand {
    fn action_name(&self) -> &'static str {
        match self {
            Self::FocusBody { .. } => "focus_body",
            Self::ResetCamera => "reset_camera",
            Self::OpenImmersive => "open_immersive",
            Self::ReturnToSandbox => "return_to_sandbox",
        }
    }

    fn body_query(&self) -> Option<&str> {
        match self {
            Self::FocusBody { body_query } => Some(body_query.as_str()),
            Self::ResetCamera | Self::OpenImmersive | Self::ReturnToSandbox => None,
        }
    }
}

#[tool_router(router = tool_router_android, vis = "pub")]
impl AndroidEmulatorMcp {
    #[tool(
        name = "android.health",
        description = "Report SDK paths, known AVDs, and attached Android devices.",
        annotations(read_only_hint = true)
    )]
    async fn android_health(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let avds = self.list_avds_internal().await?;
        let devices = self.list_devices_internal(None).await?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "sdk_root": self.config.sdk_root,
            "adb_path": self.config.adb_path,
            "emulator_path": self.config.emulator_path,
            "avdmanager_path": self.config.avdmanager_path,
            "artifact_dir": self.config.artifact_dir,
            "emulator_grpc_port": self.config.emulator_grpc_port,
            "io_backend_preference": if self.config.emulator_grpc_port.is_some() {
                "grpc-preferred-with-adb-fallback"
            } else if discovery::any_grpc_endpoint_published() {
                "grpc-autodiscovery-with-adb-fallback"
            } else {
                "adb"
            },
            "use_sg_kvm": self.config.use_sg_kvm,
            "avds": avds,
            "devices": devices,
        })))
    }

    #[tool(
        name = "android.list_avds",
        description = "List known local Android virtual devices.",
        annotations(read_only_hint = true)
    )]
    async fn android_list_avds(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "avds": self.list_avds_internal().await?,
        })))
    }

    #[tool(
        name = "android.list_devices",
        description = "List adb-visible Android devices.",
        annotations(read_only_hint = true)
    )]
    async fn android_list_devices(
        &self,
        Parameters(args): Parameters<SerialArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "devices": self.list_devices_internal(args.serial.as_deref()).await?,
        })))
    }

    #[tool(
        name = "interactive_session.get_status",
        description = "Report hosted interactive-session configuration, live session root, and active build metadata.",
        annotations(read_only_hint = true)
    )]
    async fn interactive_session_get_status(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(
            self.interactive_session_status_json().await?,
        ))
    }

    #[tool(
        name = "interactive_session.get_current_build",
        description = "Read the current active-build metadata for the hosted interactive session.",
        annotations(read_only_hint = true)
    )]
    async fn interactive_session_get_current_build(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(
            self.interactive_session_current_build_json().await?,
        ))
    }

    #[tool(
        name = "interactive_session.install_build_from_run",
        description = "Download a reusable interactive build artifact from a GitHub Actions run, install it into the live session, and relaunch the app."
    )]
    async fn interactive_session_install_build_from_run(
        &self,
        Parameters(args): Parameters<InteractiveSessionInstallBuildArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(
            self.interactive_session_install_build_from_run_json(&args)
                .await?,
        ))
    }

    #[tool(
        name = "interactive_session.relaunch_current_build",
        description = "Relaunch the current active build inside the live hosted interactive session."
    )]
    async fn interactive_session_relaunch_current_build(
        &self,
        Parameters(args): Parameters<InteractiveSessionRelaunchArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(
            self.interactive_session_relaunch_current_build_json(&args)
                .await?,
        ))
    }

    #[tool(
        name = "android.launch_avd",
        description = "Launch a named AVD in the background."
    )]
    async fn android_launch_avd(
        &self,
        Parameters(args): Parameters<LaunchAvdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let launched = self.spawn_avd_process(LaunchRequest::from(&args)).await?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "pid": launched.pid,
            "avd_name": launched.avd_name,
            "grpc_port": launched.grpc_port,
            "log_path": launched.log_path,
            "args": launched.args,
        })))
    }

    #[tool(
        name = "android.launch_avd_and_wait",
        description = "Launch a named AVD, wait for its emulator serial to appear, and verify boot readiness."
    )]
    async fn android_launch_avd_and_wait(
        &self,
        Parameters(args): Parameters<LaunchAvdAndWaitArgs>,
    ) -> Result<CallToolResult, McpError> {
        let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(DEFAULT_BOOT_TIMEOUT_SECS));
        let pre_launch_devices = self.list_devices_internal(None).await?;
        let started = std::time::Instant::now();
        let launched = self.spawn_avd_process(LaunchRequest::from(&args)).await?;
        let serial = self
            .wait_for_new_emulator_serial(
                &ready_emulator_serials(&pre_launch_devices),
                args.expected_serial.as_deref(),
                timeout,
            )
            .await?;
        let remaining = timeout.checked_sub(started.elapsed()).ok_or_else(|| {
            McpError::internal_error(
                format!(
                    "AVD {} did not expose a bootable emulator within {:?}",
                    launched.avd_name, timeout
                ),
                None,
            )
        })?;
        let readiness = self.wait_for_boot_readiness(&serial, remaining).await?;

        Ok(CallToolResult::structured(json!({
            "ok": true,
            "pid": launched.pid,
            "avd_name": launched.avd_name,
            "serial": serial,
            "grpc_port": launched.grpc_port,
            "log_path": launched.log_path,
            "args": launched.args,
            "boot_elapsed_ms": readiness.elapsed_ms,
            "total_elapsed_ms": started.elapsed().as_millis(),
            "package_manager": readiness.package_manager,
        })))
    }

    #[tool(
        name = "android.wait_for_boot",
        description = "Wait until sys.boot_completed=1 and package manager is responsive.",
        annotations(read_only_hint = true)
    )]
    async fn android_wait_for_boot(
        &self,
        Parameters(args): Parameters<WaitForBootArgs>,
    ) -> Result<CallToolResult, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(DEFAULT_BOOT_TIMEOUT_SECS));
        let readiness = self.wait_for_boot_readiness(&serial, timeout).await?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "serial": serial,
            "elapsed_ms": readiness.elapsed_ms,
            "package_manager": readiness.package_manager,
        })))
    }

    #[tool(
        name = "android.install_apk",
        description = "Install an APK onto the target device."
    )]
    async fn android_install_apk(
        &self,
        Parameters(args): Parameters<InstallApkArgs>,
    ) -> Result<CallToolResult, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let apk_path = PathBuf::from(args.apk_path.trim());
        if !apk_path.is_file() {
            return Err(McpError::invalid_params(
                format!("apk_path is not a file: {}", apk_path.display()),
                None,
            ));
        }
        let mut adb_args = vec!["install".to_string()];
        if args.reinstall {
            adb_args.push("-r".to_string());
        }
        adb_args.push(apk_path.display().to_string());
        let output = self.run_adb(&serial, adb_args).await?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "serial": serial,
            "apk_path": apk_path,
            "stdout": output.stdout,
            "stderr": output.stderr,
        })))
    }

    #[tool(
        name = "android.launch_app",
        description = "Launch an installed Android app by package name and optional activity."
    )]
    async fn android_launch_app(
        &self,
        Parameters(args): Parameters<LaunchAppArgs>,
    ) -> Result<CallToolResult, McpError> {
        let wait_for_selector = normalize_optional_selector_input(args.wait_for_selector);
        if let Some(selector) = wait_for_selector.as_ref() {
            ensure_selector_not_empty(selector)?;
        }
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        if args.package_name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "package_name must not be empty",
                None,
            ));
        }
        let output = if let Some(activity) = args.activity.as_deref() {
            self.run_adb_shell(
                &serial,
                [
                    "am",
                    "start",
                    "-n",
                    &format!("{}/{}", args.package_name.trim(), activity.trim()),
                ],
            )
            .await?
        } else {
            self.run_adb_shell(
                &serial,
                [
                    "monkey",
                    "-p",
                    args.package_name.trim(),
                    "-c",
                    "android.intent.category.LAUNCHER",
                    "1",
                ],
            )
            .await?
        };
        let deadline = tool_deadline(args.timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
        let postcondition = self
            .wait_for_tool_postcondition(ToolPostconditionRequest {
                serial: &serial,
                selector: wait_for_selector.as_ref(),
                match_index: None,
                wait_for_activity: args
                    .wait_for_activity
                    .as_deref()
                    .or(args.activity.as_deref()),
                wait_for_package: args
                    .wait_for_package
                    .as_deref()
                    .or(Some(args.package_name.as_str())),
                deadline,
                include_screenshot: false,
                artifact_prefix: "launch-app-postcondition",
            })
            .await?;
        ensure_tool_postcondition_satisfied(
            "android.launch_app",
            "postcondition failed after launch",
            &postcondition,
        )?;
        Ok(CallToolResult::structured(json!({
            "ok": postcondition.satisfied,
            "serial": serial,
            "package_name": args.package_name,
            "activity": args.activity,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "postcondition": tool_postcondition_json(&postcondition),
        })))
    }

    #[tool(
        name = "android.capture_screenshot",
        description = "Capture a PNG screenshot from the device and save it locally."
    )]
    async fn android_capture_screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let (path, backend_used) = self
            .capture_screenshot_internal(
                &serial,
                filename_or_timestamp(args.filename, "capture-screenshot", "png"),
            )
            .await?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "serial": serial,
            "path": path,
            "backend_used": backend_used,
        })))
    }

    #[tool(
        name = "android.dump_ui_hierarchy",
        description = "Dump the current UI hierarchy XML and save it locally."
    )]
    async fn android_dump_ui_hierarchy(
        &self,
        Parameters(args): Parameters<UiDumpArgs>,
    ) -> Result<CallToolResult, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let local_path = self
            .dump_ui_hierarchy_internal(&serial, args.filename.unwrap_or_default())
            .await?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "serial": serial,
            "path": local_path,
        })))
    }

    #[tool(
        name = "android.inspect_ui",
        description = "Capture the current UI hierarchy, optionally pair it with a screenshot, and return a normalized UI tree."
    )]
    async fn android_inspect_ui(
        &self,
        Parameters(args): Parameters<InspectUiArgs>,
    ) -> Result<CallToolResult, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let observation = self
            .capture_ui_observation(
                &serial,
                filename_or_timestamp(args.hierarchy_filename, "inspect-ui", "xml"),
                args.include_screenshot,
                filename_or_timestamp(args.screenshot_filename, "inspect-ui", "png"),
            )
            .await?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "serial": serial,
            "artifacts": {
                "hierarchy_path": observation.hierarchy_path,
                "screenshot_path": observation.screenshot_path,
                "screenshot_backend": observation.screenshot_backend,
            },
            "current_focus": observation.window_state.current_focus,
            "window_state": observation.window_state,
            "node_count": observation.nodes.len(),
            "nodes": observation.nodes,
        })))
    }

    #[tool(
        name = "android.wait_for_stable_ui",
        description = "Wait until the UI hierarchy and top window state stop changing, then return a paired observation bundle."
    )]
    async fn android_wait_for_stable_ui(
        &self,
        Parameters(args): Parameters<WaitForStableUiArgs>,
    ) -> Result<CallToolResult, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let deadline = tool_deadline(args.timeout_secs, 15);
        let stable_result = self
            .wait_for_stable_ui_internal(StableUiWaitRequest {
                serial: &serial,
                deadline,
                poll_interval: Duration::from_millis(args.poll_interval_ms.unwrap_or(500)),
                required_stable_polls: args.stable_polls.unwrap_or(2).max(1),
                hierarchy_filename: filename_or_timestamp(
                    args.hierarchy_filename.clone(),
                    "wait-stable-ui",
                    "xml",
                ),
                include_screenshot: args.include_screenshot,
                screenshot_filename: filename_or_timestamp(
                    args.screenshot_filename.clone(),
                    "wait-stable-ui",
                    "png",
                ),
            })
            .await?;

        Ok(CallToolResult::structured(json!({
            "ok": stable_result.stabilized,
            "serial": serial,
            "stabilized": stable_result.stabilized,
            "timed_out": stable_result.timed_out,
            "elapsed_ms": stable_result.elapsed_ms,
            "stable_polls_required": stable_result.stable_polls_required,
            "stable_polls_observed": stable_result.stable_polls_observed,
            "artifacts": {
                "hierarchy_path": stable_result.observation.hierarchy_path,
                "screenshot_path": stable_result.observation.screenshot_path,
                "screenshot_backend": stable_result.observation.screenshot_backend,
            },
            "current_focus": stable_result.observation.window_state.current_focus,
            "window_state": stable_result.observation.window_state,
            "node_count": stable_result.observation.nodes.len(),
            "nodes": stable_result.observation.nodes,
        })))
    }

    #[tool(
        name = "android.find_ui_element",
        description = "Find the first normalized UI element matching a semantic selector in the current hierarchy."
    )]
    async fn android_find_ui_element(
        &self,
        Parameters(args): Parameters<FindUiElementArgs>,
    ) -> Result<CallToolResult, McpError> {
        let selector = normalize_selector_input(args.selector);
        ensure_selector_not_empty(&selector)?;
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let hierarchy_path = self
            .dump_ui_hierarchy_internal(
                &serial,
                filename_or_timestamp(args.hierarchy_filename, "find-ui-element", "xml"),
            )
            .await?;
        let nodes = parse_ui_nodes_from_path(&hierarchy_path)?;
        let matches = matching_nodes(&nodes, &selector).matches;
        let current_focus = self.current_focus_internal(&serial).await.ok().flatten();
        match resolve_node_selection(matches, args.match_index) {
            Ok(selection) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "serial": serial,
                "matched": true,
                "match_count": selection.match_count,
                "ambiguous": selection.match_count > 1,
                "selected_match_index": selection.selected_match_index,
                "selector": selector,
                "artifacts": {
                    "hierarchy_path": hierarchy_path,
                },
                "current_focus": current_focus,
                "node": selection.node,
                "candidate_summary": selection.candidates,
            }))),
            Err(error) => Ok(CallToolResult::structured(json!({
                "ok": false,
                "serial": serial,
                "matched": false,
                "selector": selector,
                "artifacts": {
                    "hierarchy_path": hierarchy_path,
                },
                "current_focus": current_focus,
                "selection": selection_failure_json(&error),
            }))),
        }
    }

    #[tool(
        name = "android.wait_for_ui_element",
        description = "Wait until a semantic UI selector is present or absent in the current hierarchy."
    )]
    async fn android_wait_for_ui_element(
        &self,
        Parameters(args): Parameters<WaitUiElementArgs>,
    ) -> Result<CallToolResult, McpError> {
        let selector = normalize_selector_input(args.selector);
        ensure_selector_not_empty(&selector)?;
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(15));
        let started = std::time::Instant::now();
        let mut last_hierarchy_path: Option<PathBuf> = None;
        let mut last_node = None;
        let mut last_match_count = 0usize;
        let mut last_candidates: Vec<SelectorCandidateSummary> = Vec::new();
        let mut last_selected_match_index: Option<usize> = None;
        let mut last_dialog: Option<SystemDialogReport> = None;

        while started.elapsed() <= timeout {
            let hierarchy_path = self
                .dump_ui_hierarchy_internal(
                    &serial,
                    filename_or_timestamp(
                        args.hierarchy_filename.clone(),
                        "wait-ui-element",
                        "xml",
                    ),
                )
                .await?;
            let nodes = parse_ui_nodes_from_path(&hierarchy_path)?;
            match self
                .check_system_dialog(&serial, &hierarchy_path, &nodes)
                .await?
            {
                SystemDialogCheck::Handled(report) => {
                    last_dialog = Some(report);
                    continue;
                }
                SystemDialogCheck::Blocked(report) => {
                    return Ok(CallToolResult::structured(json!({
                        "ok": false,
                        "serial": serial,
                        "selector": selector,
                        "absent": args.absent,
                        "satisfied": false,
                        "timed_out": false,
                        "elapsed_ms": started.elapsed().as_millis(),
                        "artifacts": {
                            "hierarchy_path": hierarchy_path,
                        },
                        "system_dialog": report,
                    })));
                }
                SystemDialogCheck::None => {}
            }
            remove_artifact_if_exists(
                last_hierarchy_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            )
            .await;
            let matches = matching_nodes(&nodes, &selector).matches;
            let matched = !matches.is_empty();
            let selection_result = if args.absent {
                None
            } else {
                Some(resolve_node_selection(matches.clone(), args.match_index))
            };
            let satisfied = if args.absent { !matched } else { matched };
            last_hierarchy_path = Some(hierarchy_path);
            last_match_count = matches.len();
            last_candidates = selector_candidate_summary(&matches);
            match selection_result {
                Some(Ok(selection)) => {
                    last_selected_match_index = Some(selection.selected_match_index);
                    last_node = Some(selection.node.clone());
                    if satisfied {
                        let current_focus =
                            self.current_focus_internal(&serial).await.ok().flatten();
                        return Ok(CallToolResult::structured(json!({
                            "ok": true,
                            "serial": serial,
                            "selector": selector,
                            "absent": args.absent,
                            "matched": true,
                            "match_count": selection.match_count,
                            "ambiguous": selection.match_count > 1,
                            "selected_match_index": selection.selected_match_index,
                            "satisfied": true,
                            "timed_out": false,
                            "elapsed_ms": started.elapsed().as_millis(),
                            "artifacts": {
                                "hierarchy_path": last_hierarchy_path,
                            },
                            "current_focus": current_focus,
                            "node": last_node,
                            "candidate_summary": selection.candidates,
                            "system_dialog": last_dialog,
                        })));
                    }
                }
                None if args.absent && !matched => {
                    let current_focus = self.current_focus_internal(&serial).await.ok().flatten();
                    return Ok(CallToolResult::structured(json!({
                        "ok": true,
                        "serial": serial,
                        "selector": selector,
                        "absent": true,
                        "matched": false,
                        "match_count": 0,
                        "ambiguous": false,
                        "selected_match_index": serde_json::Value::Null,
                        "satisfied": true,
                        "timed_out": false,
                        "elapsed_ms": started.elapsed().as_millis(),
                        "artifacts": {
                            "hierarchy_path": last_hierarchy_path,
                        },
                        "current_focus": current_focus,
                        "node": serde_json::Value::Null,
                        "candidate_summary": [],
                        "system_dialog": last_dialog,
                    })));
                }
                Some(Err(error)) if !args.absent => {
                    return Ok(CallToolResult::structured(json!({
                        "ok": false,
                        "serial": serial,
                        "selector": selector,
                        "absent": false,
                        "matched": matched,
                        "match_count": last_match_count,
                        "ambiguous": last_match_count > 1,
                        "selected_match_index": serde_json::Value::Null,
                        "satisfied": false,
                        "timed_out": false,
                        "elapsed_ms": started.elapsed().as_millis(),
                        "artifacts": {
                            "hierarchy_path": last_hierarchy_path,
                        },
                        "current_focus": self.current_focus_internal(&serial).await.ok().flatten(),
                        "node": serde_json::Value::Null,
                        "candidate_summary": last_candidates,
                        "selection": selection_failure_json(&error),
                        "system_dialog": last_dialog,
                    })));
                }
                _ => {}
            }
            sleep(Duration::from_millis(500)).await;
        }

        let current_focus = self.current_focus_internal(&serial).await.ok().flatten();
        Ok(CallToolResult::structured(json!({
            "ok": false,
            "serial": serial,
            "selector": selector,
            "absent": args.absent,
            "matched": last_node.is_some(),
            "match_count": last_match_count,
            "ambiguous": last_match_count > 1,
            "selected_match_index": last_selected_match_index,
            "satisfied": false,
            "timed_out": true,
            "elapsed_ms": started.elapsed().as_millis(),
            "artifacts": {
                "hierarchy_path": last_hierarchy_path,
            },
            "current_focus": current_focus,
            "node": last_node,
            "candidate_summary": last_candidates,
            "system_dialog": last_dialog,
        })))
    }

    #[tool(
        name = "android.tap_element",
        description = "Find a semantic UI element and tap its center point."
    )]
    async fn android_tap_element(
        &self,
        Parameters(args): Parameters<TapElementArgs>,
    ) -> Result<CallToolResult, McpError> {
        let selector = normalize_selector_input(args.selector);
        let wait_for_selector = normalize_optional_selector_input(args.wait_for_selector);
        ensure_selector_not_empty(&selector)?;
        if let Some(wait_for_selector) = wait_for_selector.as_ref() {
            ensure_selector_not_empty(wait_for_selector)?;
        }
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let deadline = tool_deadline(args.timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
        let mut dialog_report = None;
        let (pre_tap_hierarchy_path, pre_tap_nodes, resolved) = loop {
            let hierarchy_path = self
                .dump_ui_hierarchy_internal_with_deadline(
                    &serial,
                    filename_or_timestamp(args.hierarchy_filename.clone(), "tap-element", "xml"),
                    Some(deadline),
                )
                .await?;
            let nodes = parse_ui_nodes_from_path(&hierarchy_path)?;
            match self
                .check_system_dialog(&serial, &hierarchy_path, &nodes)
                .await?
            {
                SystemDialogCheck::Handled(report) => {
                    dialog_report = Some(report);
                    continue;
                }
                SystemDialogCheck::Blocked(report) => {
                    return Err(McpError::internal_error(
                        format!(
                            "android.tap_element blocked by system dialog: {}",
                            serde_json::to_string(&report)
                                .unwrap_or_else(|_| "system dialog".to_string())
                        ),
                        None,
                    ));
                }
                SystemDialogCheck::None => {}
            }
            let candidates = find_interactive_ui_node(&hierarchy_path, &selector)?;
            break (
                Some(hierarchy_path),
                nodes,
                resolve_node_selection(candidates.matches, args.match_index),
            );
        };
        let selection = resolved.map_err(|error| {
            McpError::internal_error(
                format!(
                    "android.tap_element could not resolve selector {:?}: {}",
                    selector,
                    selection_failure_json(&error)
                ),
                None,
            )
        })?;
        let node = Some(selection.node.clone());
        let center = actionable_center(&node, &selector, "tap")?;
        let mut attempted_backends = Vec::new();
        let mut dispatch = self.tap_point_internal(&serial, center.0, center.1).await?;
        attempted_backends.push(dispatch.backend);
        let mut attempts = Vec::new();
        let mut verification = self
            .verify_tap_outcome(TapVerificationRequest {
                serial: &serial,
                tapped_selector: &selector,
                pre_tap_nodes: &pre_tap_nodes,
                wait_until_absent: args.wait_until_absent,
                wait_for_tracker: None,
                wait_for_selector: wait_for_selector.as_ref(),
                deadline,
                hierarchy_filename: args.hierarchy_filename.clone(),
            })
            .await?;
        attempts.push(json!({
            "backend": dispatch.backend,
            "dispatch_detail": dispatch.detail,
            "verification": tap_verification_json(&verification),
        }));
        let should_retry_with_adb = should_retry_tap_with_adb(
            dispatch.backend,
            args.retry_with_adb_on_no_change,
            &verification,
        );
        if should_retry_with_adb {
            let retry_reason = format!(
                "initial grpc tap did not satisfy verification: {}",
                tap_verification_summary(&verification)
            );
            dispatch = self
                .tap_point_internal_adb(&serial, center.0, center.1)
                .await?;
            attempted_backends.push(dispatch.backend);
            verification = self
                .verify_tap_outcome(TapVerificationRequest {
                    serial: &serial,
                    tapped_selector: &selector,
                    pre_tap_nodes: &pre_tap_nodes,
                    wait_until_absent: args.wait_until_absent,
                    wait_for_tracker: None,
                    wait_for_selector: wait_for_selector.as_ref(),
                    deadline,
                    hierarchy_filename: args.hierarchy_filename.clone(),
                })
                .await?;
            attempts.push(json!({
                "backend": dispatch.backend,
                "dispatch_detail": dispatch
                    .detail
                    .clone()
                    .or(Some(retry_reason)),
                "verification": tap_verification_json(&verification),
            }));
        }
        let tap_satisfied = !verification.requested || verification.satisfied;
        if !tap_satisfied && !args.allow_verification_failure {
            return Err(McpError::internal_error(
                format!(
                    "android.tap_element verification failed after dispatch: {}",
                    json!({
                        "serial": serial,
                        "selector": selector,
                        "attempted_backends": attempted_backends,
                        "attempts": attempts,
                        "artifacts": {
                            "hierarchy_path": pre_tap_hierarchy_path,
                            "post_tap_hierarchy_path": verification.hierarchy_path,
                        },
                        "system_dialog": dialog_report,
                    })
                ),
                None,
            ));
        }
        Ok(CallToolResult::structured(json!({
            "ok": tap_satisfied,
            "serial": serial,
            "selector": selector,
            "tapped": true,
            "tap_dispatched": true,
            "match_count": selection.match_count,
            "ambiguous": selection.match_count > 1,
            "selected_match_index": selection.selected_match_index,
            "allow_verification_failure": args.allow_verification_failure,
            "backend_used": dispatch.backend,
            "attempted_backends": attempted_backends,
            "attempts": attempts,
            "artifacts": {
                "hierarchy_path": pre_tap_hierarchy_path,
                "post_tap_hierarchy_path": verification.hierarchy_path,
            },
            "stdout": dispatch.output.stdout,
            "stderr": dispatch.output.stderr,
            "node": node,
            "verification": tap_verification_json(&verification),
            "candidate_summary": selection.candidates,
            "system_dialog": dialog_report,
        })))
    }

    #[tool(
        name = "android.type_into_element",
        description = "Find a semantic UI element, tap it, and send text input."
    )]
    async fn android_type_into_element(
        &self,
        Parameters(args): Parameters<TypeIntoElementArgs>,
    ) -> Result<CallToolResult, McpError> {
        let selector = normalize_selector_input(args.selector);
        ensure_selector_not_empty(&selector)?;
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let deadline = tool_deadline(args.timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
        let mut dialog_report = None;
        let (hierarchy_path, selection) = loop {
            let hierarchy_path = self
                .dump_ui_hierarchy_internal_with_deadline(
                    &serial,
                    filename_or_timestamp(
                        args.hierarchy_filename.clone(),
                        "type-into-element",
                        "xml",
                    ),
                    Some(deadline),
                )
                .await?;
            let nodes = parse_ui_nodes_from_path(&hierarchy_path)?;
            match self
                .check_system_dialog(&serial, &hierarchy_path, &nodes)
                .await?
            {
                SystemDialogCheck::Handled(report) => {
                    dialog_report = Some(report);
                    continue;
                }
                SystemDialogCheck::Blocked(report) => {
                    return Err(McpError::internal_error(
                        format!(
                            "android.type_into_element blocked by system dialog: {}",
                            serde_json::to_string(&report)
                                .unwrap_or_else(|_| "system dialog".to_string())
                        ),
                        None,
                    ));
                }
                SystemDialogCheck::None => {}
            }
            let candidates = find_interactive_ui_node(&hierarchy_path, &selector)?;
            let selection =
                resolve_node_selection(candidates.matches, args.match_index).map_err(|error| {
                    McpError::internal_error(
                        format!(
                            "android.type_into_element could not resolve selector {:?}: {}",
                            selector,
                            selection_failure_json(&error)
                        ),
                        None,
                    )
                })?;
            break (hierarchy_path, selection);
        };
        let node = Some(selection.node.clone());
        let focus_target_selector = UiSelector {
            focused: Some(true),
            ..text_verification_target_selector(&selection.node)
        };
        let focus_target_tracker =
            InternalNodeTracker::from_target_node_with_focus(&selection.node, Some(true))
                .unwrap_or_else(|| InternalNodeTracker::Selector(focus_target_selector.clone()));
        let pre_tap_nodes = parse_ui_nodes_from_path(&hierarchy_path)?;
        let already_focused = pre_tap_nodes
            .iter()
            .any(|node| tracker_matches_node(node, &focus_target_tracker));
        let mut tap_dispatch = ActionDispatch {
            backend: "none",
            output: CommandOutput::default(),
            detail: Some("target already focused before typing".to_string()),
        };
        let focus_verification = if already_focused {
            TapVerification {
                requested: true,
                wait_until_absent: false,
                wait_for_selector: Some(focus_target_selector.clone()),
                satisfied: true,
                stabilized: true,
                stable_polls_observed: 0,
                stable_polls_required: 0,
                timed_out: false,
                elapsed_ms: 0,
                hierarchy_path: Some(hierarchy_path.display().to_string()),
                tapped_selector_present_pre_tap: Some(true),
                post_selector_matched_pre_tap: Some(true),
                tapped_selector_still_present: Some(true),
                post_selector_matched: Some(true),
                ui_changed_from_pre_tap: Some(false),
            }
        } else {
            let center = actionable_center(&node, &selector, "type into")?;
            tap_dispatch = self.tap_point_internal(&serial, center.0, center.1).await?;
            self.verify_tap_outcome(TapVerificationRequest {
                serial: &serial,
                tapped_selector: &selector,
                pre_tap_nodes: &pre_tap_nodes,
                wait_until_absent: false,
                wait_for_tracker: Some(&focus_target_tracker),
                wait_for_selector: Some(&focus_target_selector),
                deadline,
                hierarchy_filename: Some("type-into-element-focus.xml".to_string()),
            })
            .await?
        };
        if !focus_verification.satisfied {
            return Err(McpError::internal_error(
                format!(
                    "android.type_into_element could not confirm that the target field gained focus before typing: {}",
                    json!({
                        "serial": serial,
                        "selector": selector,
                        "tap_backend_used": tap_dispatch.backend,
                        "focus_verification": tap_verification_json(&focus_verification),
                    })
                ),
                None,
            ));
        }
        let focused_nodes = focus_verification
            .hierarchy_path
            .as_deref()
            .map(Path::new)
            .map(parse_ui_nodes_from_path)
            .transpose()?
            .unwrap_or_default();
        let focused_node = focused_nodes
            .iter()
            .find(|node| tracker_matches_node(node, &focus_target_tracker))
            .cloned();
        let public_text_target_selector = focused_node
            .as_ref()
            .map(text_verification_target_selector)
            .unwrap_or_else(|| focus_target_selector.clone());
        let text_target_tracker = focused_node
            .as_ref()
            .and_then(InternalNodeTracker::from_target_node)
            .unwrap_or_else(|| InternalNodeTracker::Selector(public_text_target_selector.clone()));
        if focused_node
            .as_ref()
            .is_some_and(|node| node_matches_requested_text(node, &args.text))
        {
            let no_op_verification = TextVerification {
                requested: true,
                target_selector: Some(public_text_target_selector.clone()),
                wait_for_selector: None,
                satisfied: true,
                stabilized: true,
                stable_polls_observed: 0,
                stable_polls_required: 0,
                timed_out: false,
                elapsed_ms: 0,
                hierarchy_path: focus_verification.hierarchy_path.clone(),
                target_selector_present_pre_text: Some(true),
                post_selector_matched_pre_text: None,
                target_selector_still_present: Some(true),
                target_text_matches_requested: Some(true),
                post_selector_matched: None,
                ui_changed_from_pre_text: Some(false),
            };
            return Ok(CallToolResult::structured(json!({
                "ok": true,
                "serial": serial,
                "selector": selector,
                "text": args.text,
                "typed": false,
                "match_count": selection.match_count,
                "ambiguous": selection.match_count > 1,
                "selected_match_index": selection.selected_match_index,
                "tap_backend_used": tap_dispatch.backend,
                "text_backend_used": "none",
                "text_dispatch_detail": "target already contained requested text",
                "node": node,
                "focus_hierarchy_path": focus_verification.hierarchy_path,
                "text_hierarchy_path": no_op_verification.hierarchy_path,
                "stdout": tap_dispatch.output.stdout,
                "stderr": tap_dispatch.output.stderr,
                "dialog_report": dialog_report,
                "focus_verification": tap_verification_json(&focus_verification),
                "text_verification": text_verification_json(&no_op_verification),
            })));
        }
        let text_dispatch = self
            .send_text_with_verification(VerifiedTextDispatchRequest {
                serial: &serial,
                text: &args.text,
                pre_text_nodes: &focused_nodes,
                public_target_selector: Some(&public_text_target_selector),
                target_tracker: Some(&text_target_tracker),
                target_selector: Some(&public_text_target_selector),
                wait_for_selector: None,
                deadline,
                hierarchy_filename: Some("type-into-element-text.xml".to_string()),
                retry_with_adb_on_no_change: true,
                replace_existing_text_on_adb: true,
                existing_text_for_adb_replace: focused_node
                    .as_ref()
                    .and_then(|node| node.text.as_deref().or(node.semantic_label.as_deref())),
            })
            .await?;
        ensure_action_outcome_satisfied(
            "android.type_into_element",
            "text effect verification failed after dispatch",
            true,
            text_dispatch.verification.satisfied,
            json!({
                "serial": serial,
                "selector": selector,
                "text": args.text,
                "text_backend_used": text_dispatch.dispatch.backend,
                "text_verification": text_verification_json(&text_dispatch.verification),
            }),
        )?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "serial": serial,
            "selector": selector,
            "text": args.text,
            "typed": true,
            "match_count": selection.match_count,
            "ambiguous": selection.match_count > 1,
            "selected_match_index": selection.selected_match_index,
            "tap_backend_used": tap_dispatch.backend,
            "text_backend_used": text_dispatch.dispatch.backend,
            "text_dispatch_detail": text_dispatch.dispatch.detail,
            "artifacts": {
                "hierarchy_path": hierarchy_path,
                "focus_hierarchy_path": focus_verification.hierarchy_path,
                "text_hierarchy_path": text_dispatch.verification.hierarchy_path,
            },
            "stdout": merge_stdout(&tap_dispatch.output, &text_dispatch.dispatch.output),
            "stderr": merge_stderr(&tap_dispatch.output, &text_dispatch.dispatch.output),
            "node": node,
            "focused_node": focused_node,
            "focus_verification": tap_verification_json(&focus_verification),
            "text_verification": text_verification_json(&text_dispatch.verification),
            "candidate_summary": selection.candidates,
            "system_dialog": dialog_report,
        })))
    }

    #[tool(
        name = "android.scroll_until_visible",
        description = "Swipe the viewport until a semantic UI selector becomes visible or the swipe budget is exhausted."
    )]
    async fn android_scroll_until_visible(
        &self,
        Parameters(args): Parameters<ScrollUntilVisibleArgs>,
    ) -> Result<CallToolResult, McpError> {
        let selector = normalize_selector_input(args.selector);
        ensure_selector_not_empty(&selector)?;
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let max_swipes = args.max_swipes.unwrap_or(5);
        let direction = normalize_scroll_direction(args.direction.as_deref())?;
        let display = self.display_size_internal(&serial).await?;
        let swipe_points = swipe_points_for_direction(display, direction);
        let mut last_hierarchy_path: Option<PathBuf> = None;
        let mut last_node = None;
        let mut last_match_count = 0usize;
        let mut swipe_backend = None;
        let mut last_candidates: Vec<SelectorCandidateSummary> = Vec::new();
        let mut last_selected_match_index: Option<usize> = None;
        let mut last_dialog: Option<SystemDialogReport> = None;

        for swipe_index in 0..=max_swipes {
            let hierarchy_path = self
                .dump_ui_hierarchy_internal(
                    &serial,
                    filename_or_timestamp(
                        args.hierarchy_filename.clone(),
                        "scroll-until-visible",
                        "xml",
                    ),
                )
                .await?;
            let nodes = parse_ui_nodes_from_path(&hierarchy_path)?;
            match self
                .check_system_dialog(&serial, &hierarchy_path, &nodes)
                .await?
            {
                SystemDialogCheck::Handled(report) => {
                    last_dialog = Some(report);
                    continue;
                }
                SystemDialogCheck::Blocked(report) => {
                    return Ok(CallToolResult::structured(json!({
                        "ok": false,
                        "serial": serial,
                        "selector": selector,
                        "direction": direction.as_str(),
                        "matched": false,
                        "swipes_used": swipe_index,
                        "max_swipes": max_swipes,
                        "display_size": display,
                        "artifacts": {
                            "hierarchy_path": hierarchy_path,
                        },
                        "system_dialog": report,
                    })));
                }
                SystemDialogCheck::None => {}
            }
            let matches = matching_nodes(&nodes, &selector).matches;
            last_hierarchy_path = Some(hierarchy_path);
            last_match_count = matches.len();
            last_candidates = selector_candidate_summary(&matches);
            match resolve_node_selection(matches, args.match_index) {
                Ok(selection) => {
                    last_node = Some(selection.node.clone());
                    return Ok(CallToolResult::structured(json!({
                        "ok": true,
                        "serial": serial,
                        "selector": selector,
                        "direction": direction.as_str(),
                        "matched": true,
                        "match_count": selection.match_count,
                        "ambiguous": selection.match_count > 1,
                        "selected_match_index": selection.selected_match_index,
                        "swipes_used": swipe_index,
                        "max_swipes": max_swipes,
                        "display_size": display,
                        "artifacts": {
                            "hierarchy_path": last_hierarchy_path,
                        },
                        "node": last_node,
                        "candidate_summary": selection.candidates,
                        "system_dialog": last_dialog,
                    })));
                }
                Err(SelectionFailure::NoMatches) => {
                    last_node = None;
                    last_selected_match_index = None;
                }
                Err(error) => {
                    return Ok(CallToolResult::structured(json!({
                        "ok": false,
                        "serial": serial,
                        "selector": selector,
                        "direction": direction.as_str(),
                        "matched": true,
                        "match_count": last_match_count,
                        "ambiguous": last_match_count > 1,
                        "selected_match_index": serde_json::Value::Null,
                        "swipes_used": swipe_index,
                        "max_swipes": max_swipes,
                        "display_size": display,
                        "artifacts": {
                            "hierarchy_path": last_hierarchy_path,
                        },
                        "node": serde_json::Value::Null,
                        "candidate_summary": last_candidates,
                        "selection": selection_failure_json(&error),
                        "system_dialog": last_dialog,
                    })));
                }
            };
            if swipe_index < max_swipes {
                let dispatch = self
                    .swipe_points_internal(
                        &serial,
                        swipe_points.0,
                        swipe_points.1,
                        swipe_points.2,
                        swipe_points.3,
                        DEFAULT_SWIPE_DURATION_MS,
                    )
                    .await?;
                swipe_backend = Some(dispatch.backend);
            }
        }

        Ok(CallToolResult::structured(json!({
            "ok": false,
            "serial": serial,
            "selector": selector,
            "direction": direction.as_str(),
            "matched": false,
            "match_count": last_match_count,
            "ambiguous": last_match_count > 1,
            "selected_match_index": last_selected_match_index,
            "swipes_used": max_swipes,
            "max_swipes": max_swipes,
            "display_size": display,
            "swipe_backend_used": swipe_backend,
            "exhausted_swipe_budget": true,
            "artifacts": {
                "hierarchy_path": last_hierarchy_path,
            },
            "node": last_node,
            "candidate_summary": last_candidates,
            "system_dialog": last_dialog,
        })))
    }

    #[tool(
        name = "android.collect_logcat",
        description = "Capture recent logcat output and save it locally."
    )]
    async fn android_collect_logcat(
        &self,
        Parameters(args): Parameters<LogcatArgs>,
    ) -> Result<CallToolResult, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let lines = args.lines.unwrap_or(400).to_string();
        let output = self
            .run_adb(
                &serial,
                [
                    "logcat".to_string(),
                    "-d".to_string(),
                    "-t".to_string(),
                    lines,
                ],
            )
            .await?;
        let path = self
            .artifact_path("logcat", args.filename, Some("txt"))
            .await?;
        fs::write(&path, &output.stdout)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "serial": serial,
            "path": path,
            "stderr": output.stderr,
        })))
    }

    #[tool(
        name = "android.input.tap",
        description = "Send a tap input event to the device."
    )]
    async fn android_input_tap(
        &self,
        Parameters(args): Parameters<TapArgs>,
    ) -> Result<CallToolResult, McpError> {
        let tapped_selector = normalize_optional_selector_input(args.tapped_selector);
        let wait_for_selector = normalize_optional_selector_input(args.wait_for_selector);
        if let Some(selector) = tapped_selector.as_ref() {
            ensure_selector_not_empty(selector)?;
        }
        if let Some(selector) = wait_for_selector.as_ref() {
            ensure_selector_not_empty(selector)?;
        }
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let deadline = tool_deadline(args.timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
        let pre_tap_nodes = if tapped_selector.is_some() || wait_for_selector.is_some() {
            let path = self
                .dump_ui_hierarchy_internal_with_deadline(
                    &serial,
                    filename_or_timestamp(None, "raw-tap", "xml"),
                    Some(deadline),
                )
                .await?;
            Some(parse_ui_nodes_from_path(&path)?)
        } else {
            None
        };
        let dispatch = self.tap_point_internal(&serial, args.x, args.y).await?;
        let verification = if let Some(tapped_selector) = tapped_selector.as_ref() {
            Some(
                self.verify_tap_outcome(TapVerificationRequest {
                    serial: &serial,
                    tapped_selector,
                    pre_tap_nodes: pre_tap_nodes.as_deref().unwrap_or(&[]),
                    wait_until_absent: args.wait_until_absent,
                    wait_for_tracker: None,
                    wait_for_selector: wait_for_selector.as_ref(),
                    deadline,
                    hierarchy_filename: None,
                })
                .await?,
            )
        } else {
            None
        };
        let postcondition = if tapped_selector.is_none() && wait_for_selector.is_some() {
            self.wait_for_tool_postcondition(ToolPostconditionRequest {
                serial: &serial,
                selector: wait_for_selector.as_ref(),
                match_index: None,
                wait_for_activity: None,
                wait_for_package: None,
                deadline,
                include_screenshot: false,
                artifact_prefix: "raw-tap-postcondition",
            })
            .await?
        } else {
            ToolPostconditionResult {
                requested: verification.is_some(),
                satisfied: verification
                    .as_ref()
                    .map(|result| result.satisfied)
                    .unwrap_or(true),
                timed_out: verification
                    .as_ref()
                    .map(|result| result.timed_out)
                    .unwrap_or(false),
                elapsed_ms: verification
                    .as_ref()
                    .map(|result| result.elapsed_ms)
                    .unwrap_or(0),
                hierarchy_path: verification
                    .as_ref()
                    .and_then(|result| result.hierarchy_path.clone()),
                screenshot_path: None,
                observed_activity: None,
                observed_package: None,
                node: None,
                match_count: 0,
                selected_match_index: None,
                candidate_summary: Vec::new(),
            }
        };
        ensure_tool_postcondition_satisfied(
            "android.input.tap",
            "postcondition failed after dispatch",
            &postcondition,
        )?;
        Ok(CallToolResult::structured(json!({
            "ok": postcondition.satisfied,
            "serial": serial,
            "x": args.x,
            "y": args.y,
            "backend_used": dispatch.backend,
            "dispatch_detail": dispatch.detail,
            "stdout": dispatch.output.stdout,
            "stderr": dispatch.output.stderr,
            "verification": verification.as_ref().map(tap_verification_json),
            "postcondition": tool_postcondition_json(&postcondition),
        })))
    }

    #[tool(
        name = "android.input.text",
        description = "Send text input to the device."
    )]
    async fn android_input_text(
        &self,
        Parameters(args): Parameters<TextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let expect_focus_selector = normalize_optional_selector_input(args.expect_focus_selector);
        let wait_for_selector = normalize_optional_selector_input(args.wait_for_selector);
        if let Some(selector) = expect_focus_selector.as_ref() {
            ensure_selector_not_empty(selector)?;
        }
        if let Some(selector) = wait_for_selector.as_ref() {
            ensure_selector_not_empty(selector)?;
        }
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let deadline = tool_deadline(args.timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
        let focus_postcondition = self
            .wait_for_tool_postcondition(ToolPostconditionRequest {
                serial: &serial,
                selector: expect_focus_selector.as_ref(),
                match_index: None,
                wait_for_activity: None,
                wait_for_package: None,
                deadline,
                include_screenshot: false,
                artifact_prefix: "input-text-focus",
            })
            .await?;
        ensure_tool_postcondition_satisfied(
            "android.input.text",
            "focus expectation failed before typing",
            &focus_postcondition,
        )?;
        let pre_text_nodes = if let Some(path) = focus_postcondition.hierarchy_path.as_deref() {
            parse_ui_nodes_from_path(Path::new(path))?
        } else if expect_focus_selector.is_some() || wait_for_selector.is_some() {
            self.capture_ui_observation_with_deadline(
                &serial,
                filename_or_timestamp(None, "input-text-pre", "xml"),
                false,
                filename_or_timestamp(None, "input-text-pre", "png"),
                Some(deadline),
            )
            .await?
            .nodes
        } else {
            Vec::new()
        };
        let public_text_target_selector = focus_postcondition
            .node
            .as_ref()
            .map(text_verification_target_selector);
        let text_target_tracker = focus_postcondition
            .node
            .as_ref()
            .and_then(InternalNodeTracker::from_target_node)
            .or_else(|| {
                public_text_target_selector
                    .as_ref()
                    .or(expect_focus_selector.as_ref())
                    .cloned()
                    .map(InternalNodeTracker::Selector)
            });
        let verified_dispatch = self
            .send_text_with_verification(VerifiedTextDispatchRequest {
                serial: &serial,
                text: &args.text,
                pre_text_nodes: &pre_text_nodes,
                public_target_selector: public_text_target_selector
                    .as_ref()
                    .or(expect_focus_selector.as_ref()),
                target_tracker: text_target_tracker.as_ref(),
                target_selector: public_text_target_selector
                    .as_ref()
                    .or(expect_focus_selector.as_ref()),
                wait_for_selector: wait_for_selector.as_ref(),
                deadline,
                hierarchy_filename: Some("input-text-verify.xml".to_string()),
                retry_with_adb_on_no_change: true,
                replace_existing_text_on_adb: false,
                existing_text_for_adb_replace: None,
            })
            .await?;
        ensure_action_outcome_satisfied(
            "android.input.text",
            "text effect verification failed after dispatch",
            verified_dispatch.verification.requested,
            verified_dispatch.verification.satisfied,
            json!({
                "focus_postcondition": tool_postcondition_json(&focus_postcondition),
                "text_verification": text_verification_json(&verified_dispatch.verification),
            }),
        )?;
        let postcondition = self
            .wait_for_tool_postcondition(ToolPostconditionRequest {
                serial: &serial,
                selector: wait_for_selector.as_ref(),
                match_index: None,
                wait_for_activity: None,
                wait_for_package: None,
                deadline,
                include_screenshot: false,
                artifact_prefix: "input-text-postcondition",
            })
            .await?;
        ensure_tool_postcondition_satisfied(
            "android.input.text",
            "postcondition failed after dispatch",
            &postcondition,
        )?;
        Ok(CallToolResult::structured(json!({
            "ok": postcondition.satisfied,
            "serial": serial,
            "text": args.text,
            "backend_used": verified_dispatch.dispatch.backend,
            "dispatch_detail": verified_dispatch.dispatch.detail,
            "stdout": verified_dispatch.dispatch.output.stdout,
            "stderr": verified_dispatch.dispatch.output.stderr,
            "focus_postcondition": tool_postcondition_json(&focus_postcondition),
            "text_verification": text_verification_json(&verified_dispatch.verification),
            "postcondition": tool_postcondition_json(&postcondition),
        })))
    }

    #[tool(
        name = "android.input.swipe",
        description = "Send a swipe gesture to the device."
    )]
    async fn android_input_swipe(
        &self,
        Parameters(args): Parameters<SwipeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let wait_for_selector = normalize_optional_selector_input(args.wait_for_selector);
        if let Some(selector) = wait_for_selector.as_ref() {
            ensure_selector_not_empty(selector)?;
        }
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let duration = args.duration_ms.unwrap_or(DEFAULT_SWIPE_DURATION_MS);
        let deadline = tool_deadline(args.timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
        let pre_scroll_fingerprint = if args.expect_scroll_change {
            let observation = self
                .capture_ui_observation_with_deadline(
                    &serial,
                    filename_or_timestamp(None, "input-swipe-pre", "xml"),
                    false,
                    filename_or_timestamp(None, "input-swipe-pre", "png"),
                    Some(deadline),
                )
                .await?;
            Some(tap_verification_fingerprint(
                &observation.nodes,
                &UiSelector {
                    scrollable: Some(true),
                    ..UiSelector::default()
                },
                None,
                None,
            ))
        } else {
            None
        };
        let dispatch = self
            .swipe_points_internal(&serial, args.x1, args.y1, args.x2, args.y2, duration)
            .await?;
        let selector_postcondition = self
            .wait_for_tool_postcondition(ToolPostconditionRequest {
                serial: &serial,
                selector: wait_for_selector.as_ref(),
                match_index: None,
                wait_for_activity: None,
                wait_for_package: None,
                deadline,
                include_screenshot: false,
                artifact_prefix: "input-swipe-postcondition",
            })
            .await?;
        let scroll_changed = if let Some(pre) = pre_scroll_fingerprint {
            let observation = self
                .capture_ui_observation_with_deadline(
                    &serial,
                    filename_or_timestamp(None, "input-swipe-post", "xml"),
                    false,
                    filename_or_timestamp(None, "input-swipe-post", "png"),
                    Some(deadline),
                )
                .await?;
            let post = tap_verification_fingerprint(
                &observation.nodes,
                &UiSelector {
                    scrollable: Some(true),
                    ..UiSelector::default()
                },
                None,
                None,
            );
            pre != post
        } else {
            false
        };
        let postcondition_ok = selector_postcondition.satisfied || scroll_changed;
        ensure_action_outcome_satisfied(
            "android.input.swipe",
            "postcondition failed after dispatch",
            selector_postcondition.requested || args.expect_scroll_change,
            postcondition_ok,
            json!({
                "postcondition": tool_postcondition_json(&selector_postcondition),
                "scroll_changed": scroll_changed,
            }),
        )?;
        Ok(CallToolResult::structured(json!({
            "ok": postcondition_ok,
            "serial": serial,
            "duration_ms": duration,
            "backend_used": dispatch.backend,
            "dispatch_detail": dispatch.detail,
            "stdout": dispatch.output.stdout,
            "stderr": dispatch.output.stderr,
            "postcondition": tool_postcondition_json(&selector_postcondition),
            "scroll_changed": scroll_changed,
        })))
    }

    #[tool(
        name = "android.input.keyevent",
        description = "Send a keyevent to the device."
    )]
    async fn android_input_keyevent(
        &self,
        Parameters(args): Parameters<KeyeventArgs>,
    ) -> Result<CallToolResult, McpError> {
        let wait_for_selector = normalize_optional_selector_input(args.wait_for_selector);
        if let Some(selector) = wait_for_selector.as_ref() {
            ensure_selector_not_empty(selector)?;
        }
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let output = self
            .run_adb_shell(&serial, ["input", "keyevent", args.keycode.trim()])
            .await?;
        let deadline = tool_deadline(args.timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
        let postcondition = self
            .wait_for_tool_postcondition(ToolPostconditionRequest {
                serial: &serial,
                selector: wait_for_selector.as_ref(),
                match_index: None,
                wait_for_activity: args.wait_for_activity.as_deref(),
                wait_for_package: args.wait_for_package.as_deref(),
                deadline,
                include_screenshot: false,
                artifact_prefix: "input-keyevent-postcondition",
            })
            .await?;
        ensure_tool_postcondition_satisfied(
            "android.input.keyevent",
            "postcondition failed after dispatch",
            &postcondition,
        )?;
        Ok(CallToolResult::structured(json!({
            "ok": postcondition.satisfied,
            "serial": serial,
            "keycode": args.keycode,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "postcondition": tool_postcondition_json(&postcondition),
        })))
    }

    #[tool(
        name = "solarlab.scenario.stage_first_focus_earth",
        description = "Launch Solar Lab, open search, focus Earth, and capture step-by-step artifacts."
    )]
    async fn solarlab_scenario_stage_first_focus_earth(
        &self,
        Parameters(args): Parameters<SolarLabScenarioArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(
            self.run_stage_first_focus_earth(args).await?,
        ))
    }

    #[tool(
        name = "solarlab.scenario.stage_first_immersive_roundtrip",
        description = "Launch Solar Lab, open immersive view, return to sandbox, and capture artifacts."
    )]
    async fn solarlab_scenario_stage_first_immersive_roundtrip(
        &self,
        Parameters(args): Parameters<SolarLabScenarioArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(
            self.run_stage_first_immersive_roundtrip(args).await?,
        ))
    }

    #[tool(
        name = "solarlab.semantic_action",
        description = "Send a narrow semantic action into the Solar Lab app and optionally capture post-action screenshot and UI artifacts."
    )]
    async fn solarlab_semantic_action(
        &self,
        Parameters(args): Parameters<SolarLabSemanticActionArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(
            self.run_solarlab_semantic_action(args).await?,
        ))
    }
}

#[derive(Default)]
pub(crate) struct CommandOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

struct EmulatorLaunch {
    pid: Option<u32>,
    avd_name: String,
    grpc_port: Option<u16>,
    log_path: String,
    args: Vec<String>,
}

#[derive(Clone)]
struct LaunchRequest {
    avd_name: String,
    no_window: bool,
    gpu: Option<String>,
    grpc_port: Option<u16>,
    extra_args: Vec<String>,
}

struct BootReadiness {
    elapsed_ms: u128,
    package_manager: String,
}

struct ActionDispatch {
    backend: &'static str,
    output: CommandOutput,
    detail: Option<String>,
}

struct StableUiWaitResult {
    stabilized: bool,
    timed_out: bool,
    elapsed_ms: u128,
    stable_polls_observed: u32,
    stable_polls_required: u32,
    observation: UiObservation,
}

struct StableUiWaitRequest<'a> {
    serial: &'a str,
    deadline: Instant,
    poll_interval: Duration,
    required_stable_polls: u32,
    hierarchy_filename: String,
    include_screenshot: bool,
    screenshot_filename: String,
}

struct VerifiedTextDispatch {
    dispatch: ActionDispatch,
    verification: TextVerification,
}

struct SolarLabAppArgs {
    package_name: String,
    activity: Option<String>,
}

struct ScenarioBundleSpec<'a> {
    scenario_slug: &'a str,
    scenario_name: &'a str,
    logcat_filename: &'a str,
}

pub(crate) fn tool_deadline(timeout_secs: Option<u64>, default_secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(timeout_secs.unwrap_or(default_secs))
}

pub(crate) fn remaining_until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn require_remaining_budget(deadline: Instant, label: &str) -> Result<Duration, McpError> {
    let remaining = remaining_until(deadline);
    if remaining.is_zero() {
        Err(McpError::internal_error(
            format!("{label} timed out before the next verification step could begin"),
            None,
        ))
    } else {
        Ok(remaining)
    }
}

pub(crate) fn has_meaningful_observation_budget(deadline: Instant) -> bool {
    remaining_until(deadline) >= Duration::from_millis(MIN_OBSERVATION_BUDGET_MS)
}

pub(crate) fn has_meaningful_fast_ui_fingerprint_budget(deadline: Instant) -> bool {
    remaining_until(deadline)
        >= Duration::from_millis(
            FULL_OBSERVATION_CAPTURE_RESERVE_MS + MIN_FAST_UI_FINGERPRINT_BUDGET_MS,
        )
}

fn bounded_deadline(deadline: Instant, soft_budget: Duration) -> Instant {
    let soft_deadline = Instant::now() + soft_budget;
    if soft_deadline < deadline {
        soft_deadline
    } else {
        deadline
    }
}

fn reserved_fallback_deadline(deadline: Instant, reserve_budget: Duration) -> Instant {
    let now = Instant::now();
    let remaining = remaining_until(deadline);
    if remaining <= reserve_budget {
        deadline
    } else {
        now + (remaining - reserve_budget)
    }
}

fn initial_text_verification_deadline(
    dispatch_backend: &str,
    deadline: Instant,
    retry_with_adb_on_no_change: bool,
) -> Instant {
    if dispatch_backend != "grpc" || !retry_with_adb_on_no_change {
        return deadline;
    }

    let reserve_budget = Duration::from_millis(ADB_TEXT_FALLBACK_RESERVE_MS);
    let minimum_verification_window = Duration::from_millis(MIN_TEXT_VERIFICATION_WINDOW_MS);
    let remaining = remaining_until(deadline);

    if remaining <= reserve_budget + minimum_verification_window {
        return deadline;
    }

    bounded_deadline(
        deadline,
        Duration::from_millis(GRPC_TEXT_VERIFICATION_BUDGET_MS),
    )
    .max(reserved_fallback_deadline(deadline, reserve_budget))
}

fn has_meaningful_text_fallback_budget(deadline: Instant) -> bool {
    remaining_until(deadline) >= Duration::from_millis(ADB_TEXT_FALLBACK_RESERVE_MS)
}

fn fast_ui_fingerprint_deadline(deadline: Instant) -> Instant {
    bounded_deadline(
        reserved_fallback_deadline(
            deadline,
            Duration::from_millis(FULL_OBSERVATION_CAPTURE_RESERVE_MS),
        ),
        Duration::from_millis(MIN_FAST_UI_FINGERPRINT_BUDGET_MS),
    )
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AndroidWindowState {
    pub(crate) current_focus: Option<String>,
    pub(crate) focused_app: Option<String>,
    pub(crate) resumed_activity: Option<String>,
}

pub(crate) struct UiObservation {
    pub(crate) hierarchy_path: PathBuf,
    pub(crate) hierarchy_xml: String,
    pub(crate) nodes: Vec<NormalizedUiNode>,
    pub(crate) screenshot_path: Option<PathBuf>,
    pub(crate) screenshot_backend: Option<&'static str>,
    pub(crate) window_state: AndroidWindowState,
}

#[derive(Debug, Clone, Serialize)]
struct SystemDialogReport {
    detected: bool,
    kind: String,
    labels: Vec<String>,
    action_taken: Option<String>,
    artifact_path: String,
}

enum SystemDialogCheck {
    None,
    Handled(SystemDialogReport),
    Blocked(SystemDialogReport),
}

struct SystemDialogPlan {
    kind: &'static str,
    action_label: Option<&'static str>,
}

impl AndroidEmulatorMcp {
    pub async fn run_solarlab_scenario(
        &self,
        name: &str,
        serial: Option<&str>,
        package_name: Option<&str>,
        activity: Option<&str>,
    ) -> Result<serde_json::Value, McpError> {
        let args = SolarLabScenarioArgs {
            serial: serial.map(str::to_string),
            package_name: package_name.map(str::to_string),
            activity: activity.map(str::to_string),
        };
        let result = match name {
            "stage_first_focus_earth" => self.run_stage_first_focus_earth(args).await?,
            "stage_first_immersive_roundtrip" => {
                self.run_stage_first_immersive_roundtrip(args).await?
            }
            other => {
                return Err(McpError::invalid_params(
                    format!(
                        "unknown scenario '{other}', expected one of: stage_first_focus_earth, stage_first_immersive_roundtrip"
                    ),
                    None,
                ));
            }
        };
        Ok(result)
    }

    pub async fn run_solarlab_semantic_action(
        &self,
        args: SolarLabSemanticActionArgs,
    ) -> Result<serde_json::Value, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let app = SolarLabAppArgs {
            package_name: args
                .package_name
                .unwrap_or_else(|| "com.sednalabs.solarlab".to_string()),
            activity: Some(args.activity.unwrap_or_else(|| ".MainActivity".to_string())),
        };
        let body_query = args
            .body_query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_string);
        let action = validate_solarlab_semantic_action(&args.action, body_query)?;

        self.dispatch_solarlab_semantic_action(
            &serial,
            &app,
            action.action_name(),
            action.body_query(),
        )
        .await?;

        let acknowledgment = self
            .wait_for_solarlab_semantic_ack(&serial, &action)
            .await?;
        if let Some(ack) = acknowledgment.as_ref().filter(|ack| !ack.acknowledged) {
            let artifacts = if args.capture_state {
                Some(
                    self.capture_solarlab_artifacts(
                        &serial,
                        "solarlab-semantic-action-failed",
                        action.action_name(),
                    )
                    .await?,
                )
            } else {
                None
            };
            return Err(McpError::internal_error(
                format!(
                    "Solar Lab semantic action '{}' did not reach acknowledged UI state '{}'. Last UI dump: {}. Failure artifacts: {}",
                    action.action_name(),
                    ack.matcher,
                    ack.observed_ui_dump,
                    artifacts
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "<not captured>".to_string()),
                ),
                None,
            ));
        }

        let artifacts = if args.capture_state {
            Some(
                self.capture_solarlab_artifacts(
                    &serial,
                    "solarlab-semantic-action",
                    action.action_name(),
                )
                .await?,
            )
        } else {
            None
        };

        Ok(json!({
            "ok": true,
            "serial": serial,
            "package_name": app.package_name,
            "activity": app.activity,
            "action": action.action_name(),
            "body_query": action.body_query(),
            "acknowledgment": acknowledgment,
            "artifacts": artifacts,
        }))
    }

    async fn list_avds_internal(&self) -> Result<Vec<String>, McpError> {
        let mut command = Command::new(&self.config.emulator_path);
        command.arg("-list-avds");
        let output = run_command_with_timeout(
            command,
            Duration::from_secs(DEFAULT_EMULATOR_COMMAND_TIMEOUT_SECS),
            "emulator -list-avds",
        )
        .await?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }

    async fn list_devices_internal(
        &self,
        serial_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, McpError> {
        let output = self
            .run_adb_raw(["devices".to_string(), "-l".to_string()])
            .await?;
        let devices = output
            .stdout
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter_map(parse_adb_device_line)
            .filter(|device| {
                serial_filter
                    .map(|wanted| device["serial"].as_str() == Some(wanted))
                    .unwrap_or(true)
            })
            .collect();
        Ok(devices)
    }

    pub(crate) async fn resolve_serial_for_tools(
        &self,
        serial: Option<&str>,
    ) -> Result<String, McpError> {
        if let Some(explicit) = serial.map(str::trim).filter(|value| !value.is_empty()) {
            return Ok(explicit.to_string());
        }
        let devices = self.list_devices_internal(None).await?;
        resolve_serial_from_devices(None, &devices)
    }

    async fn spawn_avd_process(&self, request: LaunchRequest) -> Result<EmulatorLaunch, McpError> {
        if request.avd_name.trim().is_empty() {
            return Err(McpError::invalid_params("avd_name must not be empty", None));
        }

        let emulator_args = request.emulator_args(self.config.emulator_grpc_port);
        let log_path = self
            .artifact_path("emulator", Some("log".to_string()), Some("txt"))
            .await?;
        let pid = if should_isolate_emulator_launch(systemd_run_is_available()) {
            self.launch_avd_via_systemd_scope(&request, &emulator_args, &log_path)
                .await?;
            None
        } else {
            let stdout = std::fs::File::create(&log_path)
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
            let stderr = stdout
                .try_clone()
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;

            let mut command = self.build_emulator_launch_command(&emulator_args);
            command
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .stdin(Stdio::null());
            command
                .spawn()
                .map_err(|err| McpError::internal_error(err.to_string(), None))?
                .id()
        };
        Ok(EmulatorLaunch {
            pid,
            avd_name: request.avd_name,
            grpc_port: request.grpc_port.or(self.config.emulator_grpc_port),
            log_path: log_path.display().to_string(),
            args: emulator_args,
        })
    }

    async fn wait_for_new_emulator_serial(
        &self,
        pre_launch_ready_serials: &[String],
        expected_serial: Option<&str>,
        timeout: Duration,
    ) -> Result<String, McpError> {
        let started = std::time::Instant::now();
        while started.elapsed() <= timeout {
            let devices = self.list_devices_internal(None).await?;
            match select_emulator_serial_after_launch(
                pre_launch_ready_serials,
                &devices,
                expected_serial,
            )? {
                Some(serial) => return Ok(serial),
                None => sleep(Duration::from_secs(2)).await,
            }
        }

        let expectation = expected_serial
            .map(|serial| format!("expected emulator serial {serial}"))
            .unwrap_or_else(|| "a newly launched emulator serial".to_string());
        Err(McpError::internal_error(
            format!("did not observe {expectation} within {:?}", timeout),
            None,
        ))
    }

    async fn wait_for_boot_readiness(
        &self,
        serial: &str,
        timeout: Duration,
    ) -> Result<BootReadiness, McpError> {
        self.run_adb(serial, ["wait-for-device"]).await?;

        let started = std::time::Instant::now();
        while started.elapsed() <= timeout {
            let boot = self
                .adb_shell_output(serial, ["getprop", "sys.boot_completed"])
                .await?;
            if boot.trim() == "1" {
                let pm = self
                    .adb_shell_output(serial, ["pm", "path", "android"])
                    .await?;
                if pm.trim().starts_with("package:") {
                    return Ok(BootReadiness {
                        elapsed_ms: started.elapsed().as_millis(),
                        package_manager: pm.trim().to_string(),
                    });
                }
            }
            sleep(Duration::from_secs(2)).await;
        }

        Err(McpError::internal_error(
            format!(
                "device {serial} did not reach boot readiness within {:?}",
                timeout
            ),
            None,
        ))
    }

    async fn launch_avd_via_systemd_scope(
        &self,
        request: &LaunchRequest,
        emulator_args: &[String],
        log_path: &Path,
    ) -> Result<(), McpError> {
        let unit_name = format!(
            "android-emulator-avd-{}",
            timestamp_filename(&sanitize_filename(&request.avd_name), None)
        );
        let absolute_log_path = absolute_path(log_path)?;
        std::fs::File::create(&absolute_log_path)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let mut command = Command::new("systemd-run");
        command.args([
            "--user",
            "--quiet",
            "--no-block",
            "--same-dir",
            "--collect",
            "--unit",
            &unit_name,
            "--property",
            &format!("StandardOutput=append:{}", absolute_log_path.display()),
            "--property",
            &format!("StandardError=append:{}", absolute_log_path.display()),
        ]);
        if self.config.use_sg_kvm {
            command
                .arg("sg")
                .arg("kvm")
                .arg("-c")
                .arg(shell_quote_command(
                    &self.config.emulator_path,
                    emulator_args,
                ));
        } else {
            command.arg(&self.config.emulator_path).args(emulator_args);
        }
        run_command_with_timeout(
            command,
            Duration::from_secs(DEFAULT_EMULATOR_COMMAND_TIMEOUT_SECS),
            "systemd-run emulator service",
        )
        .await?;
        Ok(())
    }

    fn grpc_endpoint_for_serial(&self, serial: &str) -> Option<discovery::GrpcEndpoint> {
        if !serial.starts_with("emulator-") {
            return None;
        }

        let discovered = discovery::grpc_endpoint_for_serial(serial);
        match (self.config.emulator_grpc_port, discovered) {
            (Some(port), Some(endpoint)) => Some(discovery::GrpcEndpoint {
                port,
                auth_token: endpoint.auth_token,
            }),
            (Some(port), None) => Some(discovery::GrpcEndpoint {
                port,
                auth_token: None,
            }),
            (None, discovered) => discovered,
        }
    }

    fn build_emulator_launch_command(&self, emulator_args: &[String]) -> Command {
        if self.config.use_sg_kvm {
            let mut command = Command::new("sg");
            command.arg("kvm").arg("-c").arg(shell_quote_command(
                &self.config.emulator_path,
                emulator_args,
            ));
            command
        } else {
            let mut command = Command::new(&self.config.emulator_path);
            command.args(emulator_args);
            command
        }
    }

    pub(crate) async fn run_adb<I, S>(
        &self,
        serial: &str,
        args: I,
    ) -> Result<CommandOutput, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.run_adb_with_timeout(
            serial,
            args,
            Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS),
        )
        .await
    }

    async fn run_adb_with_timeout<I, S>(
        &self,
        serial: &str,
        args: I,
        timeout_duration: Duration,
    ) -> Result<CommandOutput, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut argv = vec!["-s".to_string(), serial.to_string()];
        argv.extend(args.into_iter().map(|value| value.as_ref().to_string()));
        self.run_adb_raw_with_timeout(argv, timeout_duration).await
    }

    async fn run_adb_raw<I, S>(&self, args: I) -> Result<CommandOutput, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.run_adb_raw_with_timeout(args, Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS))
            .await
    }

    async fn run_adb_raw_with_timeout<I, S>(
        &self,
        args: I,
        timeout_duration: Duration,
    ) -> Result<CommandOutput, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let argv = args
            .into_iter()
            .map(|value| value.as_ref().to_string())
            .collect::<Vec<_>>();
        let mut command = Command::new(&self.config.adb_path);
        command.args(&argv);
        let output = run_command_with_timeout(
            command,
            timeout_duration,
            &format!("adb {}", argv.join(" ")),
        )
        .await?;
        Ok(command_output_from_process(output))
    }

    pub(crate) async fn run_adb_shell<I, S>(
        &self,
        serial: &str,
        args: I,
    ) -> Result<CommandOutput, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.run_adb_shell_with_timeout(
            serial,
            args,
            Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS),
        )
        .await
    }

    async fn run_adb_shell_with_timeout<I, S>(
        &self,
        serial: &str,
        args: I,
        timeout_duration: Duration,
    ) -> Result<CommandOutput, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut argv = vec!["shell".to_string()];
        argv.extend(args.into_iter().map(|value| value.as_ref().to_string()));
        self.run_adb_with_timeout(serial, argv, timeout_duration)
            .await
    }

    async fn adb_shell_output<I, S>(&self, serial: &str, args: I) -> Result<String, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.adb_shell_output_with_timeout(
            serial,
            args,
            Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS),
        )
        .await
    }

    async fn adb_shell_output_with_timeout<I, S>(
        &self,
        serial: &str,
        args: I,
        timeout_duration: Duration,
    ) -> Result<String, McpError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Ok(self
            .run_adb_shell_with_timeout(serial, args, timeout_duration)
            .await?
            .stdout)
    }

    async fn window_state_internal(&self, serial: &str) -> Result<AndroidWindowState, McpError> {
        self.window_state_internal_with_timeout(
            serial,
            Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS),
        )
        .await
    }

    async fn window_state_internal_with_timeout(
        &self,
        serial: &str,
        timeout_duration: Duration,
    ) -> Result<AndroidWindowState, McpError> {
        let window_output = self
            .adb_shell_output_with_timeout(
                serial,
                ["dumpsys", "window", "windows"],
                timeout_duration,
            )
            .await?;
        let activity_output = self
            .adb_shell_output_with_timeout(
                serial,
                ["dumpsys", "activity", "activities"],
                timeout_duration,
            )
            .await?;
        Ok(parse_android_window_state(&window_output, &activity_output))
    }

    async fn current_focus_internal(&self, serial: &str) -> Result<Option<String>, McpError> {
        Ok(self.window_state_internal(serial).await?.current_focus)
    }

    async fn display_size_internal(&self, serial: &str) -> Result<(u32, u32), McpError> {
        let output = self.adb_shell_output(serial, ["wm", "size"]).await?;
        parse_display_size(&output).ok_or_else(|| {
            McpError::internal_error(
                format!(
                    "failed to parse display size from `wm size`: {}",
                    output.trim()
                ),
                None,
            )
        })
    }

    async fn tap_point_internal(
        &self,
        serial: &str,
        x: u32,
        y: u32,
    ) -> Result<ActionDispatch, McpError> {
        if let Some(endpoint) = self.grpc_endpoint_for_serial(serial) {
            match grpc_backend::send_tap(endpoint.port, endpoint.auth_token.as_deref(), x, y).await
            {
                Ok(()) => {
                    return Ok(ActionDispatch {
                        backend: "grpc",
                        output: CommandOutput::default(),
                        detail: None,
                    });
                }
                Err(err) => {
                    let output = self
                        .run_adb_shell(serial, ["input", "tap", &x.to_string(), &y.to_string()])
                        .await?;
                    return Ok(ActionDispatch {
                        backend: "adb",
                        output,
                        detail: Some(format!(
                            "gRPC tap transport failed, fell back to adb: {err}"
                        )),
                    });
                }
            }
        }

        let output = self
            .run_adb_shell(serial, ["input", "tap", &x.to_string(), &y.to_string()])
            .await?;
        Ok(ActionDispatch {
            backend: "adb",
            output,
            detail: None,
        })
    }

    async fn tap_point_internal_adb(
        &self,
        serial: &str,
        x: u32,
        y: u32,
    ) -> Result<ActionDispatch, McpError> {
        let output = self
            .run_adb_shell(serial, ["input", "tap", &x.to_string(), &y.to_string()])
            .await?;
        Ok(ActionDispatch {
            backend: "adb",
            output,
            detail: None,
        })
    }

    async fn send_text_with_verification(
        &self,
        request: VerifiedTextDispatchRequest<'_>,
    ) -> Result<VerifiedTextDispatch, McpError> {
        let VerifiedTextDispatchRequest {
            serial,
            text,
            pre_text_nodes,
            public_target_selector,
            target_tracker,
            target_selector,
            wait_for_selector,
            deadline,
            hierarchy_filename,
            retry_with_adb_on_no_change,
            replace_existing_text_on_adb,
            existing_text_for_adb_replace,
        } = request;
        let dispatch = if replace_existing_text_on_adb {
            self.send_text_replacing_via_adb_internal(serial, text, existing_text_for_adb_replace)
                .await?
        } else {
            self.send_text_internal(serial, text).await?
        };
        let initial_verification_deadline = initial_text_verification_deadline(
            dispatch.backend,
            deadline,
            retry_with_adb_on_no_change,
        );
        let verification = self
            .verify_text_outcome(TextVerificationRequest {
                serial,
                pre_text_nodes,
                public_target_selector,
                target_tracker,
                target_selector,
                expected_text: Some(text),
                wait_for_selector,
                deadline: initial_verification_deadline,
                hierarchy_filename: hierarchy_filename.clone(),
            })
            .await?;
        if should_retry_text_with_adb(dispatch.backend, retry_with_adb_on_no_change, &verification)
        {
            if !has_meaningful_text_fallback_budget(deadline) {
                let remaining_ms = remaining_until(deadline).as_millis();
                return Ok(VerifiedTextDispatch {
                    dispatch: ActionDispatch {
                        backend: dispatch.backend,
                        output: dispatch.output,
                        detail: Some(format!(
                            "gRPC text transport did not produce a verified UI change, but adb fallback was skipped because only {remaining_ms} ms remained: {}",
                            text_verification_summary(&verification)
                        )),
                    },
                    verification,
                });
            }
            require_remaining_budget(deadline, "android.input.text adb fallback")?;
            let retry_dispatch = self.send_text_via_adb_internal(serial, text).await?;
            let retry_verification = self
                .verify_text_outcome(TextVerificationRequest {
                    serial,
                    pre_text_nodes,
                    public_target_selector,
                    target_tracker,
                    target_selector,
                    expected_text: Some(text),
                    wait_for_selector,
                    deadline,
                    hierarchy_filename,
                })
                .await?;
            return Ok(VerifiedTextDispatch {
                dispatch: ActionDispatch {
                    backend: retry_dispatch.backend,
                    output: retry_dispatch.output,
                    detail: Some(format!(
                        "gRPC text transport did not produce a verified UI change, fell back to adb: {}",
                        text_verification_summary(&verification)
                    )),
                },
                verification: retry_verification,
            });
        }
        Ok(VerifiedTextDispatch {
            dispatch,
            verification,
        })
    }

    async fn wait_for_stable_ui_internal(
        &self,
        request: StableUiWaitRequest<'_>,
    ) -> Result<StableUiWaitResult, McpError> {
        let StableUiWaitRequest {
            serial,
            deadline,
            poll_interval,
            required_stable_polls,
            hierarchy_filename,
            include_screenshot,
            screenshot_filename,
        } = request;
        let started = Instant::now();
        let mut last_fingerprint: Option<String> = None;
        let mut last_fast_fingerprint: Option<String> = None;
        let mut use_fast_stability_backend: Option<bool> = None;
        let mut stable_polls = 0u32;
        let mut final_observation: Option<UiObservation> = None;

        while Instant::now() < deadline {
            if final_observation.is_some() && !has_meaningful_observation_budget(deadline) {
                break;
            }
            let final_poll = remaining_until(deadline) <= poll_interval;
            let fast_fingerprint = if use_fast_stability_backend == Some(false) {
                None
            } else {
                self.capture_fast_ui_fingerprint_until_deadline(serial, deadline, None)
                    .await?
            };
            if use_fast_stability_backend.is_none() {
                use_fast_stability_backend = Some(fast_fingerprint.is_some());
            }
            if use_fast_stability_backend == Some(true) {
                if let Some(fast_fingerprint) = fast_fingerprint {
                    if last_fast_fingerprint.as_deref() == Some(fast_fingerprint.as_str()) {
                        stable_polls += 1;
                    } else {
                        stable_polls = 1;
                        last_fast_fingerprint = Some(fast_fingerprint);
                    }
                } else {
                    use_fast_stability_backend = Some(false);
                }
                if use_fast_stability_backend == Some(true)
                    && stable_polls < required_stable_polls
                    && !final_poll
                {
                    sleep(poll_interval).await;
                    continue;
                }
            }
            let Some(observation) = self
                .capture_ui_observation_until_deadline(
                    serial,
                    hierarchy_filename.clone(),
                    include_screenshot,
                    screenshot_filename.clone(),
                    deadline,
                )
                .await?
            else {
                break;
            };
            let fingerprint =
                ui_observation_fingerprint(&observation.hierarchy_xml, &observation.window_state);
            if use_fast_stability_backend == Some(false) {
                if last_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                    stable_polls += 1;
                } else {
                    stable_polls = 1;
                    last_fingerprint = Some(fingerprint);
                }
            }
            final_observation = Some(observation);
            if stable_polls >= required_stable_polls {
                break;
            }
            if final_poll {
                break;
            }
            sleep(poll_interval).await;
        }

        let observation = final_observation.ok_or_else(|| {
            McpError::internal_error(
                "android.wait_for_stable_ui timed out before a UI observation could be captured",
                None,
            )
        })?;
        let stabilized = stable_polls >= required_stable_polls;
        Ok(StableUiWaitResult {
            stabilized,
            timed_out: !stabilized,
            elapsed_ms: started.elapsed().as_millis(),
            stable_polls_observed: stable_polls,
            stable_polls_required: required_stable_polls,
            observation,
        })
    }

    async fn send_text_internal(
        &self,
        serial: &str,
        text: &str,
    ) -> Result<ActionDispatch, McpError> {
        if let Some(endpoint) = self.grpc_endpoint_for_serial(serial) {
            match grpc_backend::send_text(endpoint.port, endpoint.auth_token.as_deref(), text).await
            {
                Ok(()) => {
                    return Ok(ActionDispatch {
                        backend: "grpc",
                        output: CommandOutput::default(),
                        detail: None,
                    });
                }
                Err(err) => {
                    let fallback = self.send_text_via_adb_internal(serial, text).await?;
                    return Ok(ActionDispatch {
                        backend: fallback.backend,
                        output: fallback.output,
                        detail: Some(format!(
                            "gRPC text transport failed, fell back to adb: {err}"
                        )),
                    });
                }
            }
        }

        self.send_text_via_adb_internal(serial, text).await
    }

    async fn send_text_via_adb_internal(
        &self,
        serial: &str,
        text: &str,
    ) -> Result<ActionDispatch, McpError> {
        let output = self
            .run_adb_shell(serial, ["input", "text", &escape_adb_text(text)])
            .await?;
        Ok(ActionDispatch {
            backend: "adb",
            output,
            detail: None,
        })
    }

    async fn send_text_replacing_via_adb_internal(
        &self,
        serial: &str,
        text: &str,
        existing_text: Option<&str>,
    ) -> Result<ActionDispatch, McpError> {
        let selection_output = match self
            .run_adb_shell(serial, ["input", "keycombination", "CTRL_LEFT", "A"])
            .await
        {
            Ok(output) => output,
            Err(selection_error) => {
                self.clear_focused_text_via_delete_fallback(serial, existing_text)
                    .await?;
                let replacement_output = self
                    .send_replacement_text_after_selection(serial, text)
                    .await?;
                return Ok(ActionDispatch {
                    backend: replacement_output.backend,
                    output: replacement_output.output,
                    detail: Some(merge_dispatch_detail(
                        format!(
                            "replaced focused field contents via adb delete fallback after select-all failed: {selection_error}"
                        ),
                        replacement_output.detail.as_deref(),
                    )),
                });
            }
        };

        let replacement_output = self
            .send_replacement_text_after_selection(serial, text)
            .await?;
        Ok(ActionDispatch {
            backend: replacement_output.backend,
            output: merge_command_outputs(selection_output, replacement_output.output),
            detail: Some(merge_dispatch_detail(
                "replaced focused field contents via adb select-all".to_string(),
                replacement_output.detail.as_deref(),
            )),
        })
    }

    async fn clear_focused_text_via_delete_fallback(
        &self,
        serial: &str,
        existing_text: Option<&str>,
    ) -> Result<(), McpError> {
        self.run_adb_shell(serial, ["input", "keyevent", "KEYCODE_MOVE_END"])
            .await?;
        for _ in 0..fallback_clear_delete_count(existing_text) {
            self.run_adb_shell(serial, ["input", "keyevent", "KEYCODE_DEL"])
                .await?;
        }
        Ok(())
    }

    async fn send_replacement_text_after_selection(
        &self,
        serial: &str,
        text: &str,
    ) -> Result<ActionDispatch, McpError> {
        match replacement_text_dispatch_plan(text) {
            ReplacementTextDispatchPlan::GenericTextDispatch => {
                self.send_text_internal(serial, text).await
            }
            ReplacementTextDispatchPlan::RawAdbStep(replacement_step) => {
                let output = self.run_adb_shell(serial, replacement_step).await?;
                Ok(ActionDispatch {
                    backend: "adb",
                    output,
                    detail: None,
                })
            }
        }
    }

    async fn swipe_points_internal(
        &self,
        serial: &str,
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        duration_ms: u64,
    ) -> Result<ActionDispatch, McpError> {
        if let Some(endpoint) = self.grpc_endpoint_for_serial(serial) {
            match grpc_backend::send_swipe(
                endpoint.port,
                endpoint.auth_token.as_deref(),
                x1,
                y1,
                x2,
                y2,
                duration_ms,
            )
            .await
            {
                Ok(()) => {
                    return Ok(ActionDispatch {
                        backend: "grpc",
                        output: CommandOutput::default(),
                        detail: None,
                    });
                }
                Err(err) => {
                    let output = self
                        .run_adb_shell(
                            serial,
                            [
                                "input",
                                "swipe",
                                &x1.to_string(),
                                &y1.to_string(),
                                &x2.to_string(),
                                &y2.to_string(),
                                &duration_ms.to_string(),
                            ],
                        )
                        .await?;
                    return Ok(ActionDispatch {
                        backend: "adb",
                        output,
                        detail: Some(format!(
                            "gRPC swipe transport failed, fell back to adb: {err}"
                        )),
                    });
                }
            }
        }

        let output = self
            .run_adb_shell(
                serial,
                [
                    "input",
                    "swipe",
                    &x1.to_string(),
                    &y1.to_string(),
                    &x2.to_string(),
                    &y2.to_string(),
                    &duration_ms.to_string(),
                ],
            )
            .await?;
        Ok(ActionDispatch {
            backend: "adb",
            output,
            detail: None,
        })
    }

    async fn artifact_path(
        &self,
        group: &str,
        filename: Option<String>,
        extension: Option<&str>,
    ) -> Result<PathBuf, McpError> {
        let dir = self.config.artifact_dir.join(group);
        fs::create_dir_all(&dir)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let name = filename
            .as_deref()
            .and_then(|value| normalize_artifact_name(value, extension))
            .unwrap_or_else(|| timestamp_filename(group, extension));
        Ok(dir.join(name))
    }

    async fn launch_solarlab_app(
        &self,
        serial: &str,
        app: &SolarLabAppArgs,
    ) -> Result<(), McpError> {
        self.run_adb_shell(serial, ["am", "force-stop", app.package_name.as_str()])
            .await
            .ok();
        if let Some(activity) = app.activity.as_deref() {
            self.run_adb_shell(
                serial,
                [
                    "am",
                    "start",
                    "-n",
                    &format!("{}/{}", app.package_name, activity),
                ],
            )
            .await?;
        } else {
            self.run_adb_shell(
                serial,
                [
                    "monkey",
                    "-p",
                    app.package_name.as_str(),
                    "-c",
                    "android.intent.category.LAUNCHER",
                    "1",
                ],
            )
            .await?;
        }
        sleep(Duration::from_secs(2)).await;
        Ok(())
    }

    async fn dispatch_solarlab_semantic_action(
        &self,
        serial: &str,
        app: &SolarLabAppArgs,
        action: &str,
        body_query: Option<&str>,
    ) -> Result<(), McpError> {
        let mut adb_args = vec![
            "am".to_string(),
            "start".to_string(),
            "-n".to_string(),
            format!(
                "{}/{}",
                app.package_name,
                app.activity.as_deref().unwrap_or(".MainActivity")
            ),
            "-a".to_string(),
            "com.sednalabs.solarlab.action.SEMANTIC_CONTROL".to_string(),
            "--es".to_string(),
            "com.sednalabs.solarlab.extra.SEMANTIC_COMMAND".to_string(),
            action.to_string(),
        ];
        if let Some(body_query) = body_query {
            adb_args.extend([
                "--es".to_string(),
                "com.sednalabs.solarlab.extra.BODY_QUERY".to_string(),
                body_query.to_string(),
            ]);
        }
        self.run_adb_shell(serial, adb_args).await?;
        sleep(Duration::from_millis(1500)).await;
        Ok(())
    }

    async fn wait_for_solarlab_semantic_ack(
        &self,
        serial: &str,
        action: &SolarLabSemanticCommand,
    ) -> Result<Option<SolarLabSemanticAck>, McpError> {
        let matcher = match action {
            SolarLabSemanticCommand::ResetCamera => return Ok(None),
            _ => matcher_for_action(action),
        };
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(DEFAULT_SOLARLAB_ACK_TIMEOUT_SECS);
        let filename = format!(
            "solarlab-semantic-ack-{}.xml",
            sanitize_filename(action.action_name()),
        );
        loop {
            let path = self
                .dump_ui_hierarchy_internal(serial, filename.clone())
                .await?;
            let hierarchy = fs::read_to_string(&path)
                .await
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
            let observed_ui_dump = path.display().to_string();
            if solarlab_semantic_ack_matches(action, &hierarchy) {
                return Ok(Some(SolarLabSemanticAck {
                    acknowledged: true,
                    matcher: matcher.description.clone(),
                    observed_ui_dump,
                }));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(Some(SolarLabSemanticAck {
                    acknowledged: false,
                    matcher: matcher.description,
                    observed_ui_dump,
                }));
            }
            sleep(Duration::from_millis(DEFAULT_SOLARLAB_ACK_POLL_MS)).await;
        }
    }

    async fn capture_solarlab_artifacts(
        &self,
        serial: &str,
        scenario: &str,
        step: &str,
    ) -> Result<serde_json::Value, McpError> {
        let (screenshot, screenshot_backend) = self
            .capture_screenshot_internal(serial, format!("{scenario}-{step}.png"))
            .await?;
        let ui_dump = self
            .dump_ui_hierarchy_internal(serial, format!("{scenario}-{step}.xml"))
            .await?;
        Ok(json!({
            "step": step,
            "screenshot": screenshot.display().to_string(),
            "screenshot_backend": screenshot_backend,
            "ui_dump": ui_dump.display().to_string(),
        }))
    }

    async fn collect_logcat_internal(
        &self,
        serial: &str,
        filename: String,
        lines: u32,
    ) -> Result<PathBuf, McpError> {
        let output = self
            .run_adb(
                serial,
                [
                    "logcat".to_string(),
                    "-d".to_string(),
                    "-t".to_string(),
                    lines.to_string(),
                ],
            )
            .await?;
        let path = self
            .artifact_path("logcat", Some(filename), Some("txt"))
            .await?;
        fs::write(&path, &output.stdout)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        Ok(path)
    }

    async fn write_scenario_manifest(
        &self,
        scenario: &str,
        manifest: &serde_json::Value,
    ) -> Result<(PathBuf, PathBuf), McpError> {
        let bundle_dir = self
            .config
            .artifact_dir
            .join("scenario-bundles")
            .join(timestamp_filename(&sanitize_filename(scenario), None));
        fs::create_dir_all(&bundle_dir)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let manifest_path = bundle_dir.join("manifest.json");
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        fs::write(&manifest_path, bytes)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        Ok((bundle_dir, manifest_path))
    }

    async fn finish_solarlab_scenario(
        &self,
        spec: ScenarioBundleSpec<'_>,
        serial: &str,
        app: &SolarLabAppArgs,
        artifacts: Vec<serde_json::Value>,
        outcome: Result<(), McpError>,
    ) -> Result<serde_json::Value, McpError> {
        let error_message = outcome.as_ref().err().map(ToString::to_string);
        let logcat_path = self
            .collect_logcat_internal(serial, spec.logcat_filename.to_string(), 400)
            .await
            .ok();
        let manifest = json!({
            "scenario": spec.scenario_name,
            "status": if error_message.is_some() { "failed" } else { "ok" },
            "serial": serial,
            "package_name": app.package_name,
            "activity": app.activity,
            "artifacts": artifacts,
            "logcat": logcat_path.as_ref().map(|path| path.display().to_string()),
            "error": error_message,
        });
        let (bundle_dir, manifest_path) = self
            .write_scenario_manifest(spec.scenario_slug, &manifest)
            .await?;

        match outcome {
            Ok(()) => Ok(json!({
                "ok": true,
                "serial": serial,
                "scenario": spec.scenario_name,
                "package_name": app.package_name,
                "activity": app.activity,
                "artifacts": manifest["artifacts"].clone(),
                "logcat": manifest["logcat"].clone(),
                "bundle_dir": bundle_dir.display().to_string(),
                "manifest_path": manifest_path.display().to_string(),
            })),
            Err(err) => Err(McpError::internal_error(
                format!(
                    "{}; partial scenario manifest: {}",
                    err,
                    manifest_path.display()
                ),
                None,
            )),
        }
    }

    async fn capture_screenshot_internal(
        &self,
        serial: &str,
        filename: String,
    ) -> Result<(PathBuf, &'static str), McpError> {
        self.capture_screenshot_internal_with_timeout(
            serial,
            filename,
            Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS),
        )
        .await
    }

    async fn capture_screenshot_internal_with_timeout(
        &self,
        serial: &str,
        filename: String,
        timeout_duration: Duration,
    ) -> Result<(PathBuf, &'static str), McpError> {
        let path = self
            .artifact_path("screenshots", Some(filename), Some("png"))
            .await?;
        if let Some(endpoint) = self.grpc_endpoint_for_serial(serial)
            && let Ok(png_bytes) =
                grpc_backend::capture_screenshot_png(endpoint.port, endpoint.auth_token.as_deref())
                    .await
        {
            fs::write(&path, png_bytes)
                .await
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
            return Ok((path, "grpc"));
        }
        let mut command = Command::new(&self.config.adb_path);
        command
            .arg("-s")
            .arg(serial)
            .args(["exec-out", "screencap", "-p"]);
        let output = run_command_with_timeout(
            command,
            timeout_duration,
            &format!("adb -s {serial} exec-out screencap -p"),
        )
        .await?;
        fs::write(&path, output.stdout)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        Ok((path, "adb"))
    }

    async fn dump_ui_hierarchy_internal(
        &self,
        serial: &str,
        filename: String,
    ) -> Result<PathBuf, McpError> {
        self.dump_ui_hierarchy_internal_with_deadline(serial, filename, None)
            .await
    }

    async fn dump_ui_hierarchy_internal_with_deadline(
        &self,
        serial: &str,
        filename: String,
        deadline: Option<Instant>,
    ) -> Result<PathBuf, McpError> {
        let local_path = self
            .artifact_path("uiautomator", Some(filename), Some("xml"))
            .await?;
        let exec_out_timeout = deadline
            .map(|deadline| require_remaining_budget(deadline, "UI hierarchy dump"))
            .transpose()?
            .unwrap_or(Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS));

        match self
            .run_adb_with_timeout(
                serial,
                ["exec-out", "uiautomator", "dump", "/dev/tty"],
                exec_out_timeout,
            )
            .await
        {
            Ok(output) => {
                if let Some(xml) = extract_uiautomator_hierarchy_xml(&output.stdout) {
                    fs::write(&local_path, xml)
                        .await
                        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
                    return Ok(local_path);
                }
            }
            Err(error) if is_exec_out_uiautomator_supported_failure(&error) => {}
            Err(error) => return Err(error),
        }

        let remote_path = remote_ui_dump_path();
        let mut last_pull_error: Option<McpError> = None;

        for attempt in 0..UI_DUMP_PULL_MAX_ATTEMPTS {
            let attempt_timeout = deadline
                .map(|deadline| require_remaining_budget(deadline, "UI hierarchy dump"))
                .transpose()?
                .unwrap_or(Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS));
            self.run_adb_shell_with_timeout(
                serial,
                ["uiautomator", "dump", &remote_path],
                attempt_timeout,
            )
            .await?;
            let pull_timeout = deadline
                .map(|deadline| require_remaining_budget(deadline, "UI hierarchy pull"))
                .transpose()?
                .unwrap_or(Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS));
            match self
                .run_adb_with_timeout(
                    serial,
                    [
                        "pull".to_string(),
                        remote_path.clone(),
                        local_path.display().to_string(),
                    ],
                    pull_timeout,
                )
                .await
            {
                Ok(_) => {
                    let _ = self.run_adb_shell(serial, ["rm", "-f", &remote_path]).await;
                    return Ok(local_path);
                }
                Err(error)
                    if should_retry_ui_dump_pull(&error)
                        && attempt + 1 < UI_DUMP_PULL_MAX_ATTEMPTS =>
                {
                    last_pull_error = Some(error);
                    remove_artifact_if_exists(Some(local_path.display().to_string())).await;
                    let _ = self.run_adb_shell(serial, ["rm", "-f", &remote_path]).await;
                    if let Some(deadline) = deadline {
                        let retry_delay = Duration::from_millis(UI_DUMP_PULL_RETRY_DELAY_MS)
                            .min(remaining_until(deadline));
                        if retry_delay.is_zero() {
                            break;
                        }
                        sleep(retry_delay).await;
                    } else {
                        sleep(Duration::from_millis(UI_DUMP_PULL_RETRY_DELAY_MS)).await;
                    }
                }
                Err(error) => {
                    let _ = self.run_adb_shell(serial, ["rm", "-f", &remote_path]).await;
                    return Err(error);
                }
            }
        }

        let _ = self.run_adb_shell(serial, ["rm", "-f", &remote_path]).await;
        Err(last_pull_error.unwrap_or_else(|| {
            McpError::internal_error(
                "UI hierarchy dump pull failed without a captured adb error",
                None,
            )
        }))
    }

    async fn capture_fast_ui_fingerprint_with_timeout(
        &self,
        serial: &str,
        timeout_duration: Duration,
        target_package: Option<&str>,
    ) -> Result<String, McpError> {
        let mut command = Command::new(&self.config.adb_path);
        command.arg("-s").arg(serial).args([
            "exec-out",
            "cmd",
            "window",
            "dump-visible-window-views",
        ]);
        let output = run_command_with_timeout(
            command,
            timeout_duration,
            &format!("adb -s {serial} exec-out cmd window dump-visible-window-views"),
        )
        .await?;
        let fingerprint = match target_package {
            Some(target_package) => normalized_visible_window_dump_fingerprint_for_package(
                &output.stdout,
                Some(target_package),
            ),
            None => normalized_visible_window_dump_fingerprint(&output.stdout),
        };
        fingerprint.map_err(|error| {
            McpError::internal_error(
                format!("failed to normalize visible-window dump fingerprint: {error}"),
                None,
            )
        })
    }

    pub(crate) async fn capture_fast_ui_fingerprint_until_deadline(
        &self,
        serial: &str,
        deadline: Instant,
        target_package: Option<&str>,
    ) -> Result<Option<String>, McpError> {
        if !has_meaningful_fast_ui_fingerprint_budget(deadline) {
            return Ok(None);
        }
        let timeout_duration = require_remaining_budget(
            fast_ui_fingerprint_deadline(deadline),
            "fast visible-window dump",
        )?;
        match self
            .capture_fast_ui_fingerprint_with_timeout(serial, timeout_duration, target_package)
            .await
        {
            Ok(fingerprint) => Ok(Some(fingerprint)),
            Err(_) => Ok(None),
        }
    }

    async fn capture_ui_observation(
        &self,
        serial: &str,
        hierarchy_filename: String,
        include_screenshot: bool,
        screenshot_filename: String,
    ) -> Result<UiObservation, McpError> {
        self.capture_ui_observation_with_deadline(
            serial,
            hierarchy_filename,
            include_screenshot,
            screenshot_filename,
            None,
        )
        .await
    }

    pub(crate) async fn capture_ui_observation_until_deadline(
        &self,
        serial: &str,
        hierarchy_filename: String,
        include_screenshot: bool,
        screenshot_filename: String,
        deadline: Instant,
    ) -> Result<Option<UiObservation>, McpError> {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.capture_ui_observation_with_deadline(
                serial,
                hierarchy_filename,
                include_screenshot,
                screenshot_filename,
                Some(deadline),
            ),
        )
        .await
        {
            Ok(Ok(observation)) => Ok(Some(observation)),
            Ok(Err(error)) if is_deadline_limited_observation_timeout(&error) => Ok(None),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(None),
        }
    }

    async fn capture_ui_observation_with_deadline(
        &self,
        serial: &str,
        hierarchy_filename: String,
        include_screenshot: bool,
        screenshot_filename: String,
        deadline: Option<Instant>,
    ) -> Result<UiObservation, McpError> {
        let hierarchy_path = self
            .dump_ui_hierarchy_internal_with_deadline(serial, hierarchy_filename, deadline)
            .await?;
        let hierarchy_xml = fs::read_to_string(&hierarchy_path)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let nodes = parse_ui_nodes_from_xml(&hierarchy_xml)?;
        let (screenshot_path, screenshot_backend) = if include_screenshot {
            let screenshot_timeout = deadline
                .map(|deadline| require_remaining_budget(deadline, "screenshot capture"))
                .transpose()?
                .unwrap_or(Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS));
            let (path, backend) = self
                .capture_screenshot_internal_with_timeout(
                    serial,
                    screenshot_filename,
                    screenshot_timeout,
                )
                .await?;
            (Some(path), Some(backend))
        } else {
            (None, None)
        };
        let window_state_timeout = deadline
            .map(|deadline| require_remaining_budget(deadline, "window-state capture"))
            .transpose()?
            .unwrap_or(Duration::from_secs(DEFAULT_ADB_COMMAND_TIMEOUT_SECS));
        let window_state = self
            .window_state_internal_with_timeout(serial, window_state_timeout)
            .await?;
        Ok(UiObservation {
            hierarchy_path,
            hierarchy_xml,
            nodes,
            screenshot_path,
            screenshot_backend,
            window_state,
        })
    }

    async fn check_system_dialog(
        &self,
        serial: &str,
        ui_path: &Path,
        nodes: &[NormalizedUiNode],
    ) -> Result<SystemDialogCheck, McpError> {
        let labels = labels_from_nodes(nodes);
        if labels.is_empty() {
            return Ok(SystemDialogCheck::None);
        }
        let artifact_path = ui_path.display().to_string();
        let Some(plan) = classify_system_dialog(&labels) else {
            return Ok(SystemDialogCheck::None);
        };
        if let Some(label) = plan.action_label
            && let Ok(button) = find_ui_node_by_label(ui_path, label)
        {
            let (x, y) = button.bounds.center();
            self.tap_point_internal(serial, x, y).await?;
            sleep(if plan.kind == "anr" {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(750)
            })
            .await;
            return Ok(SystemDialogCheck::Handled(SystemDialogReport {
                detected: true,
                kind: plan.kind.to_string(),
                labels,
                action_taken: Some(format!("tap:{label}")),
                artifact_path,
            }));
        }
        Ok(SystemDialogCheck::Blocked(SystemDialogReport {
            detected: true,
            kind: plan.kind.to_string(),
            labels,
            action_taken: None,
            artifact_path,
        }))
    }

    async fn tap_ui_label(&self, serial: &str, label: &str) -> Result<UiNodeMatch, McpError> {
        for attempt in 0..=1 {
            let ui_path = self
                .dump_ui_hierarchy_internal(
                    serial,
                    format!(
                        "tap-target-{}-attempt-{}.xml",
                        sanitize_filename(label),
                        attempt
                    ),
                )
                .await?;
            let nodes = parse_ui_nodes_from_path(&ui_path)?;
            match self.check_system_dialog(serial, &ui_path, &nodes).await? {
                SystemDialogCheck::Handled(_) => continue,
                SystemDialogCheck::Blocked(report) => {
                    return Err(McpError::internal_error(
                        format!(
                            "unable to tap ui label '{label}' because a blocking system dialog was detected: {}",
                            serde_json::to_string(&report)
                                .unwrap_or_else(|_| "system dialog".to_string())
                        ),
                        None,
                    ));
                }
                SystemDialogCheck::None => {}
            }
            let node = find_ui_node_by_label(&ui_path, label)?;
            let (x, y) = node.bounds.center();
            self.tap_point_internal(serial, x, y).await?;
            sleep(Duration::from_millis(500)).await;
            return Ok(node);
        }
        Err(McpError::internal_error(
            format!("unable to tap ui label '{label}' because a system dialog kept taking focus"),
            None,
        ))
    }

    async fn wait_for_ui_label(
        &self,
        serial: &str,
        label: &str,
        timeout_secs: u64,
    ) -> Result<UiNodeMatch, McpError> {
        let started = std::time::Instant::now();
        while started.elapsed() <= Duration::from_secs(timeout_secs) {
            let ui_path = self
                .dump_ui_hierarchy_internal(
                    serial,
                    format!(
                        "wait-{}-{}.xml",
                        sanitize_filename(label),
                        started.elapsed().as_millis()
                    ),
                )
                .await?;
            let nodes = parse_ui_nodes_from_path(&ui_path)?;
            match self.check_system_dialog(serial, &ui_path, &nodes).await? {
                SystemDialogCheck::Handled(_) => continue,
                SystemDialogCheck::Blocked(report) => {
                    return Err(McpError::internal_error(
                        format!(
                            "timed out waiting for ui label '{label}' because a blocking system dialog was detected: {}",
                            serde_json::to_string(&report)
                                .unwrap_or_else(|_| "system dialog".to_string())
                        ),
                        None,
                    ));
                }
                SystemDialogCheck::None => {}
            }
            if let Ok(node) = find_ui_node_by_label(&ui_path, label) {
                return Ok(node);
            }
            sleep(Duration::from_millis(750)).await;
        }
        Err(McpError::internal_error(
            format!("timed out waiting for ui label '{label}'"),
            None,
        ))
    }

    async fn run_stage_first_focus_earth(
        &self,
        args: SolarLabScenarioArgs,
    ) -> Result<serde_json::Value, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let app = SolarLabAppArgs::from(args);
        let mut artifacts = Vec::new();
        let outcome = async {
            self.launch_solarlab_app(&serial, &app).await?;
            self.wait_for_ui_label(&serial, "Search", 20).await?;

            artifacts.push(
                self.capture_solarlab_artifacts(&serial, "solarlab-focus-earth", "00-home")
                    .await?,
            );

            self.tap_ui_label(&serial, "Search").await?;
            self.wait_for_ui_label(&serial, "Search by name or id", 15)
                .await?;
            artifacts.push(
                self.capture_solarlab_artifacts(
                    &serial,
                    "solarlab-focus-earth",
                    "01-search-dialog",
                )
                .await?,
            );

            self.tap_ui_label(&serial, "Search by name or id").await?;
            self.run_adb_shell(&serial, ["input", "keyevent", "KEYCODE_MOVE_END"])
                .await?;
            for _ in 0..8 {
                self.run_adb_shell(&serial, ["input", "keyevent", "KEYCODE_DEL"])
                    .await
                    .ok();
            }
            self.run_adb_shell(&serial, ["input", "text", "earth"])
                .await?;

            self.wait_for_ui_label(&serial, "Earth", 15).await?;
            artifacts.push(
                self.capture_solarlab_artifacts(
                    &serial,
                    "solarlab-focus-earth",
                    "02-search-results",
                )
                .await?,
            );

            self.tap_ui_label(&serial, "Focus").await?;
            self.wait_for_ui_label(&serial, "Frame selected", 15)
                .await?;
            artifacts.push(
                self.capture_solarlab_artifacts(
                    &serial,
                    "solarlab-focus-earth",
                    "03-earth-focused",
                )
                .await?,
            );

            Ok(())
        }
        .await;

        self.finish_solarlab_scenario(
            ScenarioBundleSpec {
                scenario_slug: "solarlab-focus-earth",
                scenario_name: "stage_first_focus_earth",
                logcat_filename: "solarlab-focus-earth-logcat.txt",
            },
            &serial,
            &app,
            artifacts,
            outcome,
        )
        .await
    }

    async fn run_stage_first_immersive_roundtrip(
        &self,
        args: SolarLabScenarioArgs,
    ) -> Result<serde_json::Value, McpError> {
        let serial = self
            .resolve_serial_for_tools(args.serial.as_deref())
            .await?;
        let app = SolarLabAppArgs::from(args);
        let mut artifacts = Vec::new();
        let outcome = async {
            self.launch_solarlab_app(&serial, &app).await?;
            self.wait_for_ui_label(&serial, "Immersive", 20).await?;

            artifacts.push(
                self.capture_solarlab_artifacts(&serial, "solarlab-immersive-roundtrip", "00-home")
                    .await?,
            );

            self.tap_ui_label(&serial, "Immersive").await?;
            self.wait_for_ui_label(&serial, "Open immersive view?", 15)
                .await?;
            artifacts.push(
                self.capture_solarlab_artifacts(
                    &serial,
                    "solarlab-immersive-roundtrip",
                    "01-immersive-prompt",
                )
                .await?,
            );

            self.tap_ui_label(&serial, "Open immersive view").await?;
            self.wait_for_ui_label(&serial, "Sandbox", 20).await?;
            artifacts.push(
                self.capture_solarlab_artifacts(
                    &serial,
                    "solarlab-immersive-roundtrip",
                    "02-immersive-view",
                )
                .await?,
            );

            self.tap_ui_label(&serial, "Sandbox").await?;
            self.wait_for_ui_label(&serial, "Immersive", 20).await?;
            artifacts.push(
                self.capture_solarlab_artifacts(
                    &serial,
                    "solarlab-immersive-roundtrip",
                    "03-returned-to-sandbox",
                )
                .await?,
            );

            Ok(())
        }
        .await;

        self.finish_solarlab_scenario(
            ScenarioBundleSpec {
                scenario_slug: "solarlab-immersive-roundtrip",
                scenario_name: "stage_first_immersive_roundtrip",
                logcat_filename: "solarlab-immersive-roundtrip-logcat.txt",
            },
            &serial,
            &app,
            artifacts,
            outcome,
        )
        .await
    }
}

impl From<SolarLabScenarioArgs> for SolarLabAppArgs {
    fn from(value: SolarLabScenarioArgs) -> Self {
        Self {
            package_name: value
                .package_name
                .unwrap_or_else(|| "com.sednalabs.solarlab".to_string()),
            activity: value.activity,
        }
    }
}

fn validate_solarlab_semantic_action(
    action: &str,
    body_query: Option<String>,
) -> Result<SolarLabSemanticCommand, McpError> {
    match action.trim().to_lowercase().as_str() {
        "focus_body" => {
            let body_query = body_query.ok_or_else(|| {
                McpError::internal_error(
                    "solarlab.semantic_action action 'focus_body' requires a non-empty body_query"
                        .to_string(),
                    None,
                )
            })?;
            Ok(SolarLabSemanticCommand::FocusBody { body_query })
        }
        "reset_camera" => Ok(SolarLabSemanticCommand::ResetCamera),
        "open_immersive" => Ok(SolarLabSemanticCommand::OpenImmersive),
        "return_to_sandbox" => Ok(SolarLabSemanticCommand::ReturnToSandbox),
        other => Err(McpError::internal_error(
            format!(
                "Unsupported Solar Lab semantic action '{other}'. Expected one of focus_body, reset_camera, open_immersive, return_to_sandbox."
            ),
            None,
        )),
    }
}

fn matcher_for_action(action: &SolarLabSemanticCommand) -> SolarLabSemanticAckMatcher {
    match action {
        SolarLabSemanticCommand::FocusBody { body_query } => SolarLabSemanticAckMatcher {
            description: body_query.clone(),
        },
        SolarLabSemanticCommand::OpenImmersive => SolarLabSemanticAckMatcher {
            description: "Back to sandbox or sparse immersive hierarchy".to_string(),
        },
        SolarLabSemanticCommand::ReturnToSandbox => SolarLabSemanticAckMatcher {
            description: "Search, Immersive, No body selected, or Add object".to_string(),
        },
        SolarLabSemanticCommand::ResetCamera => SolarLabSemanticAckMatcher {
            description: "none".to_string(),
        },
    }
}

fn solarlab_semantic_ack_matches(action: &SolarLabSemanticCommand, hierarchy: &str) -> bool {
    let hierarchy_lower = hierarchy.to_lowercase();
    match action {
        SolarLabSemanticCommand::FocusBody { body_query } => {
            hierarchy_lower.contains(&body_query.to_lowercase())
        }
        SolarLabSemanticCommand::OpenImmersive => {
            if hierarchy_lower.contains("back to sandbox") {
                return true;
            }
            let sandbox_markers = ["search", "immersive", "no body selected"];
            let node_count = hierarchy_lower.matches("<node").count();
            node_count <= 2
                && !sandbox_markers
                    .iter()
                    .any(|marker| hierarchy_lower.contains(marker))
        }
        SolarLabSemanticCommand::ReturnToSandbox => {
            ["add object", "search", "immersive", "no body selected"]
                .iter()
                .any(|marker| hierarchy_lower.contains(marker))
        }
        SolarLabSemanticCommand::ResetCamera => true,
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
enum ScrollDirection {
    Up,
    Down,
}

impl ScrollDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

pub(crate) fn filename_or_timestamp(
    filename: Option<String>,
    prefix: &str,
    extension: &str,
) -> String {
    filename
        .as_deref()
        .and_then(|value| normalize_artifact_name(value, Some(extension)))
        .unwrap_or_else(|| timestamp_filename(prefix, Some(extension)))
}

fn normalize_artifact_name(filename: &str, extension: Option<&str>) -> Option<String> {
    let trimmed = filename.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut normalized = Path::new(trimmed)
        .file_name()
        .map(|value| value.to_string_lossy().trim().to_string())?;
    if normalized.is_empty() {
        return None;
    }

    if let Some(extension) = extension.map(|value| value.trim_start_matches('.'))
        && !extension.is_empty()
        && Path::new(&normalized).extension().is_none()
    {
        normalized.push('.');
        normalized.push_str(extension);
    }

    Some(normalized)
}

impl From<&LaunchAvdArgs> for LaunchRequest {
    fn from(value: &LaunchAvdArgs) -> Self {
        Self {
            avd_name: value.avd_name.trim().to_string(),
            no_window: value.no_window,
            gpu: value.gpu.clone(),
            grpc_port: value.grpc_port,
            extra_args: value.extra_args.clone(),
        }
    }
}

impl From<&LaunchAvdAndWaitArgs> for LaunchRequest {
    fn from(value: &LaunchAvdAndWaitArgs) -> Self {
        Self {
            avd_name: value.avd_name.trim().to_string(),
            no_window: value.no_window,
            gpu: value.gpu.clone(),
            grpc_port: value.grpc_port,
            extra_args: value.extra_args.clone(),
        }
    }
}

impl LaunchRequest {
    fn emulator_args(&self, default_grpc_port: Option<u16>) -> Vec<String> {
        let mut emulator_args = vec!["-avd".to_string(), self.avd_name.clone()];
        if should_launch_without_window(
            self.no_window,
            std::env::var_os("DISPLAY"),
            std::env::var_os("WAYLAND_DISPLAY"),
        ) {
            emulator_args.push("-no-window".to_string());
        }
        emulator_args.extend([
            "-no-audio".to_string(),
            "-no-boot-anim".to_string(),
            "-netdelay".to_string(),
            "none".to_string(),
            "-netspeed".to_string(),
            "full".to_string(),
            "-no-snapshot-load".to_string(),
        ]);
        if let Some(gpu) = self.gpu.as_deref() {
            emulator_args.extend(["-gpu".to_string(), gpu.to_string()]);
        }
        if let Some(grpc_port) = self.grpc_port.or(default_grpc_port) {
            emulator_args.extend(["-grpc".to_string(), grpc_port.to_string()]);
        }
        emulator_args.extend(self.extra_args.clone());
        emulator_args
    }
}

fn should_launch_without_window(
    requested_no_window: bool,
    display: Option<std::ffi::OsString>,
    wayland_display: Option<std::ffi::OsString>,
) -> bool {
    requested_no_window
        || [display, wayland_display]
            .into_iter()
            .flatten()
            .map(|value| value.to_string_lossy().trim().to_string())
            .all(|value| value.is_empty())
}

fn should_isolate_emulator_launch(systemd_run_available: bool) -> bool {
    systemd_run_available
}

fn systemd_run_is_available() -> bool {
    std::process::Command::new("systemd-run")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn resolve_serial_from_devices(
    serial: Option<&str>,
    devices: &[serde_json::Value],
) -> Result<String, McpError> {
    if let Some(explicit) = serial.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(explicit.to_string());
    }
    let ready_devices = devices
        .iter()
        .filter(|device| device.get("state").and_then(|value| value.as_str()) == Some("device"))
        .filter_map(|device| {
            device
                .get("serial")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    match ready_devices.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(McpError::invalid_params(
            "no ready Android devices found; pass `serial` explicitly or launch/connect a device",
            None,
        )),
        many => Err(McpError::invalid_params(
            format!(
                "multiple ready Android devices found ({}); pass `serial` explicitly",
                many.join(", ")
            ),
            None,
        )),
    }
}

fn ready_emulator_serials(devices: &[serde_json::Value]) -> Vec<String> {
    devices
        .iter()
        .filter(|device| device.get("state").and_then(|value| value.as_str()) == Some("device"))
        .filter_map(|device| device.get("serial").and_then(|value| value.as_str()))
        .filter(|serial| serial.starts_with("emulator-"))
        .map(ToOwned::to_owned)
        .collect()
}

fn select_emulator_serial_after_launch(
    pre_launch_ready_serials: &[String],
    devices: &[serde_json::Value],
    expected_serial: Option<&str>,
) -> Result<Option<String>, McpError> {
    if let Some(expected_serial) = expected_serial
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let expected_ready = devices.iter().any(|device| {
            device.get("serial").and_then(|value| value.as_str()) == Some(expected_serial)
                && device.get("state").and_then(|value| value.as_str()) == Some("device")
        });
        return Ok(expected_ready.then(|| expected_serial.to_string()));
    }

    let pre_launch = pre_launch_ready_serials
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let newly_ready = ready_emulator_serials(devices)
        .into_iter()
        .filter(|serial| !pre_launch.contains(serial.as_str()))
        .collect::<Vec<_>>();
    match newly_ready.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        many => Err(McpError::invalid_params(
            format!(
                "multiple newly ready emulator devices found after launch ({}); pass `expected_serial` explicitly",
                many.join(", ")
            ),
            None,
        )),
    }
}

fn timestamp_filename(prefix: &str, extension: Option<&str>) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let sequence = ARTIFACT_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    match extension {
        Some(ext) => format!("{prefix}-{millis}-{sequence}.{ext}"),
        None => format!("{prefix}-{millis}-{sequence}"),
    }
}

fn remote_ui_dump_path() -> String {
    format!("/sdcard/{}", timestamp_filename("window-dump", Some("xml")))
}

fn extract_uiautomator_hierarchy_xml(output: &str) -> Option<&str> {
    let start = output.find("<?xml").or_else(|| output.find("<hierarchy"))?;
    let end = output.rfind("</hierarchy>")?;
    let end = end + "</hierarchy>".len();
    let xml = output.get(start..end)?.trim();
    (!xml.is_empty()).then_some(xml)
}

fn is_exec_out_uiautomator_supported_failure(error: &McpError) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("inaccessible or not found")
        || message.contains("unknown command")
        || message.contains("usage: uiautomator")
        || message.contains("killed")
        || message.contains("closed")
}

fn is_deadline_limited_observation_timeout(error: &McpError) -> bool {
    let message = error.to_string();
    if !message.contains("timed out after") {
        return false;
    }
    message.contains("exec-out uiautomator dump /dev/tty")
        || message.contains(" shell uiautomator dump ")
        || (message.contains(" pull ") && message.contains("window-dump"))
}

fn should_retry_ui_dump_pull(error: &McpError) -> bool {
    let message = error.to_string();
    message.contains("failed to stat remote object")
        || message.contains("No such file or directory")
}

fn parse_adb_device_line(line: &str) -> Option<serde_json::Value> {
    let mut parts = line.split_whitespace();
    let serial = parts.next()?;
    let state = parts.next()?;
    let mut extra = serde_json::Map::new();
    for token in parts {
        if let Some((key, value)) = token.split_once(':') {
            extra.insert(key.to_string(), json!(value));
        }
    }
    Some(json!({
        "serial": serial,
        "state": state,
        "extra": extra,
    }))
}

fn escape_adb_text(text: &str) -> String {
    text.replace(' ', "%s")
}

fn fallback_clear_delete_count(existing_text: Option<&str>) -> usize {
    existing_text.unwrap_or_default().chars().count()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplacementTextDispatchPlan {
    GenericTextDispatch,
    RawAdbStep(Vec<String>),
}

fn replacement_text_dispatch_plan(text: &str) -> ReplacementTextDispatchPlan {
    if text.is_empty() {
        ReplacementTextDispatchPlan::RawAdbStep(vec![
            "input".to_string(),
            "keyevent".to_string(),
            "KEYCODE_DEL".to_string(),
        ])
    } else {
        ReplacementTextDispatchPlan::GenericTextDispatch
    }
}

fn merge_command_outputs(first: CommandOutput, second: CommandOutput) -> CommandOutput {
    let stdout = [first.stdout.trim_end(), second.stdout.trim_end()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let stderr = [first.stderr.trim_end(), second.stderr.trim_end()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    CommandOutput { stdout, stderr }
}

fn merge_dispatch_detail(prefix: String, nested_detail: Option<&str>) -> String {
    match nested_detail
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        Some(detail) => format!("{prefix}; {detail}"),
        None => prefix,
    }
}

fn node_matches_requested_text(node: &NormalizedUiNode, requested_text: &str) -> bool {
    matches_text(node.text.as_deref(), requested_text, true)
        || matches_text(node.semantic_label.as_deref(), requested_text, true)
        || matches_text(node.content_desc.as_deref(), requested_text, true)
}

fn shell_quote_command(program: &Path, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_quote(program.to_string_lossy().as_ref()));
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn absolute_path(path: &Path) -> Result<PathBuf, McpError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|err| McpError::internal_error(err.to_string(), None))
    }
}

fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn merge_stdout(first: &CommandOutput, second: &CommandOutput) -> String {
    merge_non_empty_lines(&first.stdout, &second.stdout)
}

fn merge_stderr(first: &CommandOutput, second: &CommandOutput) -> String {
    merge_non_empty_lines(&first.stderr, &second.stderr)
}

fn merge_non_empty_lines(first: &str, second: &str) -> String {
    [first.trim(), second.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn command_output_from_process(output: Output) -> CommandOutput {
    CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

fn command_failure_message(command_label: &str, output: &Output) -> String {
    let status = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "no stdout/stderr captured".to_string()
    };
    format!("{command_label} failed with exit status {status}: {detail}")
}

async fn run_command_with_timeout(
    mut command: Command,
    timeout_duration: Duration,
    command_label: &str,
) -> Result<Output, McpError> {
    command.kill_on_drop(true);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let child = command.spawn().map_err(|err| {
        McpError::internal_error(format!("{command_label} spawn failed: {err}"), None)
    })?;
    let output = timeout(timeout_duration, child.wait_with_output())
        .await
        .map_err(|_| {
            McpError::internal_error(
                format!(
                    "{command_label} timed out after {} ms",
                    timeout_duration.as_millis()
                ),
                None,
            )
        })?
        .map_err(|err| {
            McpError::internal_error(format!("{command_label} wait failed: {err}"), None)
        })?;
    if !output.status.success() {
        return Err(McpError::internal_error(
            command_failure_message(command_label, &output),
            None,
        ));
    }
    Ok(output)
}

fn labels_from_nodes(nodes: &[NormalizedUiNode]) -> Vec<String> {
    let mut labels = Vec::new();
    for value in nodes
        .iter()
        .flat_map(|node| {
            [
                node.text.as_deref(),
                node.semantic_label.as_deref(),
                node.content_desc.as_deref(),
            ]
        })
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if labels
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(value))
        {
            continue;
        }
        labels.push(value.to_string());
    }
    labels
}

fn has_label(labels: &[String], wanted: &str) -> bool {
    let wanted = wanted.to_ascii_lowercase();
    labels.iter().any(|label| {
        let candidate = label.to_ascii_lowercase();
        candidate == wanted || candidate.contains(&wanted)
    })
}

fn classify_system_dialog(labels: &[String]) -> Option<SystemDialogPlan> {
    if has_label(labels, "isn't responding") {
        return Some(SystemDialogPlan {
            kind: "anr",
            action_label: Some("Wait"),
        });
    }

    let permission_action = [
        "While using the app",
        "Allow only while using the app",
        "Only this time",
        "Allow",
    ]
    .into_iter()
    .find(|label| has_label(labels, label));
    if permission_action.is_some()
        && (has_label(labels, "don't allow")
            || has_label(labels, "deny")
            || has_label(labels, "permission")
            || has_label(labels, "while using the app"))
    {
        return Some(SystemDialogPlan {
            kind: "permission",
            action_label: permission_action,
        });
    }

    if has_label(labels, "keeps stopping") || has_label(labels, "Close app") {
        return Some(SystemDialogPlan {
            kind: "crash",
            action_label: None,
        });
    }

    None
}

pub(crate) async fn remove_artifact_if_exists(path: Option<String>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path).await;
    }
}

fn ui_observation_fingerprint(hierarchy_xml: &str, window_state: &AndroidWindowState) -> String {
    format!("{hierarchy_xml}\n{window_state:?}")
}

fn should_retry_tap_with_adb(
    backend: &str,
    retry_with_adb_on_no_change: bool,
    verification: &TapVerification,
) -> bool {
    verification.requested
        && !verification.satisfied
        && retry_with_adb_on_no_change
        && backend == "grpc"
}

fn should_retry_text_with_adb(
    backend: &str,
    retry_with_adb_on_no_change: bool,
    verification: &TextVerification,
) -> bool {
    verification.requested
        && !verification.satisfied
        && retry_with_adb_on_no_change
        && backend == "grpc"
        && verification.ui_changed_from_pre_text == Some(false)
        && verification.target_text_matches_requested != Some(true)
}

fn normalize_scroll_direction(raw: Option<&str>) -> Result<ScrollDirection, McpError> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(ScrollDirection::Up),
        Some(value) if value.eq_ignore_ascii_case("up") => Ok(ScrollDirection::Up),
        Some(value) if value.eq_ignore_ascii_case("down") => Ok(ScrollDirection::Down),
        Some(value) => Err(McpError::invalid_params(
            format!("direction must be one of: up, down; got '{value}'"),
            None,
        )),
    }
}

fn swipe_points_for_direction(
    display: (u32, u32),
    direction: ScrollDirection,
) -> (u32, u32, u32, u32) {
    let (width, height) = display;
    let x = (width / 2).max(1);
    let top = (height / 4).max(1);
    let bottom = ((height * 3) / 4).max(top + 1);
    match direction {
        ScrollDirection::Up => (x, bottom, x, top),
        ScrollDirection::Down => (x, top, x, bottom),
    }
}

fn parse_display_size(output: &str) -> Option<(u32, u32)> {
    output
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("Override size:")
                .map(str::trim)
                .and_then(parse_display_dimensions)
        })
        .or_else(|| {
            output.lines().find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("Physical size:")
                    .map(str::trim)
                    .or(Some(trimmed))
                    .and_then(parse_display_dimensions)
            })
        })
}

fn parse_display_dimensions(raw: &str) -> Option<(u32, u32)> {
    let (width, height) = raw.split_once('x')?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}

fn parse_android_window_state(window_output: &str, activity_output: &str) -> AndroidWindowState {
    AndroidWindowState {
        current_focus: find_dumpsys_component(window_output, &["mCurrentFocus="]),
        focused_app: find_dumpsys_component(window_output, &["mFocusedApp="]),
        resumed_activity: find_dumpsys_component(
            activity_output,
            &["topResumedActivity=", "mResumedActivity:"],
        ),
    }
}

fn find_dumpsys_component(output: &str, needles: &[&str]) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| needles.iter().any(|needle| line.contains(needle)))
        .and_then(extract_component_token)
}

fn extract_component_token(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|token| token.contains('/'))
        .map(|token| {
            token
                .trim_matches(|ch: char| "{}[](),".contains(ch))
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_surface::build_tool_inventory;
    use crate::ui::{UiBounds, parse_bounds, selector_matches};
    use mcp_toolkit_core::tool_inventory::{ToolInventoryPolicy, ToolOperation};
    use mcp_toolkit_testing::assert_tool_schema_snapshot;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn adb_device_line_is_parsed() {
        let parsed = parse_adb_device_line(
            "emulator-5554 device product:sdk_gphone64_x86_64 model:sdk device:emu64xa transport_id:1",
        )
        .expect("device line should parse");
        assert_eq!(parsed["serial"], "emulator-5554");
        assert_eq!(parsed["state"], "device");
        assert_eq!(parsed["extra"]["transport_id"], "1");
    }

    #[test]
    fn resolve_serial_from_devices_prefers_explicit_serial() {
        let resolved = resolve_serial_from_devices(Some(" emulator-5554 "), &[])
            .expect("explicit serial should be accepted");
        assert_eq!(resolved, "emulator-5554");
    }

    #[test]
    fn resolve_serial_from_devices_accepts_single_ready_device() {
        let resolved = resolve_serial_from_devices(
            None,
            &[json!({
                "serial": "emulator-5554",
                "state": "device"
            })],
        )
        .expect("single ready device should be auto-selected");
        assert_eq!(resolved, "emulator-5554");
    }

    #[test]
    fn resolve_serial_from_devices_rejects_when_no_ready_devices_exist() {
        let err = resolve_serial_from_devices(
            None,
            &[json!({
                "serial": "emulator-5554",
                "state": "offline"
            })],
        )
        .expect_err("no ready devices should be rejected");
        assert!(
            err.to_string().contains("no ready Android devices found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_serial_from_devices_rejects_ambiguous_device_selection() {
        let err = resolve_serial_from_devices(
            None,
            &[
                json!({
                    "serial": "emulator-5554",
                    "state": "device"
                }),
                json!({
                    "serial": "emulator-5556",
                    "state": "device"
                }),
            ],
        )
        .expect_err("multiple ready devices should be rejected");
        let rendered = err.to_string();
        assert!(rendered.contains("multiple ready Android devices found"));
        assert!(rendered.contains("emulator-5554"));
        assert!(rendered.contains("emulator-5556"));
    }

    #[test]
    fn select_emulator_serial_after_launch_prefers_expected_ready_serial() {
        let selected = select_emulator_serial_after_launch(
            &["emulator-5554".to_string()],
            &[json!({
                "serial": "emulator-5560",
                "state": "device",
                "extra": {}
            })],
            Some("emulator-5560"),
        )
        .expect("expected serial selection should succeed");
        assert_eq!(selected.as_deref(), Some("emulator-5560"));
    }

    #[test]
    fn select_emulator_serial_after_launch_detects_single_new_ready_emulator() {
        let selected = select_emulator_serial_after_launch(
            &["emulator-5554".to_string()],
            &[
                json!({
                    "serial": "emulator-5554",
                    "state": "device",
                    "extra": {}
                }),
                json!({
                    "serial": "emulator-5560",
                    "state": "device",
                    "extra": {}
                }),
            ],
            None,
        )
        .expect("newly ready emulator detection should succeed");
        assert_eq!(selected.as_deref(), Some("emulator-5560"));
    }

    #[test]
    fn select_emulator_serial_after_launch_waits_when_no_new_ready_emulator_exists() {
        let selected = select_emulator_serial_after_launch(
            &["emulator-5554".to_string()],
            &[json!({
                "serial": "emulator-5554",
                "state": "device",
                "extra": {}
            })],
            None,
        )
        .expect("no new emulator should not be an error");
        assert_eq!(selected, None);
    }

    #[test]
    fn select_emulator_serial_after_launch_rejects_ambiguous_new_emulators() {
        let err = select_emulator_serial_after_launch(
            &[],
            &[
                json!({
                    "serial": "emulator-5554",
                    "state": "device",
                    "extra": {}
                }),
                json!({
                    "serial": "emulator-5560",
                    "state": "device",
                    "extra": {}
                }),
            ],
            None,
        )
        .expect_err("multiple newly ready emulators should be rejected");
        assert!(
            err.to_string()
                .contains("multiple newly ready emulator devices found after launch")
        );
    }

    #[test]
    fn launch_request_forces_headless_mode_when_no_display_is_available() {
        assert!(should_launch_without_window(
            false,
            None,
            Some(std::ffi::OsString::from("")),
        ));
    }

    #[test]
    fn launch_request_preserves_windowed_mode_when_display_exists() {
        assert!(!should_launch_without_window(
            false,
            Some(std::ffi::OsString::from(":0")),
            None,
        ));
    }

    #[test]
    fn launch_request_honors_explicit_no_window() {
        assert!(should_launch_without_window(
            true,
            Some(std::ffi::OsString::from(":0")),
            Some(std::ffi::OsString::from("wayland-0")),
        ));
    }

    #[test]
    fn emulator_launch_is_isolated_when_running_under_systemd_service() {
        assert!(should_isolate_emulator_launch(true));
    }

    #[test]
    fn emulator_launch_is_not_isolated_without_systemd_invocation_context() {
        assert!(!should_isolate_emulator_launch(false));
    }

    #[test]
    fn adb_text_spaces_are_escaped() {
        assert_eq!(escape_adb_text("Earth Focus"), "Earth%sFocus");
    }

    #[test]
    fn parses_ui_bounds() {
        let bounds = parse_bounds("[10,20][110,220]").expect("bounds should parse");
        assert_eq!(bounds.left, 10);
        assert_eq!(bounds.top, 20);
        assert_eq!(bounds.right, 110);
        assert_eq!(bounds.bottom, 220);
        assert_eq!(bounds.center(), (60, 120));
    }

    #[test]
    fn sanitizes_filenames() {
        assert_eq!(
            sanitize_filename("Open immersive view"),
            "open-immersive-view"
        );
    }

    #[test]
    fn parses_normalized_ui_nodes_from_xml() {
        let xml = r#"
            <hierarchy>
              <node text="" content-desc="" resource-id="" class="android.widget.FrameLayout" package="com.example" clickable="false" focusable="false" focused="false" scrollable="false" long-clickable="false" enabled="true" selected="false" checked="false" bounds="[0,0][1080,2400]">
                <node text="Search" content-desc="Search action" resource-id="com.example:id/search" class="android.widget.Button" package="com.example" clickable="true" focusable="true" focused="true" scrollable="false" long-clickable="true" enabled="true" selected="false" checked="false" bounds="[48,96][240,180]" />
              </node>
            </hierarchy>
        "#;
        let nodes = parse_ui_nodes_from_xml(xml).expect("ui nodes should parse");
        assert_eq!(nodes.len(), 2);
        let button = nodes
            .iter()
            .find(|node| node.resource_id.as_deref() == Some("com.example:id/search"))
            .expect("button node should be present");
        assert_eq!(button.text.as_deref(), Some("Search"));
        assert_eq!(button.content_desc.as_deref(), Some("Search action"));
        assert!(button.clickable);
        assert!(button.focused);
        assert!(button.long_clickable);
        assert_eq!(button.center, Some((144, 138)));
    }

    #[test]
    fn selector_matches_text_and_resource_id() {
        let node = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Earth".to_string()),
            semantic_label: None,
            content_desc: Some("Focus Earth".to_string()),
            resource_id: Some("com.example:id/earth".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        assert!(selector_matches(
            &node,
            &UiSelector {
                text: Some("ear".to_string()),
                ..UiSelector::default()
            }
        ));
        assert!(selector_matches(
            &node,
            &UiSelector {
                text: Some("Earth".to_string()),
                text_exact: Some(true),
                ..UiSelector::default()
            }
        ));
        assert!(!selector_matches(
            &node,
            &UiSelector {
                text: Some("Ear".to_string()),
                text_exact: Some(true),
                ..UiSelector::default()
            }
        ));
        assert!(selector_matches(
            &node,
            &UiSelector {
                resource_id: Some("com.example:id/earth".to_string()),
                clickable: Some(true),
                focused: Some(true),
                long_clickable: Some(true),
                ..UiSelector::default()
            }
        ));
        assert!(!selector_matches(
            &node,
            &UiSelector {
                content_desc: Some("Mars".to_string()),
                ..UiSelector::default()
            }
        ));
    }

    #[test]
    fn selector_matches_focus_and_interaction_state() {
        let node = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        assert!(selector_matches(
            &node,
            &UiSelector {
                focused: Some(true),
                focusable: Some(true),
                long_clickable: Some(true),
                ..UiSelector::default()
            }
        ));
        assert!(!selector_matches(
            &node,
            &UiSelector {
                focusable: Some(false),
                ..UiSelector::default()
            }
        ));
        assert!(!selector_matches(
            &node,
            &UiSelector {
                scrollable: Some(true),
                ..UiSelector::default()
            }
        ));
    }

    #[test]
    fn tap_verification_status_requires_a_real_transition_for_preexisting_follow_up_selector() {
        let wait_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("android".to_string()),
            text: Some("Wait".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("android:id/aerr_wait".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        let search_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.sednalabs.solarlab".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.sednalabs.solarlab:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 100,
                top: 100,
                right: 240,
                bottom: 160,
            }),
            center: Some((170, 130)),
        };
        let tapped_selector = UiSelector {
            resource_id: Some("android:id/aerr_wait".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };
        let follow_up_selector = UiSelector {
            text: Some("Search".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };

        let status = tap_verification_status(
            std::slice::from_ref(&wait_button),
            std::slice::from_ref(&wait_button),
            &tapped_selector,
            true,
            None,
            Some(&follow_up_selector),
            Some(true),
        );
        assert!(status.tapped_present);
        assert_eq!(status.post_present, Some(false));
        assert!(!status.satisfied);

        let status = tap_verification_status(
            std::slice::from_ref(&wait_button),
            std::slice::from_ref(&search_button),
            &tapped_selector,
            true,
            None,
            Some(&follow_up_selector),
            Some(true),
        );
        assert!(!status.tapped_present);
        assert_eq!(status.post_present, Some(true));
        assert!(status.satisfied);

        let status = tap_verification_status(
            &[wait_button.clone(), search_button.clone()],
            &[wait_button, search_button],
            &tapped_selector,
            false,
            None,
            Some(&follow_up_selector),
            Some(true),
        );
        assert!(status.tapped_present);
        assert_eq!(status.post_present, Some(true));
        assert_eq!(status.pre_post_present, Some(true));
        assert_eq!(status.ui_changed_from_pre_tap, Some(false));
        assert!(!status.satisfied);
    }

    #[test]
    fn tap_verification_status_still_requires_disappearance_when_requested() {
        let wait_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("android".to_string()),
            text: Some("Wait".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("android:id/aerr_wait".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        let search_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.sednalabs.solarlab".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.sednalabs.solarlab:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 100,
                top: 100,
                right: 240,
                bottom: 160,
            }),
            center: Some((170, 130)),
        };
        let tapped_selector = UiSelector {
            resource_id: Some("android:id/aerr_wait".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };
        let follow_up_selector = UiSelector {
            text: Some("Search".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };

        let status = tap_verification_status(
            &[wait_button.clone(), search_button.clone()],
            &[wait_button, search_button],
            &tapped_selector,
            true,
            None,
            Some(&follow_up_selector),
            Some(true),
        );
        assert!(status.tapped_present);
        assert_eq!(status.post_present, Some(true));
        assert!(!status.satisfied);
    }

    #[test]
    fn tap_verification_status_accepts_preexisting_follow_up_when_ui_changes() {
        let wait_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("android".to_string()),
            text: Some("Wait".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("android:id/aerr_wait".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        let search_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.sednalabs.solarlab".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.sednalabs.solarlab:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 100,
                top: 100,
                right: 240,
                bottom: 160,
            }),
            center: Some((170, 130)),
        };
        let tapped_selector = UiSelector {
            resource_id: Some("android:id/aerr_wait".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };
        let follow_up_selector = UiSelector {
            text: Some("Search".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };

        let status = tap_verification_status(
            &[wait_button.clone(), search_button.clone()],
            &[
                wait_button,
                NormalizedUiNode {
                    selected: true,
                    ..search_button
                },
            ],
            &tapped_selector,
            false,
            None,
            Some(&follow_up_selector),
            Some(true),
        );
        assert!(status.tapped_present);
        assert_eq!(status.post_present, Some(true));
        assert_eq!(status.pre_post_present, Some(true));
        assert_eq!(status.ui_changed_from_pre_tap, Some(true));
        assert!(status.satisfied);
    }

    #[test]
    fn tap_verification_status_ignores_unrelated_ui_churn_for_preexisting_follow_up_selector() {
        let refresh_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Refresh".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/refresh".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        let list_item = NormalizedUiNode {
            class_name: Some("android.widget.TextView".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Earth".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/list_item".to_string()),
            clickable: false,
            focusable: false,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 100,
                top: 100,
                right: 240,
                bottom: 160,
            }),
            center: Some((170, 130)),
        };
        let unrelated_clock = NormalizedUiNode {
            class_name: Some("android.widget.TextView".to_string()),
            package_name: Some("com.android.systemui".to_string()),
            text: Some("10:00".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.android.systemui:id/clock".to_string()),
            clickable: false,
            focusable: false,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 900,
                top: 0,
                right: 1080,
                bottom: 40,
            }),
            center: Some((990, 20)),
        };
        let unrelated_clock_changed = NormalizedUiNode {
            text: Some("10:01".to_string()),
            ..unrelated_clock.clone()
        };
        let tapped_selector = UiSelector {
            resource_id: Some("com.example:id/refresh".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };
        let follow_up_selector = UiSelector {
            text: Some("Earth".to_string()),
            ..UiSelector::default()
        };

        let status = tap_verification_status(
            &[refresh_button.clone(), list_item.clone(), unrelated_clock],
            &[refresh_button, list_item, unrelated_clock_changed],
            &tapped_selector,
            false,
            None,
            Some(&follow_up_selector),
            Some(true),
        );
        assert_eq!(status.post_present, Some(true));
        assert_eq!(status.pre_post_present, Some(true));
        assert_eq!(status.ui_changed_from_pre_tap, Some(false));
        assert!(!status.satisfied);
    }

    #[test]
    fn tap_verification_status_accepts_focus_change_for_preexisting_follow_up_selector() {
        let search_field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 200,
                bottom: 60,
            }),
            center: Some((100, 30)),
        };
        let tapped_selector = UiSelector {
            resource_id: Some("com.example:id/search".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };
        let follow_up_selector = UiSelector {
            resource_id: Some("com.example:id/search".to_string()),
            focused: Some(true),
            ..UiSelector::default()
        };

        let status = tap_verification_status(
            std::slice::from_ref(&search_field),
            &[NormalizedUiNode {
                focused: true,
                ..search_field.clone()
            }],
            &tapped_selector,
            false,
            None,
            Some(&follow_up_selector),
            Some(false),
        );
        assert_eq!(status.post_present, Some(true));
        assert_eq!(status.pre_post_present, Some(false));
        assert_eq!(status.ui_changed_from_pre_tap, Some(true));
        assert!(status.satisfied);
    }

    #[test]
    fn tap_verification_status_accepts_focus_change_with_internal_tracker_when_label_drifts() {
        let search_field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search by name or id".to_string()),
            semantic_label: Some("Search by name or id".to_string()),
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 183,
                top: 1055,
                right: 897,
                bottom: 1223,
            }),
            center: Some((540, 1139)),
        };
        let focused_typed_field = NormalizedUiNode {
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            focused: true,
            ..search_field.clone()
        };
        let tapped_selector = UiSelector {
            label: Some("Search by name or id".to_string()),
            label_exact: Some(true),
            focusable: Some(true),
            ..UiSelector::default()
        };
        let focus_tracker =
            InternalNodeTracker::from_target_node_with_focus(&search_field, Some(true))
                .expect("tracker");
        let stale_focus_selector = UiSelector {
            focused: Some(true),
            ..tapped_selector.clone()
        };

        let selector_only_status = tap_verification_status(
            std::slice::from_ref(&search_field),
            std::slice::from_ref(&focused_typed_field),
            &tapped_selector,
            false,
            None,
            Some(&stale_focus_selector),
            Some(false),
        );
        let tracker_status = tap_verification_status(
            std::slice::from_ref(&search_field),
            std::slice::from_ref(&focused_typed_field),
            &tapped_selector,
            false,
            Some(&focus_tracker),
            Some(&stale_focus_selector),
            Some(false),
        );

        assert_eq!(selector_only_status.post_present, Some(false));
        assert!(!selector_only_status.satisfied);
        assert_eq!(tracker_status.post_present, Some(true));
        assert_eq!(tracker_status.pre_post_present, Some(false));
        assert_eq!(tracker_status.ui_changed_from_pre_tap, Some(true));
        assert!(tracker_status.satisfied);
    }

    #[test]
    fn tap_verification_fingerprint_ignores_unrelated_background_noise() {
        let refresh_button = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Refresh".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/refresh".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        let list_item = NormalizedUiNode {
            class_name: Some("android.widget.TextView".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Earth".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/list_item".to_string()),
            clickable: false,
            focusable: false,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 100,
                top: 100,
                right: 240,
                bottom: 160,
            }),
            center: Some((170, 130)),
        };
        let unrelated_spinner = NormalizedUiNode {
            class_name: Some("android.widget.ProgressBar".to_string()),
            package_name: Some("com.example".to_string()),
            text: None,
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/spinner".to_string()),
            clickable: false,
            focusable: false,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 400,
                top: 400,
                right: 460,
                bottom: 460,
            }),
            center: Some((430, 430)),
        };
        let unrelated_spinner_shifted = NormalizedUiNode {
            bounds: Some(UiBounds {
                left: 420,
                top: 420,
                right: 480,
                bottom: 480,
            }),
            center: Some((450, 450)),
            ..unrelated_spinner.clone()
        };
        let tapped_selector = UiSelector {
            resource_id: Some("com.example:id/refresh".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };
        let follow_up_selector = UiSelector {
            text: Some("Earth".to_string()),
            ..UiSelector::default()
        };

        let first = tap_verification_fingerprint(
            &[refresh_button.clone(), list_item.clone(), unrelated_spinner],
            &tapped_selector,
            None,
            Some(&follow_up_selector),
        );
        let second = tap_verification_fingerprint(
            &[refresh_button, list_item, unrelated_spinner_shifted],
            &tapped_selector,
            None,
            Some(&follow_up_selector),
        );

        assert_eq!(first, second);
    }

    #[test]
    fn tap_verification_fingerprint_ignores_target_bounds_jitter_but_keeps_state_changes() {
        let search_field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 200,
                bottom: 60,
            }),
            center: Some((100, 30)),
        };
        let search_field_shifted = NormalizedUiNode {
            bounds: Some(UiBounds {
                left: 8,
                top: 4,
                right: 208,
                bottom: 64,
            }),
            center: Some((108, 34)),
            ..search_field.clone()
        };
        let search_field_focused = NormalizedUiNode {
            focused: true,
            ..search_field.clone()
        };
        let tapped_selector = UiSelector {
            resource_id: Some("com.example:id/search".to_string()),
            clickable: Some(true),
            ..UiSelector::default()
        };
        let focus_selector = UiSelector {
            resource_id: Some("com.example:id/search".to_string()),
            focused: Some(true),
            ..UiSelector::default()
        };

        let original = tap_verification_fingerprint(
            std::slice::from_ref(&search_field),
            &tapped_selector,
            None,
            Some(&focus_selector),
        );
        let shifted = tap_verification_fingerprint(
            std::slice::from_ref(&search_field_shifted),
            &tapped_selector,
            None,
            Some(&focus_selector),
        );
        let focused = tap_verification_fingerprint(
            std::slice::from_ref(&search_field_focused),
            &tapped_selector,
            None,
            Some(&focus_selector),
        );

        assert_eq!(original, shifted);
        assert_ne!(original, focused);
    }

    #[test]
    fn tap_verification_confirmation_accepts_repeated_satisfied_polls_without_stability() {
        let status = TapVerificationStatus {
            tapped_present: false,
            post_present: Some(true),
            pre_post_present: Some(false),
            ui_changed_from_pre_tap: Some(true),
            satisfied: true,
        };
        assert!(!tap_verification_is_confirmed(&status, 1, 2, false));
        assert!(tap_verification_is_confirmed(&status, 2, 2, false));
        assert!(tap_verification_is_confirmed(&status, 1, 2, true));
    }

    #[test]
    fn text_verification_fingerprint_ignores_target_bounds_jitter_but_keeps_text_changes() {
        let search_field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: Some("Search".to_string()),
            content_desc: None,
            resource_id: Some("com.example:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 200,
                bottom: 60,
            }),
            center: Some((100, 30)),
        };
        let search_field_shifted = NormalizedUiNode {
            bounds: Some(UiBounds {
                left: 8,
                top: 4,
                right: 208,
                bottom: 64,
            }),
            center: Some((108, 34)),
            ..search_field.clone()
        };
        let search_field_typed = NormalizedUiNode {
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            ..search_field.clone()
        };
        let target_selector = UiSelector {
            resource_id: Some("com.example:id/search".to_string()),
            focused: Some(true),
            ..UiSelector::default()
        };

        let original = text_verification_fingerprint(
            std::slice::from_ref(&search_field),
            None,
            Some(&target_selector),
            None,
        );
        let shifted = text_verification_fingerprint(
            std::slice::from_ref(&search_field_shifted),
            None,
            Some(&target_selector),
            None,
        );
        let typed = text_verification_fingerprint(
            std::slice::from_ref(&search_field_typed),
            None,
            Some(&target_selector),
            None,
        );

        assert_eq!(original, shifted);
        assert_ne!(original, typed);
    }

    #[test]
    fn text_verification_requires_change_when_target_and_follow_up_already_match() {
        let search_field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: Some("Search".to_string()),
            content_desc: None,
            resource_id: Some("com.example:id/search".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 200,
                bottom: 60,
            }),
            center: Some((100, 30)),
        };
        let typed_field = NormalizedUiNode {
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            ..search_field.clone()
        };
        let target_selector = UiSelector {
            resource_id: Some("com.example:id/search".to_string()),
            focused: Some(true),
            ..UiSelector::default()
        };
        let wait_for_selector = UiSelector {
            resource_id: Some("com.example:id/search".to_string()),
            focused: Some(true),
            ..UiSelector::default()
        };

        let unchanged = text_verification_status(
            std::slice::from_ref(&search_field),
            std::slice::from_ref(&search_field),
            None,
            Some(&target_selector),
            Some("earth"),
            Some(&wait_for_selector),
        );
        let changed = text_verification_status(
            std::slice::from_ref(&search_field),
            std::slice::from_ref(&typed_field),
            None,
            Some(&target_selector),
            Some("earth"),
            Some(&wait_for_selector),
        );

        assert!(!unchanged.satisfied);
        assert_eq!(unchanged.ui_changed_from_pre_text, Some(false));
        assert!(changed.satisfied);
        assert_eq!(changed.ui_changed_from_pre_text, Some(true));
    }

    #[test]
    fn text_verification_target_selector_tracks_focused_field_after_text_changes() {
        let focused_field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search by name or id".to_string()),
            semantic_label: Some("Search by name or id".to_string()),
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            scrollable: false,
            long_clickable: true,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 200,
                bottom: 60,
            }),
            center: Some((100, 30)),
        };
        let typed_field = NormalizedUiNode {
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            ..focused_field.clone()
        };
        let derived_selector = text_verification_target_selector(&focused_field);

        let status = text_verification_status(
            std::slice::from_ref(&focused_field),
            std::slice::from_ref(&typed_field),
            None,
            Some(&derived_selector),
            Some("earth"),
            None,
        );

        assert_eq!(derived_selector.focused, None);
        assert_eq!(derived_selector.focusable, Some(true));
        assert!(status.satisfied);
        assert_eq!(status.ui_changed_from_pre_text, Some(true));
    }

    #[test]
    fn text_verification_allows_noop_when_target_already_matches_requested_text() {
        let field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            long_clickable: true,
            scrollable: false,
            bounds: None,
            center: None,
        };
        let derived_selector = text_verification_target_selector(&field);

        let status = text_verification_status(
            std::slice::from_ref(&field),
            std::slice::from_ref(&field),
            None,
            Some(&derived_selector),
            Some("earth"),
            None,
        );

        assert!(status.satisfied);
        assert_eq!(status.target_text_matches_requested, Some(true));
        assert_eq!(status.ui_changed_from_pre_text, Some(false));
    }

    #[test]
    fn text_verification_tolerates_focus_loss_after_successful_text_change() {
        let focused_field = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search by name or id".to_string()),
            semantic_label: Some("Search by name or id".to_string()),
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            long_clickable: true,
            scrollable: false,
            bounds: None,
            center: None,
        };
        let unfocused_typed_field = NormalizedUiNode {
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            focused: false,
            ..focused_field.clone()
        };
        let derived_selector = text_verification_target_selector(&focused_field);

        let status = text_verification_status(
            std::slice::from_ref(&focused_field),
            std::slice::from_ref(&unfocused_typed_field),
            None,
            Some(&derived_selector),
            Some("earth"),
            None,
        );

        assert!(status.satisfied);
        assert_eq!(status.target_present, Some(true));
        assert_eq!(status.target_text_matches_requested, Some(true));
    }

    #[test]
    fn internal_node_tracker_matches_by_bounds_when_resource_id_is_missing() {
        let original = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search by name or id".to_string()),
            semantic_label: Some("Search by name or id".to_string()),
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            long_clickable: true,
            scrollable: false,
            bounds: Some(UiBounds {
                left: 10,
                top: 20,
                right: 210,
                bottom: 80,
            }),
            center: Some((110, 50)),
        };
        let tracker = InternalNodeTracker::from_target_node(&original).expect("tracker");
        let typed = NormalizedUiNode {
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            bounds: Some(UiBounds {
                left: 12,
                top: 22,
                right: 212,
                bottom: 82,
            }),
            ..original.clone()
        };
        let other_field = NormalizedUiNode {
            bounds: Some(UiBounds {
                left: 260,
                top: 20,
                right: 460,
                bottom: 80,
            }),
            ..typed.clone()
        };

        assert!(tracker_matches_node(&typed, &tracker));
        assert!(!tracker_matches_node(&other_field, &tracker));
    }

    #[test]
    fn internal_node_tracker_rejects_neighboring_overlap_without_strong_spatial_match() {
        let original = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search by name or id".to_string()),
            semantic_label: Some("Search by name or id".to_string()),
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            long_clickable: true,
            scrollable: false,
            bounds: Some(UiBounds {
                left: 10,
                top: 20,
                right: 210,
                bottom: 80,
            }),
            center: Some((110, 50)),
        };
        let tracker = InternalNodeTracker::from_target_node(&original).expect("tracker");
        let overlapping_neighbor = NormalizedUiNode {
            text: Some("earth".to_string()),
            semantic_label: Some("earth".to_string()),
            bounds: Some(UiBounds {
                left: 150,
                top: 20,
                right: 350,
                bottom: 80,
            }),
            center: Some((250, 50)),
            ..original.clone()
        };

        assert!(!tracker_matches_node(&overlapping_neighbor, &tracker));
    }

    #[test]
    fn internal_node_tracker_respects_requested_focus_state() {
        let original = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search by name or id".to_string()),
            semantic_label: Some("Search by name or id".to_string()),
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            long_clickable: true,
            scrollable: false,
            bounds: Some(UiBounds {
                left: 10,
                top: 20,
                right: 210,
                bottom: 80,
            }),
            center: Some((110, 50)),
        };
        let focus_tracker = InternalNodeTracker::from_target_node_with_focus(&original, Some(true))
            .expect("tracker");
        let unfocused = original.clone();
        let focused = NormalizedUiNode {
            focused: true,
            ..original
        };

        assert!(!tracker_matches_node(&unfocused, &focus_tracker));
        assert!(tracker_matches_node(&focused, &focus_tracker));
    }

    #[test]
    fn bounded_deadline_prefers_shorter_soft_budget() {
        let hard_deadline = Instant::now() + Duration::from_secs(5);
        let bounded = bounded_deadline(hard_deadline, Duration::from_millis(250));

        assert!(bounded <= hard_deadline);
        assert!(remaining_until(bounded) <= Duration::from_millis(300));
    }

    #[test]
    fn reserved_fallback_deadline_keeps_time_in_hand_for_retry_budget() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let reserved = reserved_fallback_deadline(deadline, Duration::from_secs(2));

        assert!(reserved <= deadline);
        assert!(remaining_until(reserved) >= Duration::from_secs(2));
    }

    #[test]
    fn initial_text_verification_deadline_uses_full_budget_for_adb_dispatch() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let initial = initial_text_verification_deadline("adb", deadline, true);

        assert_eq!(initial, deadline);
    }

    #[test]
    fn initial_text_verification_deadline_uses_full_budget_when_retry_disabled() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let initial = initial_text_verification_deadline("grpc", deadline, false);

        assert_eq!(initial, deadline);
    }

    #[test]
    fn initial_text_verification_deadline_uses_full_budget_when_not_enough_time_for_split() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let initial = initial_text_verification_deadline("grpc", deadline, true);

        assert_eq!(initial, deadline);
    }

    #[test]
    fn initial_text_verification_deadline_reserves_budget_only_when_split_is_meaningful() {
        let deadline = Instant::now() + Duration::from_secs(12);
        let initial = initial_text_verification_deadline("grpc", deadline, true);

        assert!(initial < deadline);
        assert!(remaining_until(initial) >= Duration::from_millis(6800));
        assert!(remaining_until(initial) <= Duration::from_millis(7100));
        assert!(has_meaningful_text_fallback_budget(deadline));
    }

    #[test]
    fn meaningful_text_fallback_budget_rejects_near_expiry_deadlines() {
        let almost_expired = Instant::now() + Duration::from_millis(1500);
        let healthy = Instant::now() + Duration::from_millis(5200);

        assert!(!has_meaningful_text_fallback_budget(almost_expired));
        assert!(has_meaningful_text_fallback_budget(healthy));
    }

    #[test]
    fn has_meaningful_observation_budget_rejects_near_expiry_deadlines() {
        let almost_expired = Instant::now() + Duration::from_millis(100);
        let healthy = Instant::now() + Duration::from_millis(1000);

        assert!(!has_meaningful_observation_budget(almost_expired));
        assert!(has_meaningful_observation_budget(healthy));
    }

    #[test]
    fn has_meaningful_fast_ui_fingerprint_budget_requires_full_capture_reserve() {
        let tight = Instant::now() + Duration::from_millis(5400);
        let healthy = Instant::now() + Duration::from_millis(6500);

        assert!(!has_meaningful_fast_ui_fingerprint_budget(tight));
        assert!(has_meaningful_fast_ui_fingerprint_budget(healthy));
    }

    #[test]
    fn fallback_clear_delete_count_tracks_existing_text_length() {
        assert_eq!(fallback_clear_delete_count(None), 0);
        assert_eq!(fallback_clear_delete_count(Some("")), 0);
        assert_eq!(fallback_clear_delete_count(Some("earth")), 5);
    }

    #[test]
    fn replacement_text_dispatch_plan_uses_generic_dispatch_for_non_empty_values() {
        assert_eq!(
            replacement_text_dispatch_plan("hello world"),
            ReplacementTextDispatchPlan::GenericTextDispatch
        );
    }

    #[test]
    fn replacement_text_dispatch_plan_uses_delete_for_empty_values() {
        assert_eq!(
            replacement_text_dispatch_plan(""),
            ReplacementTextDispatchPlan::RawAdbStep(vec![
                "input".to_string(),
                "keyevent".to_string(),
                "KEYCODE_DEL".to_string()
            ])
        );
    }

    #[test]
    fn merge_dispatch_detail_appends_nested_transport_context() {
        assert_eq!(
            merge_dispatch_detail(
                "replaced focused field contents via adb select-all".to_string(),
                Some("gRPC text transport failed, fell back to adb: unavailable")
            ),
            "replaced focused field contents via adb select-all; gRPC text transport failed, fell back to adb: unavailable"
        );
    }

    #[test]
    fn node_matches_requested_text_accepts_exact_text_and_semantic_label() {
        let mut node = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("mars".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            scrollable: false,
            long_clickable: true,
            bounds: None,
            center: None,
        };
        assert!(node_matches_requested_text(&node, "mars"));

        node.text = None;
        node.semantic_label = Some("venus".to_string());
        assert!(node_matches_requested_text(&node, "venus"));
    }

    #[test]
    fn node_matches_requested_text_rejects_non_exact_partial_matches() {
        let node = NormalizedUiNode {
            class_name: Some("android.widget.EditText".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("marsmoon".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: None,
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: true,
            scrollable: false,
            long_clickable: true,
            bounds: None,
            center: None,
        };
        assert!(!node_matches_requested_text(&node, "mars"));
    }

    #[test]
    fn retry_with_adb_is_only_used_for_failed_verified_grpc_taps() {
        let verification = TapVerification {
            requested: true,
            wait_until_absent: false,
            wait_for_selector: None,
            satisfied: false,
            stabilized: true,
            stable_polls_observed: 2,
            stable_polls_required: 2,
            timed_out: false,
            elapsed_ms: 250,
            hierarchy_path: None,
            tapped_selector_present_pre_tap: Some(true),
            post_selector_matched_pre_tap: Some(false),
            tapped_selector_still_present: Some(true),
            post_selector_matched: Some(false),
            ui_changed_from_pre_tap: Some(false),
        };
        assert!(should_retry_tap_with_adb("grpc", true, &verification));
        assert!(!should_retry_tap_with_adb("adb", true, &verification));
        assert!(!should_retry_tap_with_adb("grpc", false, &verification));
        assert!(!should_retry_tap_with_adb(
            "grpc",
            true,
            &TapVerification {
                satisfied: true,
                ..verification
            }
        ));
    }

    #[test]
    fn retry_with_adb_is_only_used_for_failed_verified_grpc_text_dispatch() {
        let verification = TextVerification {
            requested: true,
            target_selector: None,
            wait_for_selector: None,
            satisfied: false,
            stabilized: true,
            stable_polls_observed: 2,
            stable_polls_required: 2,
            timed_out: false,
            elapsed_ms: 250,
            hierarchy_path: None,
            target_selector_present_pre_text: Some(true),
            post_selector_matched_pre_text: Some(false),
            target_selector_still_present: Some(true),
            target_text_matches_requested: Some(false),
            post_selector_matched: Some(false),
            ui_changed_from_pre_text: Some(false),
        };
        assert!(should_retry_text_with_adb("grpc", true, &verification));
        assert!(!should_retry_text_with_adb("adb", true, &verification));
        assert!(!should_retry_text_with_adb("grpc", false, &verification));
        assert!(!should_retry_text_with_adb(
            "grpc",
            true,
            &TextVerification {
                satisfied: true,
                ..verification.clone()
            }
        ));
        assert!(!should_retry_text_with_adb(
            "grpc",
            true,
            &TextVerification {
                ui_changed_from_pre_text: Some(true),
                ..verification
            }
        ));
    }

    #[test]
    fn ui_observation_fingerprint_changes_with_window_state() {
        let hierarchy = "<hierarchy><node text=\"Search\" /></hierarchy>";
        let first = AndroidWindowState {
            current_focus: Some("WindowA".to_string()),
            focused_app: Some("com.sednalabs.solarlab".to_string()),
            resumed_activity: Some("MainActivity".to_string()),
        };
        let second = AndroidWindowState {
            current_focus: Some("WindowB".to_string()),
            focused_app: Some("com.sednalabs.solarlab".to_string()),
            resumed_activity: Some("MainActivity".to_string()),
        };

        assert_ne!(
            ui_observation_fingerprint(hierarchy, &first),
            ui_observation_fingerprint(hierarchy, &second)
        );
    }

    #[test]
    fn parses_display_size_from_wm_output() {
        assert_eq!(
            parse_display_size("Physical size: 1080x2400\nOverride size: 1080x2400\n"),
            Some((1080, 2400))
        );
        assert_eq!(
            parse_display_size("Physical size: 1440x3120\nOverride size: 1080x2400\n"),
            Some((1080, 2400))
        );
        assert_eq!(parse_display_size("1080x2400"), Some((1080, 2400)));
        assert_eq!(parse_display_size("nonsense"), None);
    }

    #[test]
    fn parses_android_window_state_from_dumpsys_output() {
        let window_output = r#"
            WINDOW MANAGER WINDOWS (dumpsys window windows)
              mCurrentFocus=Window{123 u0 com.sednalabs.solarlab/com.sednalabs.solarlab.MainActivity}
              mFocusedApp=AppWindowToken{456 token=Token{789 ActivityRecord{abc com.sednalabs.solarlab/.MainActivity}}}
        "#;
        let activity_output = r#"
            topResumedActivity=ActivityRecord{abc u0 com.sednalabs.solarlab/.MainActivity t12}
        "#;
        let state = parse_android_window_state(window_output, activity_output);
        assert_eq!(
            state.current_focus.as_deref(),
            Some("com.sednalabs.solarlab/com.sednalabs.solarlab.MainActivity")
        );
        assert_eq!(
            state.focused_app.as_deref(),
            Some("com.sednalabs.solarlab/.MainActivity")
        );
        assert_eq!(
            state.resumed_activity.as_deref(),
            Some("com.sednalabs.solarlab/.MainActivity")
        );
    }

    #[test]
    fn derives_observed_package_from_resumed_activity_when_focused_app_is_missing() {
        let package = derive_observed_package(&AndroidWindowState {
            current_focus: None,
            focused_app: None,
            resumed_activity: Some("com.sednalabs.solarlab/.MainActivity".to_string()),
        });
        assert_eq!(package.as_deref(), Some("com.sednalabs.solarlab"));
    }

    #[test]
    fn derives_observed_package_from_current_focus_as_fallback() {
        let package = derive_observed_package(&AndroidWindowState {
            current_focus: Some(
                "com.sednalabs.solarlab/com.sednalabs.solarlab.MainActivity".to_string(),
            ),
            focused_app: None,
            resumed_activity: None,
        });
        assert_eq!(package.as_deref(), Some("com.sednalabs.solarlab"));
    }

    #[test]
    fn builds_swipe_points_for_each_direction() {
        assert_eq!(
            swipe_points_for_direction((1080, 2400), ScrollDirection::Up),
            (540, 1800, 540, 600)
        );
        assert_eq!(
            swipe_points_for_direction((1080, 2400), ScrollDirection::Down),
            (540, 600, 540, 1800)
        );
    }

    #[test]
    fn finds_interactive_node_center_for_text_child() {
        let xml = r#"
            <hierarchy>
              <node text="" content-desc="" resource-id="" class="android.widget.FrameLayout" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[0,0][1080,2400]">
                <node text="" content-desc="Search action" resource-id="com.example:id/search_button" class="android.widget.Button" package="com.example" clickable="true" focusable="true" enabled="true" selected="false" checked="false" bounds="[48,96][240,180]">
                  <node text="Search" content-desc="" resource-id="com.example:id/search_label" class="android.widget.TextView" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[96,110][180,150]" />
                </node>
              </node>
            </hierarchy>
        "#;
        let path =
            std::env::temp_dir().join(timestamp_filename("interactive-node-test", Some("xml")));
        std::fs::write(&path, xml).expect("xml should write");
        let selection = find_interactive_ui_node(
            &path,
            &UiSelector {
                text: Some("Search".to_string()),
                ..UiSelector::default()
            },
        )
        .expect("interactive lookup should succeed");
        assert_eq!(selection.matches.len(), 1);
        let node = selection
            .matches
            .first()
            .cloned()
            .expect("interactive node should be found");
        assert_eq!(
            node.bounds,
            Some(UiBounds {
                left: 48,
                top: 96,
                right: 240,
                bottom: 180,
            })
        );
        assert_eq!(node.center, Some((144, 138)));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn button_phrase_matches_clickable_ancestor_by_semantic_label() {
        let xml = r#"
            <hierarchy>
              <node text="" content-desc="" resource-id="" class="android.widget.FrameLayout" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[0,0][1080,2400]">
                <node text="" content-desc="" resource-id="com.example:id/search_button" class="android.view.View" package="com.example" clickable="true" focusable="true" enabled="true" selected="false" checked="false" bounds="[48,96][240,180]">
                  <node text="Search" content-desc="" resource-id="com.example:id/search_label" class="android.widget.TextView" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[96,110][180,150]" />
                </node>
              </node>
            </hierarchy>
        "#;
        let path = std::env::temp_dir().join(timestamp_filename(
            "interactive-node-button-label",
            Some("xml"),
        ));
        std::fs::write(&path, xml).expect("xml should write");
        let selection = find_interactive_ui_node(
            &path,
            &UiSelector {
                label: Some("Search".to_string()),
                label_exact: Some(true),
                clickable: Some(true),
                ..UiSelector::default()
            },
        )
        .expect("interactive lookup should succeed");
        assert_eq!(selection.matches.len(), 1);
        let node = selection
            .matches
            .first()
            .cloned()
            .expect("interactive node should be found");
        assert_eq!(node.semantic_label.as_deref(), Some("Search"));
        assert_eq!(
            node.resource_id.as_deref(),
            Some("com.example:id/search_button")
        );
        assert_eq!(
            node.bounds,
            Some(UiBounds {
                left: 48,
                top: 96,
                right: 240,
                bottom: 180,
            })
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn labeled_text_field_matches_focusable_control_by_placeholder_label() {
        let xml = r#"
            <hierarchy>
              <node text="" content-desc="" resource-id="" class="android.widget.FrameLayout" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[0,0][1080,2400]">
                <node text="" content-desc="" resource-id="com.example:id/search_field" class="android.widget.EditText" package="com.example" clickable="true" focusable="true" enabled="true" selected="false" checked="false" bounds="[120,220][960,360]">
                  <node text="Search by name or id" content-desc="" resource-id="com.example:id/search_placeholder" class="android.widget.TextView" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[160,260][580,312]" />
                </node>
              </node>
            </hierarchy>
        "#;
        let path = std::env::temp_dir().join(timestamp_filename(
            "interactive-node-field-label",
            Some("xml"),
        ));
        std::fs::write(&path, xml).expect("xml should write");
        let selection = find_interactive_ui_node(
            &path,
            &UiSelector {
                label: Some("Search by name or id".to_string()),
                label_exact: Some(true),
                focusable: Some(true),
                ..UiSelector::default()
            },
        )
        .expect("interactive lookup should succeed");
        assert_eq!(selection.matches.len(), 1);
        let node = selection
            .matches
            .first()
            .cloned()
            .expect("interactive node should be found");
        assert_eq!(node.semantic_label.as_deref(), Some("Search by name or id"));
        assert_eq!(
            node.resource_id.as_deref(),
            Some("com.example:id/search_field")
        );
        assert_eq!(
            node.bounds,
            Some(UiBounds {
                left: 120,
                top: 220,
                right: 960,
                bottom: 360,
            })
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn labeled_text_field_matches_typed_focusable_control_by_placeholder_descendant() {
        let xml = r#"
            <hierarchy>
              <node text="" content-desc="" resource-id="" class="android.widget.FrameLayout" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[0,0][1080,2400]">
                <node text="mars" content-desc="" resource-id="" class="android.widget.EditText" package="com.example" clickable="true" focusable="true" enabled="true" selected="false" checked="false" focused="true" long-clickable="true" bounds="[120,220][960,360]">
                  <node text="Search by name or id" content-desc="" resource-id="" class="android.widget.TextView" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[160,260][580,312]" />
                </node>
              </node>
            </hierarchy>
        "#;
        let path = std::env::temp_dir().join(timestamp_filename(
            "interactive-node-typed-field-label",
            Some("xml"),
        ));
        std::fs::write(&path, xml).expect("xml should write");
        let selection = find_interactive_ui_node(
            &path,
            &UiSelector {
                label: Some("Search by name or id".to_string()),
                label_exact: Some(true),
                focusable: Some(true),
                ..UiSelector::default()
            },
        )
        .expect("interactive lookup should succeed");
        assert_eq!(selection.matches.len(), 1);
        let node = selection
            .matches
            .first()
            .cloned()
            .expect("interactive node should be found");
        assert_eq!(node.text.as_deref(), Some("mars"));
        assert_eq!(node.semantic_label.as_deref(), Some("mars"));
        assert_eq!(node.resource_id, None);
        assert_eq!(
            node.bounds,
            Some(UiBounds {
                left: 120,
                top: 220,
                right: 960,
                bottom: 360,
            })
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn descendant_placeholder_label_does_not_bypass_non_label_selector_requirements() {
        let xml = r#"
            <hierarchy>
              <node text="" content-desc="" resource-id="" class="android.widget.FrameLayout" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[0,0][1080,2400]">
                <node text="mars" content-desc="" resource-id="" class="android.widget.EditText" package="com.example" clickable="true" focusable="false" enabled="true" selected="false" checked="false" focused="true" long-clickable="true" bounds="[120,220][960,360]">
                  <node text="Search by name or id" content-desc="" resource-id="" class="android.widget.TextView" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[160,260][580,312]" />
                </node>
              </node>
            </hierarchy>
        "#;
        let path = std::env::temp_dir().join(timestamp_filename(
            "interactive-node-typed-field-label-negative",
            Some("xml"),
        ));
        std::fs::write(&path, xml).expect("xml should write");
        let selection = find_interactive_ui_node(
            &path,
            &UiSelector {
                label: Some("Search by name or id".to_string()),
                label_exact: Some(true),
                focusable: Some(true),
                ..UiSelector::default()
            },
        )
        .expect("interactive lookup should succeed");
        assert!(selection.matches.is_empty());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn interactive_lookup_reports_ambiguity_count() {
        let xml = r#"
            <hierarchy>
              <node text="" content-desc="" resource-id="" class="android.widget.FrameLayout" package="com.example" clickable="false" focusable="false" enabled="true" selected="false" checked="false" bounds="[0,0][1080,2400]">
                <node text="Search" content-desc="" resource-id="com.example:id/search_one" class="android.widget.Button" package="com.example" clickable="true" focusable="true" enabled="true" selected="false" checked="false" bounds="[48,96][240,180]" />
                <node text="Search" content-desc="" resource-id="com.example:id/search_two" class="android.widget.Button" package="com.example" clickable="true" focusable="true" enabled="true" selected="false" checked="false" bounds="[300,96][492,180]" />
              </node>
            </hierarchy>
        "#;
        let path = std::env::temp_dir().join(timestamp_filename(
            "interactive-node-ambiguous",
            Some("xml"),
        ));
        std::fs::write(&path, xml).expect("xml should write");
        let selection = find_interactive_ui_node(
            &path,
            &UiSelector {
                text: Some("Search".to_string()),
                ..UiSelector::default()
            },
        )
        .expect("interactive lookup should succeed");
        assert_eq!(selection.matches.len(), 2);
        assert_eq!(
            selection
                .matches
                .first()
                .as_ref()
                .and_then(|node| node.resource_id.as_deref()),
            Some("com.example:id/search_one")
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn resolve_node_selection_requires_unique_match_without_index() {
        let first = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/search_one".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        let second = NormalizedUiNode {
            resource_id: Some("com.example:id/search_two".to_string()),
            bounds: Some(UiBounds {
                left: 120,
                top: 0,
                right: 220,
                bottom: 40,
            }),
            center: Some((170, 20)),
            ..first.clone()
        };
        let error = resolve_node_selection(vec![first, second], None)
            .expect_err("ambiguous matches should fail without an explicit index");
        match error {
            SelectionFailure::Ambiguous {
                match_count,
                candidates,
            } => {
                assert_eq!(match_count, 2);
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("unexpected selection failure: {other:?}"),
        }
    }

    #[test]
    fn resolve_node_selection_accepts_explicit_match_index() {
        let first = NormalizedUiNode {
            class_name: Some("android.widget.Button".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: Some("com.example:id/search_one".to_string()),
            clickable: true,
            focusable: true,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: Some(UiBounds {
                left: 0,
                top: 0,
                right: 100,
                bottom: 40,
            }),
            center: Some((50, 20)),
        };
        let second = NormalizedUiNode {
            resource_id: Some("com.example:id/search_two".to_string()),
            bounds: Some(UiBounds {
                left: 120,
                top: 0,
                right: 220,
                bottom: 40,
            }),
            center: Some((170, 20)),
            ..first.clone()
        };
        let selection = resolve_node_selection(vec![first, second], Some(1))
            .expect("explicit index should select the requested candidate");
        assert_eq!(selection.selected_match_index, 1);
        assert_eq!(
            selection.node.resource_id.as_deref(),
            Some("com.example:id/search_two")
        );
    }

    #[test]
    fn tool_postcondition_helper_rejects_unsatisfied_requested_postcondition() {
        let error = ensure_tool_postcondition_satisfied(
            "android.input.tap",
            "postcondition failed after dispatch",
            &ToolPostconditionResult {
                requested: true,
                satisfied: false,
                timed_out: true,
                elapsed_ms: 500,
                hierarchy_path: Some("/tmp/raw-tap.xml".to_string()),
                screenshot_path: None,
                observed_activity: None,
                observed_package: None,
                node: None,
                match_count: 0,
                selected_match_index: None,
                candidate_summary: Vec::new(),
            },
        )
        .expect_err("unsatisfied requested postconditions should hard-fail");
        let message = error.to_string();
        assert!(message.contains("android.input.tap postcondition failed after dispatch"));
        assert!(message.contains("\"requested\":true"));
        assert!(message.contains("\"satisfied\":false"));
    }

    #[test]
    fn tool_postcondition_helper_ignores_unrequested_postconditions() {
        ensure_tool_postcondition_satisfied(
            "android.input.keyevent",
            "postcondition failed after dispatch",
            &ToolPostconditionResult {
                requested: false,
                satisfied: true,
                timed_out: false,
                elapsed_ms: 0,
                hierarchy_path: None,
                screenshot_path: None,
                observed_activity: None,
                observed_package: None,
                node: None,
                match_count: 0,
                selected_match_index: None,
                candidate_summary: Vec::new(),
            },
        )
        .expect("unrequested postconditions should not error");
    }

    #[test]
    fn action_outcome_helper_rejects_unsatisfied_requested_outcomes() {
        let error = ensure_action_outcome_satisfied(
            "android.input.swipe",
            "postcondition failed after dispatch",
            true,
            false,
            json!({
                "postcondition": {
                    "requested": true,
                    "satisfied": false,
                },
                "scroll_changed": false,
            }),
        )
        .expect_err("requested outcomes should hard-fail when unsatisfied");
        let message = error.to_string();
        assert!(message.contains("android.input.swipe postcondition failed after dispatch"));
        assert!(message.contains("\"scroll_changed\":false"));
    }

    #[test]
    fn classify_system_dialog_prefers_safe_anr_and_permission_actions() {
        let anr = classify_system_dialog(&[
            "System UI isn't responding".to_string(),
            "Wait".to_string(),
            "Close app".to_string(),
        ])
        .expect("anr dialog should be detected");
        assert_eq!(anr.kind, "anr");
        assert_eq!(anr.action_label, Some("Wait"));

        let permission = classify_system_dialog(&[
            "Allow location permission".to_string(),
            "While using the app".to_string(),
            "Don't allow".to_string(),
        ])
        .expect("permission dialog should be detected");
        assert_eq!(permission.kind, "permission");
        assert_eq!(permission.action_label, Some("While using the app"));

        let crash = classify_system_dialog(&[
            "Solar Lab keeps stopping".to_string(),
            "Close app".to_string(),
        ])
        .expect("crash dialog should be detected");
        assert_eq!(crash.kind, "crash");
        assert_eq!(crash.action_label, None);
    }

    #[test]
    fn empty_selector_is_rejected() {
        let err = ensure_selector_not_empty(&UiSelector::default())
            .expect_err("selector should be rejected");
        assert!(
            err.to_string()
                .contains("selector must include at least one field")
        );
    }

    #[test]
    fn state_only_selector_is_accepted() {
        ensure_selector_not_empty(&UiSelector {
            focused: Some(true),
            ..UiSelector::default()
        })
        .expect("focused-only selector should be accepted");
        ensure_selector_not_empty(&UiSelector {
            scrollable: Some(true),
            ..UiSelector::default()
        })
        .expect("scrollable-only selector should be accepted");
        ensure_selector_not_empty(&UiSelector {
            long_clickable: Some(false),
            ..UiSelector::default()
        })
        .expect("long-clickable-only selector should be accepted");
        ensure_selector_not_empty(&UiSelector {
            focusable: Some(true),
            ..UiSelector::default()
        })
        .expect("focusable-only selector should be accepted");
    }

    #[test]
    fn normalize_artifact_name_strips_parent_components() {
        assert_eq!(
            normalize_artifact_name("artifacts/uiautomator/after-earth-focus.xml", Some("xml")),
            Some("after-earth-focus.xml".to_string())
        );
    }

    #[test]
    fn normalize_artifact_name_adds_missing_extension() {
        assert_eq!(
            normalize_artifact_name("after-earth-focus", Some("xml")),
            Some("after-earth-focus.xml".to_string())
        );
        assert_eq!(
            normalize_artifact_name("logcat/latest", Some("txt")),
            Some("latest.txt".to_string())
        );
    }

    #[test]
    fn normalize_artifact_name_rejects_blank_input() {
        assert_eq!(normalize_artifact_name("   ", Some("png")), None);
    }

    #[test]
    fn timestamp_filename_is_unique_across_rapid_calls() {
        let generated = (0..64)
            .map(|_| timestamp_filename("window-dump", Some("xml")))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(generated.len(), 64);
    }

    #[test]
    fn remote_ui_dump_path_is_unique_across_rapid_calls() {
        let generated = (0..64)
            .map(|_| remote_ui_dump_path())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(generated.len(), 64);
    }

    #[test]
    fn extracts_uiautomator_hierarchy_xml_from_clean_exec_out_payload() {
        let payload = "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation=\"0\"></hierarchy>";
        assert_eq!(extract_uiautomator_hierarchy_xml(payload), Some(payload));
    }

    #[test]
    fn extracts_uiautomator_hierarchy_xml_from_noisy_exec_out_payload() {
        let payload = "UI hierarchy dumped to: /dev/tty\n<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation=\"0\"><node /></hierarchy>\nDone.\n";
        assert_eq!(
            extract_uiautomator_hierarchy_xml(payload),
            Some(
                "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation=\"0\"><node /></hierarchy>"
            )
        );
    }

    #[test]
    fn missing_exec_out_hierarchy_xml_is_rejected() {
        assert_eq!(extract_uiautomator_hierarchy_xml("Killed\n"), None);
    }

    #[test]
    fn unsupported_exec_out_uiautomator_failure_is_retryable_via_legacy_path() {
        let error = McpError::internal_error(
            "adb -s emulator-5554 exec-out uiautomator dump /dev/tty failed with exit status 1: usage: uiautomator",
            None,
        );
        assert!(is_exec_out_uiautomator_supported_failure(&error));
    }

    #[test]
    fn timeout_exec_out_uiautomator_failure_is_not_treated_as_fallback_only() {
        let error = McpError::internal_error(
            "adb -s emulator-5554 exec-out uiautomator dump /dev/tty timed out after 5000 ms",
            None,
        );
        assert!(!is_exec_out_uiautomator_supported_failure(&error));
    }

    #[test]
    fn deadline_limited_observation_timeout_recognizes_hierarchy_capture_timeouts() {
        let exec_out_error = McpError::internal_error(
            "adb -s emulator-5554 exec-out uiautomator dump /dev/tty timed out after 1267 ms",
            None,
        );
        let shell_error = McpError::internal_error(
            "adb -s emulator-5554 shell uiautomator dump /sdcard/window-dump-1.xml timed out after 2500 ms",
            None,
        );
        let pull_error = McpError::internal_error(
            "adb -s emulator-5554 pull /sdcard/window-dump-1.xml local.xml timed out after 1800 ms",
            None,
        );

        assert!(is_deadline_limited_observation_timeout(&exec_out_error));
        assert!(is_deadline_limited_observation_timeout(&shell_error));
        assert!(is_deadline_limited_observation_timeout(&pull_error));
    }

    #[test]
    fn deadline_limited_observation_timeout_ignores_unrelated_timeouts() {
        let error = McpError::internal_error(
            "adb -s emulator-5554 exec-out screencap -p timed out after 1500 ms",
            None,
        );
        assert!(!is_deadline_limited_observation_timeout(&error));
    }

    #[test]
    fn missing_remote_ui_dump_pull_is_retryable() {
        let error = McpError::internal_error(
            "adb pull /sdcard/window-dump.xml local.xml failed with exit status 1: adb: error: failed to stat remote object '/sdcard/window-dump.xml': No such file or directory",
            None,
        );
        assert!(should_retry_ui_dump_pull(&error));
    }

    #[test]
    fn non_missing_remote_ui_dump_pull_is_not_retryable() {
        let error = McpError::internal_error(
            "adb pull /sdcard/window-dump.xml local.xml failed with exit status 1: permission denied",
            None,
        );
        assert!(!should_retry_ui_dump_pull(&error));
    }

    #[tokio::test]
    async fn run_command_with_timeout_rejects_hung_processes() {
        let mut command = Command::new("bash");
        command.args(["-lc", "sleep 1"]);
        let err =
            run_command_with_timeout(command, Duration::from_millis(50), "test timeout command")
                .await
                .expect_err("sleeping command should time out");
        assert!(
            err.to_string().contains("timed out after 50 ms"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn run_command_with_timeout_surfaces_exit_failures() {
        let mut command = Command::new("bash");
        command.args(["-lc", "echo boom >&2; exit 7"]);
        let err = run_command_with_timeout(command, Duration::from_secs(1), "test failure command")
            .await
            .expect_err("failing command should surface an error");
        assert!(
            err.to_string().contains("failed with exit status 7"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn run_command_with_timeout_captures_child_stdout() {
        let mut command = Command::new("bash");
        command.args(["-lc", "printf 'alpha\\n'"]);
        let output =
            run_command_with_timeout(command, Duration::from_secs(1), "test stdout command")
                .await
                .expect("stdout command should succeed");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "alpha\n");
    }

    #[test]
    #[cfg(unix)]
    fn command_failure_message_prefers_stderr_detail() {
        let output = Output {
            status: std::process::ExitStatus::from_raw(7 << 8),
            stdout: Vec::new(),
            stderr: b"boom".to_vec(),
        };
        let rendered = command_failure_message("test failure command", &output);
        assert!(rendered.contains("failed with exit status 7"));
        assert!(rendered.contains("boom"));
    }

    #[test]
    fn invalid_scroll_direction_is_rejected() {
        let err =
            normalize_scroll_direction(Some("sideways")).expect_err("direction should be rejected");
        assert!(
            err.to_string()
                .contains("direction must be one of: up, down")
        );
    }

    #[test]
    fn actionable_center_rejects_missing_center() {
        let node = Some(NormalizedUiNode {
            class_name: Some("android.widget.TextView".to_string()),
            package_name: Some("com.example".to_string()),
            text: Some("Search".to_string()),
            semantic_label: None,
            content_desc: None,
            resource_id: None,
            clickable: false,
            focusable: false,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: None,
            center: None,
        });
        let err = actionable_center(
            &node,
            &UiSelector {
                text: Some("Search".to_string()),
                ..UiSelector::default()
            },
            "tap",
        )
        .expect_err("missing center should be rejected");
        assert!(err.to_string().contains("no actionable bounds"));
    }

    #[test]
    fn bare_string_selector_deserializes_to_text_matcher() {
        let selector: UiSelectorInput =
            serde_json::from_value(json!("Search")).expect("string selector should deserialize");
        assert_eq!(
            normalize_selector_input(selector),
            UiSelector {
                text: Some("Search".to_string()),
                ..UiSelector::default()
            }
        );
    }

    #[test]
    fn button_phrase_selector_deserializes_to_label_and_clickable_matcher() {
        let selector: UiSelectorInput = serde_json::from_value(json!("button labelled Search"))
            .expect("button phrase selector should deserialize");
        assert_eq!(
            normalize_selector_input(selector),
            UiSelector {
                label: Some("Search".to_string()),
                label_exact: Some(true),
                clickable: Some(true),
                ..UiSelector::default()
            }
        );
    }

    #[test]
    fn text_field_phrase_selector_deserializes_to_focusable_matcher() {
        let selector: UiSelectorInput =
            serde_json::from_value(json!("text field")).expect("text field should deserialize");
        assert_eq!(
            normalize_selector_input(selector),
            UiSelector {
                focusable: Some(true),
                ..UiSelector::default()
            }
        );
    }

    #[test]
    fn labeled_text_field_phrase_deserializes_to_label_and_focusable_matcher() {
        let selector: UiSelectorInput =
            serde_json::from_value(json!("text field labelled Search by name or id"))
                .expect("labelled text field selector should deserialize");
        assert_eq!(
            normalize_selector_input(selector),
            UiSelector {
                label: Some("Search by name or id".to_string()),
                label_exact: Some(true),
                focusable: Some(true),
                ..UiSelector::default()
            }
        );
    }

    #[test]
    fn tap_element_args_accept_string_selectors() {
        let args: TapElementArgs = serde_json::from_value(json!({
            "selector": "button labelled Search",
            "wait_for_selector": "text field labelled Search by name or id",
            "timeout_secs": 10
        }))
        .expect("tap args should accept selector strings");
        assert_eq!(
            normalize_selector_input(args.selector),
            UiSelector {
                label: Some("Search".to_string()),
                label_exact: Some(true),
                clickable: Some(true),
                ..UiSelector::default()
            }
        );
        assert_eq!(
            normalize_optional_selector_input(args.wait_for_selector),
            Some(UiSelector {
                label: Some("Search by name or id".to_string()),
                label_exact: Some(true),
                focusable: Some(true),
                ..UiSelector::default()
            })
        );
    }

    #[test]
    fn validates_solarlab_focus_requires_body_query() {
        let err = validate_solarlab_semantic_action("focus_body", None)
            .expect_err("focus_body should require a non-empty body query");
        assert!(err.to_string().contains("requires a non-empty body_query"));
    }

    #[test]
    fn return_to_sandbox_ack_uses_sandbox_specific_matcher() {
        assert_eq!(
            matcher_for_action(&SolarLabSemanticCommand::ReturnToSandbox).description,
            "Search, Immersive, No body selected, or Add object"
        );
    }

    #[test]
    fn return_to_sandbox_ack_accepts_search_surface() {
        let hierarchy = r#"<hierarchy><node text="Search"/><node text="Immersive"/></hierarchy>"#;
        assert!(solarlab_semantic_ack_matches(
            &SolarLabSemanticCommand::ReturnToSandbox,
            hierarchy
        ));
    }

    #[test]
    fn open_immersive_ack_accepts_sparse_non_sandbox_hierarchy() {
        let hierarchy = r#"<hierarchy><node class="android.widget.FrameLayout"/></hierarchy>"#;
        assert!(solarlab_semantic_ack_matches(
            &SolarLabSemanticCommand::OpenImmersive,
            hierarchy
        ));
    }

    #[test]
    fn open_immersive_ack_rejects_sandbox_markers() {
        let hierarchy = r#"<hierarchy><node text="Search"/><node text="Immersive"/><node text="No body selected"/></hierarchy>"#;
        assert!(!solarlab_semantic_ack_matches(
            &SolarLabSemanticCommand::OpenImmersive,
            hierarchy
        ));
    }

    #[test]
    fn tool_schema_snapshot_contract_is_stable() {
        let inventory = build_tool_inventory().expect("inventory should build");
        let tools = inventory.filter_tools(
            AndroidEmulatorMcp::tool_router_android().list_all(),
            ToolOperation::List,
            &ToolInventoryPolicy::strict(),
            |tool| tool.name.as_ref(),
        );
        let snapshot_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("spec/tool_schema_snapshot.v1.json");
        assert_tool_schema_snapshot(snapshot_path, &tools);
    }
}
