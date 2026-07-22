//! Hosted interactive-session helpers for runner-backed Android reuse.
//!
//! ## Rationale
//! Keep GitHub artifact download, active-build state, and live-session relaunch
//! logic out of the core Android tool implementations while still exposing the
//! resulting controls through the same MCP endpoint.

use std::borrow::Cow;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::McpError;
use crate::config::{
    ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION, AndroidExecutionTarget, ExpectedBuildProvenance,
    InteractiveSessionConfig, ResolvedAndroidExecutionTarget,
};
use crate::server::AndroidEmulatorMcp;
use crate::tools::{DEFAULT_ACTION_TIMEOUT_SECS, tool_deadline};
use crate::verification::{
    ToolPostconditionRequest, ensure_tool_postcondition_satisfied, tool_postcondition_json,
};

const GITHUB_API_VERSION: &str = "2022-11-28";
const INSTALL_LAUNCH_PREVIOUS_APP_STATE: &str = "not_running";
const INSTALL_OPERATION_KIND: &str = "install_build_from_run";
const CACHED_ARTIFACT_SHA256_FILE: &str = "interactive-artifact-sha256";

#[derive(Debug, Deserialize)]
pub struct InteractiveSessionInstallBuildArgs {
    pub workflow_run_id: u64,
    pub artifact_name: String,
    #[serde(default)]
    pub repository: Option<String>,
    /// Legacy-only launch flag. Native v1 requests must declare the value in
    /// `install.launch_after_install` so the receipt never infers intent.
    #[serde(default)]
    pub launch_after_install: Option<bool>,
    #[serde(default)]
    pub install: Option<InteractiveSessionInstallOptions>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub contract_version: Option<String>,
    #[serde(default)]
    pub target: Option<AndroidExecutionTarget>,
}

#[derive(JsonSchema)]
struct InteractiveSessionInstallBuildArgsSchema {
    workflow_run_id: u64,
    artifact_name: String,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    launch_after_install: Option<bool>,
    #[serde(default)]
    install: Option<InteractiveSessionInstallOptions>,
    #[serde(default)]
    serial: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    contract_version: Option<String>,
    #[serde(default)]
    target: Option<AndroidExecutionTarget>,
}

impl JsonSchema for InteractiveSessionInstallBuildArgs {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("InteractiveSessionInstallBuildArgs")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = InteractiveSessionInstallBuildArgsSchema::json_schema(generator);
        let object = schema.ensure_object();
        object.insert(
            "title".to_string(),
            Value::String(Self::schema_name().into_owned()),
        );
        object.insert(
            "allOf".to_string(),
            json!([
                {
                    "if": { "required": ["contract_version"] },
                    "then": {
                        "properties": {
                            "contract_version": { "const": ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION },
                            "launch_after_install": { "type": "null" },
                            "target": { "required": ["expected_build"] },
                        },
                        "required": ["target", "install"],
                    },
                    "else": {
                        "properties": {
                            "install": { "type": "null" },
                        },
                    },
                }
            ]),
        );
        schema
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InteractiveSessionInstallOptions {
    pub launch_after_install: bool,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct InteractiveSessionRelaunchArgs {
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BuildManifest {
    schema_version: u64,
    artifact_name: String,
    repository: String,
    workflow: String,
    run_id: String,
    run_attempt: String,
    checkout_ref: String,
    commit_sha: String,
    android_validation_mode: String,
    interactive_debug_profile: String,
    package_name: String,
    activity_name: String,
    version_name: String,
    apk_filename: String,
    apk_sha256: String,
    built_at: String,
}

#[derive(Debug, Clone)]
struct DownloadedBuildBundle {
    manifest: BuildManifest,
    artifact_sha256: String,
    manifest_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallLaunchMode {
    InstallAndLaunch,
    InstallOnly,
    LaunchExisting,
}

impl InstallLaunchMode {
    fn installs_apk(self) -> bool {
        matches!(self, Self::InstallAndLaunch | Self::InstallOnly)
    }

    fn launches_app(self) -> bool {
        matches!(self, Self::InstallAndLaunch | Self::LaunchExisting)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActiveBuildState {
    schema_version: u64,
    status: String,
    activated_at_iso: String,
    manifest: BuildManifest,
    #[serde(default)]
    artifact_sha256: Option<String>,
    #[serde(default)]
    manifest_sha256: Option<String>,
    #[serde(default)]
    resolved_target: Option<ResolvedAndroidExecutionTarget>,
    proof: serde_json::Value,
    preflight: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GitHubArtifactList {
    artifacts: Vec<GitHubArtifact>,
}

#[derive(Debug, Deserialize)]
struct GitHubArtifact {
    #[allow(dead_code)]
    id: u64,
    name: String,
    expired: bool,
    archive_download_url: String,
}

impl AndroidEmulatorMcp {
    pub(crate) async fn interactive_session_status_json(
        &self,
    ) -> Result<serde_json::Value, McpError> {
        let config = self.interactive_session_config()?;
        let active_build = self.read_active_build_state(config).await?;
        Ok(json!({
            "ok": true,
            "session_root": config.session_root.display().to_string(),
            "github_repository": config.github_repository,
            "app_package": config.app_package,
            "app_activity": config.app_activity,
            "github_token_configured": config.github_token.is_some(),
            "active_build": active_build,
        }))
    }

    pub(crate) async fn interactive_session_current_build_json(
        &self,
    ) -> Result<serde_json::Value, McpError> {
        let config = self.interactive_session_config()?;
        let active_build = self.read_active_build_state(config).await?;
        Ok(json!({
            "ok": active_build.is_some(),
            "active_build": active_build,
        }))
    }

    pub(crate) async fn interactive_session_relaunch_current_build_json(
        &self,
        args: &InteractiveSessionRelaunchArgs,
    ) -> Result<serde_json::Value, McpError> {
        let config = self.interactive_session_config()?;
        let active_build = self.read_active_build_state(config).await?.ok_or_else(|| {
            McpError::invalid_params("No active-build.json is available for relaunch", None)
        })?;
        let (artifact_sha256, manifest_sha256, resolved_target) =
            active_build_relaunch_identity(&active_build, args.serial.as_deref())?;
        self.install_or_launch_manifest(
            &active_build.manifest,
            Some(artifact_sha256),
            Some(manifest_sha256),
            Some(resolved_target),
            Some(&resolved_target.device_serial),
            InstallLaunchMode::LaunchExisting,
            args.timeout_secs,
        )
        .await
    }

    pub(crate) async fn interactive_session_install_build_from_run_json(
        &self,
        args: &InteractiveSessionInstallBuildArgs,
    ) -> Result<serde_json::Value, McpError> {
        let native_v1_requested = args.contract_version.as_deref()
            == Some(ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION);
        if args.artifact_name.trim().is_empty() {
            if native_v1_requested {
                return Ok(native_validation_failure_response(
                    None,
                    None,
                    args.install.as_ref().map(|install| install.launch_after_install).unwrap_or(false),
                    "artifact_name must not be empty",
                ));
            }
            return Err(McpError::invalid_params(
                "artifact_name must not be empty",
                None,
            ));
        }
        let config = match self.interactive_session_config() {
            Ok(config) => config,
            Err(err) if native_v1_requested => {
                return Ok(native_validation_failure_response(
                    None,
                    None,
                    args.install.as_ref().map(|install| install.launch_after_install).unwrap_or(false),
                    &err.to_string(),
                ));
            }
            Err(err) => return Err(err),
        };
        let compatibility_mode = match compatibility_mode(args) {
            Ok(mode) => mode,
            Err(err) if native_v1_requested => {
                return Ok(native_validation_failure_response(
                    None,
                    None,
                    args.install.as_ref().map(|install| install.launch_after_install).unwrap_or(false),
                    &err.to_string(),
                ));
            }
            Err(err) => return Err(err),
        };
        let launch_after_install = match normalized_launch_after_install(args, compatibility_mode) {
            Ok(launch_after_install) => launch_after_install,
            Err(err) if compatibility_mode == "native-v1" => {
                return Ok(native_validation_failure_response(None, None, false, &err.to_string()));
            }
            Err(err) => return Err(err),
        };
        let repository = args
            .repository
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(config.github_repository.as_str())
            .to_string();
        let requested_build = match requested_build_for_install(args, &repository) {
            Ok(requested_build) => requested_build,
            Err(err) if compatibility_mode == "native-v1" => {
                return Ok(native_validation_failure_response(
                    None,
                    None,
                    launch_after_install,
                    &err.to_string(),
                ));
            }
            Err(err) => return Err(err),
        };
        let resolved_target = match self
            .resolve_android_execution_target(args.target.as_ref(), args.serial.as_deref())
            .await
        {
            Ok(resolved_target) => resolved_target,
            Err(err) if compatibility_mode == "native-v1" => {
                return Ok(native_validation_failure_response(
                    None,
                    requested_build.as_ref(),
                    launch_after_install,
                    &err.to_string(),
                ));
            }
            Err(err) => return Err(err),
        };
        let downloaded = match self
            .download_build_bundle(
                config,
                &repository,
                args.workflow_run_id,
                args.artifact_name.trim(),
            )
            .await
        {
            Ok(downloaded) => downloaded,
            Err(err) => {
                return Ok(install_download_failure_response(
                    compatibility_mode,
                    &resolved_target,
                    requested_build.as_ref(),
                    launch_after_install,
                    &err.to_string(),
                ));
            }
        };
        let requested_build = requested_build.unwrap_or_else(|| ExpectedBuildProvenance {
            repository: downloaded.manifest.repository.clone(),
            commit_sha: downloaded.manifest.commit_sha.clone(),
            workflow_run_id: downloaded
                .manifest
                .run_id
                .parse()
                .unwrap_or(args.workflow_run_id),
            artifact_name: downloaded.manifest.artifact_name.clone(),
            artifact_sha256: downloaded.artifact_sha256.clone(),
        });
        if !observed_build_matches_requested(&requested_build, &downloaded) {
            return Ok(install_receipt_response(
                json!({ "ok": false, "serial": resolved_target.device_serial.clone() }),
                compatibility_mode,
                &resolved_target,
                provenance_mismatch_receipt(&requested_build, &downloaded, launch_after_install),
                Some(json!({
                    "kind": "build_provenance_mismatch",
                    "retryability": "do_not_replay",
                    "requested": requested_build_json(&requested_build),
                    "observed": observed_build_json(&downloaded),
                })),
            ));
        }
        let current = match self.read_active_build_state(config).await {
            Ok(current) => current,
            Err(err) => {
                return Ok(install_receipt_response(
                    json!({ "ok": false, "serial": resolved_target.device_serial.clone() }),
                    compatibility_mode,
                    &resolved_target,
                    install_failed_receipt(Some(&requested_build), launch_after_install),
                    Some(json!({
                        "kind": "install_failed",
                        "retryability": "do_not_replay",
                        "message": err.to_string(),
                    })),
                ));
            }
        };
        let already_active =
            active_build_matches_downloaded(current.as_ref(), &downloaded, &resolved_target);
        let raw_result = if already_active && !launch_after_install {
            reused_active_build_result(&resolved_target.device_serial, &downloaded.manifest)
        } else {
            let mode = match (already_active, launch_after_install) {
                (true, true) => InstallLaunchMode::LaunchExisting,
                (false, true) => InstallLaunchMode::InstallAndLaunch,
                (false, false) => InstallLaunchMode::InstallOnly,
                (true, false) => unreachable!("reused active build returned above"),
            };
            match self
                .install_or_launch_manifest(
                    &downloaded.manifest,
                    Some(&downloaded.artifact_sha256),
                    Some(&downloaded.manifest_sha256),
                    Some(&resolved_target),
                    Some(&resolved_target.device_serial),
                    mode,
                    args.timeout_secs,
                )
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    return Ok(install_receipt_response(
                        json!({ "ok": false, "serial": resolved_target.device_serial.clone() }),
                        compatibility_mode,
                        &resolved_target,
                        install_failed_receipt(Some(&requested_build), launch_after_install),
                        Some(json!({
                            "kind": "install_failed",
                            "retryability": "do_not_replay",
                            "message": err.to_string(),
                        })),
                    ));
                }
            }
        };
        Ok(finish_install_build_from_run_response(
            raw_result,
            compatibility_mode,
            &resolved_target,
            &requested_build,
            &downloaded,
            launch_after_install,
        ))
    }

    fn interactive_session_config(&self) -> Result<&InteractiveSessionConfig, McpError> {
        self.config.interactive_session.as_ref().ok_or_else(|| {
            McpError::invalid_params(
                "interactive session controls are not configured for this server",
                None,
            )
        })
    }

    async fn read_active_build_state(
        &self,
        config: &InteractiveSessionConfig,
    ) -> Result<Option<ActiveBuildState>, McpError> {
        let path = config.session_root.join("active-build.json");
        if !path.is_file() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let state = serde_json::from_str(&content)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        Ok(Some(state))
    }

    async fn download_build_bundle(
        &self,
        config: &InteractiveSessionConfig,
        repository: &str,
        workflow_run_id: u64,
        artifact_name: &str,
    ) -> Result<DownloadedBuildBundle, McpError> {
        let token = config.github_token.as_deref().ok_or_else(|| {
            McpError::invalid_params(
                "interactive session download requires ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_TOKEN",
                None,
            )
        })?;
        let client = github_client(token)?;
        let artifacts: GitHubArtifactList = client
            .get(format!(
                "https://api.github.com/repos/{repository}/actions/runs/{workflow_run_id}/artifacts?per_page=100"
            ))
            .send()
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?
            .error_for_status()
            .map_err(|err| McpError::internal_error(err.to_string(), None))?
            .json()
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let artifact = artifacts
            .artifacts
            .into_iter()
            .find(|candidate| candidate.name == artifact_name && !candidate.expired)
            .ok_or_else(|| {
                McpError::invalid_params(
                    format!(
                        "artifact {artifact_name} was not found on workflow run {workflow_run_id}"
                    ),
                    None,
                )
            })?;
        let bytes = client
            .get(&artifact.archive_download_url)
            .send()
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?
            .error_for_status()
            .map_err(|err| McpError::internal_error(err.to_string(), None))?
            .bytes()
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let build_dir = config
            .session_root
            .join("build-cache")
            .join(format!("run-{workflow_run_id}"))
            .join(artifact_name);
        let artifact_sha256 = sha256_prefixed(&bytes);
        extract_artifact_archive(&build_dir, &bytes)?;
        fs::write(
            build_dir.join(CACHED_ARTIFACT_SHA256_FILE),
            format!("{artifact_sha256}\n"),
        )
        .await
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let manifest_path = find_single_file(&build_dir, "interactive-build-manifest.json")?;
        let manifest_bytes = fs::read(&manifest_path)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let manifest: BuildManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let apk_path = manifest_apk_path(&build_dir, &manifest)?;
        verify_sha256(&apk_path, &manifest.apk_sha256).await?;
        if manifest.package_name != config.app_package
            || manifest.activity_name != config.app_activity
        {
            return Err(McpError::invalid_params(
                format!(
                    "artifact {} targets {}/{} but the session expects {}/{}",
                    artifact_name,
                    manifest.package_name,
                    manifest.activity_name,
                    config.app_package,
                    config.app_activity,
                ),
                None,
            ));
        }
        Ok(DownloadedBuildBundle {
            manifest,
            artifact_sha256,
            manifest_sha256: sha256_prefixed(&manifest_bytes),
        })
    }

    async fn install_or_launch_manifest(
        &self,
        manifest: &BuildManifest,
        artifact_sha256: Option<&str>,
        manifest_sha256: Option<&str>,
        resolved_target: Option<&ResolvedAndroidExecutionTarget>,
        serial_hint: Option<&str>,
        mode: InstallLaunchMode,
        timeout_secs: Option<u64>,
    ) -> Result<serde_json::Value, McpError> {
        let config = self.interactive_session_config()?;
        let serial = self.resolve_serial_for_tools(serial_hint).await?;
        let build_dir = resolve_bundle_dir(config, manifest, artifact_sha256, manifest_sha256).await?;
        let apk_path = manifest_apk_path(&build_dir, manifest)?;
        let mut previous_app_state = if mode.launches_app() {
            Some(
                self.observe_app_state_before_install(&serial, manifest.package_name.as_str())
                    .await?,
            )
        } else {
            None
        };
        let mut install_stdout = String::new();
        let mut install_stderr = String::new();
        let mut uninstalled_existing_package = false;
        let mut post_install_persistence_errors = Vec::new();
        if mode.installs_apk() {
            let output =
                install_apk_with_signature_mismatch_fallback(self, &serial, &apk_path, manifest)
                    .await?;
            install_stdout = output.install_stdout;
            install_stderr = output.install_stderr;
            uninstalled_existing_package = output.uninstalled_existing_package;
            if let Err(err) = self
                .write_active_build_state(
                    config,
                    manifest,
                    artifact_sha256,
                    manifest_sha256,
                    resolved_target,
                    None,
                )
                .await
            {
                post_install_persistence_errors
                    .push(post_install_persistence_error("active_build_state", &err));
            }
        }
        let (launch_stdout, launch_stderr, postcondition, launch_error) = if mode.launches_app() {
            let force_stop_result = self
                .run_adb_shell(
                    &serial,
                    ["am", "force-stop", manifest.package_name.as_str()],
                )
                .await;
            match force_stop_result {
                Err(err) => (String::new(), String::new(), None, Some(err.to_string())),
                Ok(_) => {
                    previous_app_state = Some(INSTALL_LAUNCH_PREVIOUS_APP_STATE);
                    let launch_result = async {
                        let output = self
                            .run_adb_shell(
                                &serial,
                                [
                                    "am",
                                    "start",
                                    "-n",
                                    &format!(
                                        "{}/{}",
                                        manifest.package_name, manifest.activity_name
                                    ),
                                ],
                            )
                            .await?;
                        let deadline = tool_deadline(timeout_secs, DEFAULT_ACTION_TIMEOUT_SECS);
                        let postcondition = self
                            .wait_for_tool_postcondition(ToolPostconditionRequest {
                                serial: &serial,
                                selector: None,
                                match_index: None,
                                wait_for_activity: Some(manifest.activity_name.as_str()),
                                wait_for_package: Some(manifest.package_name.as_str()),
                                deadline,
                                include_screenshot: false,
                                artifact_prefix: "interactive-session-install-build",
                            })
                            .await?;
                        ensure_tool_postcondition_satisfied(
                            "interactive_session.install_build_from_run",
                            "postcondition failed after relaunch",
                            &postcondition,
                        )?;
                        Ok::<_, McpError>((output.stdout, output.stderr, postcondition))
                    }
                    .await;
                    match launch_result {
                        Ok((stdout, stderr, postcondition)) => {
                            (stdout, stderr, Some(postcondition), None)
                        }
                        Err(err) => (String::new(), String::new(), None, Some(err.to_string())),
                    }
                }
            }
        } else {
            (String::new(), String::new(), None, None)
        };
        if mode.installs_apk() {
            if launch_error.is_none() {
                if let Err(err) = self
                    .write_active_build_state(
                        config,
                        manifest,
                        artifact_sha256,
                        manifest_sha256,
                        resolved_target,
                        postcondition.as_ref(),
                    )
                    .await
                {
                    post_install_persistence_errors.push(post_install_persistence_error(
                        "active_build_readiness",
                        &err,
                    ));
                }
            }
            if let Err(err) = self
                .append_install_history(config, manifest, postcondition.as_ref())
                .await
            {
                post_install_persistence_errors
                    .push(post_install_persistence_error("install_history", &err));
            }
        } else if launch_error.is_none() {
            if let Err(err) = self
                .write_active_build_state(
                    config,
                    manifest,
                    artifact_sha256,
                    manifest_sha256,
                    resolved_target,
                    postcondition.as_ref(),
                )
                .await
            {
                post_install_persistence_errors.push(post_install_persistence_error(
                    "active_build_readiness",
                    &err,
                ));
            }
            if let Err(err) = self
                .append_install_history(config, manifest, postcondition.as_ref())
                .await
            {
                post_install_persistence_errors
                    .push(post_install_persistence_error("install_history", &err));
            }
        }
        let mut result = json!({
            "ok": launch_error.is_none() && post_install_persistence_errors.is_empty(),
            "serial": serial,
            "installed": mode.installs_apk(),
            "install_performed": mode.installs_apk(),
            "reused_existing_build": matches!(mode, InstallLaunchMode::LaunchExisting),
            "launched": mode.launches_app() && launch_error.is_none(),
            "launch_attempted": mode.launches_app(),
            "previous_app_state": previous_app_state,
            "install_stdout": install_stdout,
            "install_stderr": install_stderr,
            "uninstalled_existing_package": uninstalled_existing_package,
            "apk_path": apk_path.display().to_string(),
            "manifest": manifest,
            "stdout": launch_stdout,
            "stderr": launch_stderr,
            "postcondition": postcondition.as_ref().map(tool_postcondition_json),
        });
        if let Some(launch_error) = launch_error {
            result["launch_error"] = Value::String(launch_error);
        }
        if !post_install_persistence_errors.is_empty() {
            result["post_install_persistence_errors"] =
                Value::Array(post_install_persistence_errors);
        }
        Ok(result)
    }

    async fn observe_app_state_before_install(
        &self,
        serial: &str,
        package_name: &str,
    ) -> Result<&'static str, McpError> {
        let output = self
            .run_adb_allow_failure(serial, ["shell", "pidof", package_name])
            .await?;
        if !output.stderr.trim().is_empty() {
            return Err(McpError::internal_error(
                format!(
                    "could not observe the app state for package {package_name}: {}",
                    output.stderr.trim()
                ),
                None,
            ));
        }
        if output.stdout.trim().is_empty() {
            Ok(INSTALL_LAUNCH_PREVIOUS_APP_STATE)
        } else {
            Ok("running")
        }
    }

    async fn write_active_build_state(
        &self,
        config: &InteractiveSessionConfig,
        manifest: &BuildManifest,
        artifact_sha256: Option<&str>,
        manifest_sha256: Option<&str>,
        resolved_target: Option<&ResolvedAndroidExecutionTarget>,
        postcondition: Option<&crate::verification::ToolPostconditionResult>,
    ) -> Result<(), McpError> {
        let path = config.session_root.join("active-build.json");
        let payload = json!({
            "schema_version": 2,
            "status": if postcondition.is_some() { "ready" } else { "installed" },
            "activated_at_iso": iso_now(),
            "manifest": manifest,
            "artifact_sha256": artifact_sha256,
            "manifest_sha256": manifest_sha256,
            "resolved_target": resolved_target,
            "proof": {
                "postcondition": postcondition.map(tool_postcondition_json),
            },
            "preflight": serde_json::Value::Null,
        });
        write_json_file(&path, &payload).await
    }

    async fn append_install_history(
        &self,
        config: &InteractiveSessionConfig,
        manifest: &BuildManifest,
        postcondition: Option<&crate::verification::ToolPostconditionResult>,
    ) -> Result<(), McpError> {
        let path = config
            .session_root
            .join("install-history")
            .join(format!("{}-tool-install.json", timestamp_slug()));
        let payload = json!({
            "schema_version": 1,
            "recorded_at_iso": iso_now(),
            "status": if postcondition.is_some() { "ready" } else { "installed" },
            "manifest": manifest,
            "postcondition": postcondition.map(tool_postcondition_json),
        });
        write_json_file(&path, &payload).await
    }
}

struct InstallOutcome {
    install_stdout: String,
    install_stderr: String,
    uninstalled_existing_package: bool,
}

fn active_build_relaunch_identity<'a>(
    active_build: &'a ActiveBuildState,
    requested_serial: Option<&str>,
) -> Result<(&'a str, &'a str, &'a ResolvedAndroidExecutionTarget), McpError> {
    if active_build.schema_version != 2 {
        return Err(McpError::invalid_params(
            "active build state must include exact target and digest identity before relaunch",
            None,
        ));
    }
    let resolved_target = active_build.resolved_target.as_ref().ok_or_else(|| {
        McpError::invalid_params(
            "active build state is missing its resolved Android target",
            None,
        )
    })?;
    let artifact_sha256 = active_build.artifact_sha256.as_deref().ok_or_else(|| {
        McpError::invalid_params(
            "active build state is missing its artifact digest",
            None,
        )
    })?;
    let manifest_sha256 = active_build.manifest_sha256.as_deref().ok_or_else(|| {
        McpError::invalid_params(
            "active build state is missing its manifest digest",
            None,
        )
    })?;
    if let Some(serial) = requested_serial
        && serial != resolved_target.device_serial
    {
        return Err(McpError::invalid_params(
            "relaunch serial must match the active build's resolved Android target",
            None,
        ));
    }
    Ok((artifact_sha256, manifest_sha256, resolved_target))
}

fn compatibility_mode(args: &InteractiveSessionInstallBuildArgs) -> Result<&'static str, McpError> {
    let Some(version) = args.contract_version.as_deref() else {
        return Ok("legacy-translated");
    };
    if version != ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION {
        return Err(McpError::invalid_params(
            format!("contract_version must be {ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION}"),
            None,
        ));
    }
    if args
        .target
        .as_ref()
        .and_then(|target| target.expected_build.as_ref())
        .is_none()
    {
        return Err(McpError::invalid_params(
            "a native v1 install requires target.expected_build",
            None,
        ));
    }
    Ok("native-v1")
}

fn normalized_launch_after_install(
    args: &InteractiveSessionInstallBuildArgs,
    compatibility_mode: &str,
) -> Result<bool, McpError> {
    if compatibility_mode == "native-v1" {
        if args.launch_after_install.is_some() {
            return Err(McpError::invalid_params(
                "a native v1 install must not use launch_after_install; use install.launch_after_install",
                None,
            ));
        }
        return args
            .install
            .as_ref()
            .map(|install| install.launch_after_install)
            .ok_or_else(|| {
                McpError::invalid_params(
                    "a native v1 install requires install.launch_after_install",
                    None,
                )
            });
    }
    if args.install.is_some() {
        return Err(McpError::invalid_params(
            "install is only valid with contract_version android-provider-execution/v1",
            None,
        ));
    }
    Ok(args.launch_after_install.unwrap_or(true))
}

fn requested_build_for_install(
    args: &InteractiveSessionInstallBuildArgs,
    repository: &str,
) -> Result<Option<ExpectedBuildProvenance>, McpError> {
    let Some(expected_build) = args
        .target
        .as_ref()
        .and_then(|target| target.expected_build.as_ref())
    else {
        return Ok(None);
    };
    if expected_build.workflow_run_id != args.workflow_run_id
        || expected_build.artifact_name != args.artifact_name.trim()
        || expected_build.repository != repository
    {
        return Err(McpError::invalid_params(
            "target.expected_build must match workflow_run_id, artifact_name, and repository",
            None,
        ));
    }
    Ok(Some(expected_build.clone()))
}

fn requested_build_json(requested_build: &ExpectedBuildProvenance) -> Value {
    json!({
        "repository": requested_build.repository,
        "commit_sha": requested_build.commit_sha,
        "workflow_run_id": requested_build.workflow_run_id,
        "artifact_name": requested_build.artifact_name,
        "artifact_sha256": requested_build.artifact_sha256,
    })
}

fn observed_build_json(downloaded: &DownloadedBuildBundle) -> Value {
    let manifest = &downloaded.manifest;
    json!({
        "repository": manifest.repository,
        "commit_sha": manifest.commit_sha,
        "workflow_run_id": manifest.run_id,
        "artifact_name": manifest.artifact_name,
        "artifact_sha256": downloaded.artifact_sha256,
        "package_name": manifest.package_name,
        "manifest_sha256": downloaded.manifest_sha256,
    })
}

fn observed_build_matches_requested(
    requested_build: &ExpectedBuildProvenance,
    downloaded: &DownloadedBuildBundle,
) -> bool {
    let manifest = &downloaded.manifest;
    requested_build.repository == manifest.repository
        && requested_build.commit_sha == manifest.commit_sha
        && requested_build.workflow_run_id.to_string() == manifest.run_id
        && requested_build.artifact_name == manifest.artifact_name
        && requested_build.artifact_sha256 == downloaded.artifact_sha256
}

fn active_build_matches_downloaded(
    active_build: Option<&ActiveBuildState>,
    downloaded: &DownloadedBuildBundle,
    resolved_target: &ResolvedAndroidExecutionTarget,
) -> bool {
    let Some(current) = active_build else {
        return false;
    };
    current.schema_version == 2
        && current.manifest == downloaded.manifest
        && current.artifact_sha256.as_deref() == Some(downloaded.artifact_sha256.as_str())
        && current.manifest_sha256.as_deref() == Some(downloaded.manifest_sha256.as_str())
        && current.resolved_target.as_ref() == Some(resolved_target)
}

fn reused_active_build_result(serial: &str, manifest: &BuildManifest) -> Value {
    json!({
        "ok": true,
        "serial": serial,
        "installed": true,
        "install_performed": false,
        "launched": false,
        "reused_existing_build": true,
        "manifest": manifest,
    })
}

fn post_install_persistence_error(phase: &str, error: &McpError) -> Value {
    json!({
        "phase": phase,
        "message": error.to_string(),
    })
}

fn install_download_failure_response(
    compatibility_mode: &str,
    resolved_target: &ResolvedAndroidExecutionTarget,
    requested_build: Option<&ExpectedBuildProvenance>,
    launch_requested: bool,
    message: &str,
) -> Value {
    install_receipt_response(
        json!({ "ok": false }),
        compatibility_mode,
        resolved_target,
        install_failed_receipt(requested_build, launch_requested),
        Some(json!({
            "kind": "install_failed",
            "retryability": "do_not_replay",
            "message": message,
        })),
    )
}

fn native_validation_failure_response(
    resolved_target: Option<&ResolvedAndroidExecutionTarget>,
    requested_build: Option<&ExpectedBuildProvenance>,
    launch_requested: bool,
    message: &str,
) -> Value {
    let mut payload = json!({
        "ok": false,
        "contract_version": ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION,
        "compatibility_mode": "native-v1",
        "operation_kind": INSTALL_OPERATION_KIND,
        "install_receipt": {
            "status": "invalid_request",
            "launch": {
                "requested": launch_requested,
                "attempted": false,
                "performed": false,
            },
        },
        "error": {
            "kind": "invalid_request",
            "retryability": "do_not_replay",
            "message": message,
        },
    });
    if let Some(requested_build) = requested_build {
        payload["install_receipt"]["requested_build"] = requested_build_json(requested_build);
    }
    if let Some(resolved_target) = resolved_target {
        payload["resolved_target"] =
            serde_json::to_value(resolved_target).expect("resolved target must serialize");
    }
    payload
}

fn finish_install_build_from_run_response(
    raw_result: Value,
    compatibility_mode: &str,
    resolved_target: &ResolvedAndroidExecutionTarget,
    requested_build: &ExpectedBuildProvenance,
    downloaded: &DownloadedBuildBundle,
    launch_requested: bool,
) -> Value {
    let launch_failed = raw_result.get("launch_error").is_some();
    let previous_app_state = raw_result.get("previous_app_state").and_then(Value::as_str);
    let persistence_errors = raw_result.get("post_install_persistence_errors").cloned();
    let install_performed = raw_result
        .get("install_performed")
        .and_then(Value::as_bool)
        .or_else(|| raw_result.get("installed").and_then(Value::as_bool))
        .unwrap_or(true);
    let reused_existing_build = raw_result
        .get("reused_existing_build")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let install_receipt = install_terminal_receipt_with_effects(
        requested_build,
        downloaded,
        launch_requested,
        launch_failed,
        previous_app_state,
        install_performed,
        reused_existing_build,
    );
    let error = if launch_failed {
        let mut error = json!({
            "kind": "lifecycle_failed",
            "retryability": "do_not_replay",
            "receipt_path": "install_receipt.launch.lifecycle_receipt",
            "message": raw_result["launch_error"].clone(),
        });
        if let Some(errors) = persistence_errors {
            error["persistence_errors"] = errors;
        }
        Some(error)
    } else {
        persistence_errors.map(|errors| {
            json!({
                "kind": "post_install_persistence_failed",
                "retryability": "do_not_replay",
                "receipt_path": "install_receipt",
                "details": errors,
            })
        })
    };
    install_receipt_response(
        raw_result,
        compatibility_mode,
        resolved_target,
        install_receipt,
        error,
    )
}

fn provenance_mismatch_receipt(
    requested_build: &ExpectedBuildProvenance,
    downloaded: &DownloadedBuildBundle,
    launch_requested: bool,
) -> Value {
    json!({
        "status": "provenance_mismatch",
        "requested_build": requested_build_json(requested_build),
        "installed_build": observed_build_json(downloaded),
        "launch": {
            "requested": launch_requested,
            "attempted": false,
            "performed": false,
        },
    })
}

fn install_terminal_receipt(
    requested_build: &ExpectedBuildProvenance,
    downloaded: &DownloadedBuildBundle,
    launch_requested: bool,
    launch_failed: bool,
    previous_app_state: Option<&str>,
) -> Value {
    install_terminal_receipt_with_effects(
        requested_build,
        downloaded,
        launch_requested,
        launch_failed,
        previous_app_state,
        true,
        false,
    )
}

fn install_terminal_receipt_with_effects(
    requested_build: &ExpectedBuildProvenance,
    downloaded: &DownloadedBuildBundle,
    launch_requested: bool,
    launch_failed: bool,
    previous_app_state: Option<&str>,
    install_performed: bool,
    reused_existing_build: bool,
) -> Value {
    let (status, launch) = if launch_failed {
        (
            if install_performed {
                "installed_launch_failed"
            } else {
                "reused_existing_build_launch_failed"
            },
            json!({
                "requested": true,
                "attempted": true,
                "performed": false,
                "lifecycle_receipt": install_launch_lifecycle_receipt(
                    true,
                    previous_app_state,
                ),
            }),
        )
    } else if launch_requested {
        (
            if install_performed {
                "installed"
            } else {
                "reused_existing_build"
            },
            json!({
                "requested": true,
                "attempted": true,
                "performed": true,
                "lifecycle_receipt": install_launch_lifecycle_receipt(
                    false,
                    previous_app_state,
                ),
            }),
        )
    } else {
        (
            if install_performed {
                "installed"
            } else {
                "reused_existing_build"
            },
            json!({
                "requested": false,
                "attempted": false,
                "performed": false,
            }),
        )
    };
    json!({
        "status": status,
        "requested_build": requested_build_json(requested_build),
        "installed_build": observed_build_json(downloaded),
        "install_performed": install_performed,
        "reused_existing_build": reused_existing_build,
        "launch": launch,
    })
}

fn install_launch_lifecycle_receipt(
    launch_failed: bool,
    previous_app_state: Option<&str>,
) -> Value {
    let previous_app_state = previous_app_state.unwrap_or("unknown");
    if launch_failed {
        json!({
            "action": "launch",
            "status": "failed",
            "previous_app_state": previous_app_state,
            "resulting_app_state": "unknown",
            "retryability": "do_not_replay",
        })
    } else {
        json!({
            "action": "launch",
            "status": "applied",
            "previous_app_state": previous_app_state,
            "resulting_app_state": "running",
        })
    }
}

fn install_receipt_response(
    mut payload: Value,
    compatibility_mode: &str,
    resolved_target: &ResolvedAndroidExecutionTarget,
    install_receipt: Value,
    error: Option<Value>,
) -> Value {
    let object = payload
        .as_object_mut()
        .expect("install payload must be a JSON object");
    object.insert(
        "contract_version".to_string(),
        Value::String(ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION.to_string()),
    );
    object.insert(
        "compatibility_mode".to_string(),
        Value::String(compatibility_mode.to_string()),
    );
    object.insert(
        "operation_kind".to_string(),
        Value::String(INSTALL_OPERATION_KIND.to_string()),
    );
    object.insert(
        "resolved_target".to_string(),
        serde_json::to_value(resolved_target).expect("resolved target must serialize"),
    );
    object.insert("install_receipt".to_string(), install_receipt);
    if let Some(error) = error {
        object.insert("error".to_string(), error);
    }
    payload
}

fn install_failed_receipt(
    requested_build: Option<&ExpectedBuildProvenance>,
    launch_requested: bool,
) -> Value {
    let mut receipt = json!({
        "status": "failed",
        "launch": {
            "requested": launch_requested,
            "attempted": false,
            "performed": false,
        },
    });
    if let Some(requested_build) = requested_build {
        receipt["requested_build"] = requested_build_json(requested_build);
    }
    receipt
}

async fn install_apk_with_signature_mismatch_fallback(
    mcp: &AndroidEmulatorMcp,
    serial: &str,
    apk_path: &Path,
    manifest: &BuildManifest,
) -> Result<InstallOutcome, McpError> {
    let initial = mcp
        .run_adb_allow_failure(
            serial,
            [
                "install".to_string(),
                "-r".to_string(),
                apk_path.display().to_string(),
            ],
        )
        .await?;
    if adb_install_succeeded(&initial.stdout, &initial.stderr) {
        return Ok(InstallOutcome {
            install_stdout: initial.stdout,
            install_stderr: initial.stderr,
            uninstalled_existing_package: false,
        });
    }

    if !adb_install_failed_due_to_signature_mismatch(&initial.stdout, &initial.stderr) {
        return Err(McpError::internal_error(
            format!(
                "adb install did not report success for {}",
                apk_path.display()
            ),
            None,
        ));
    }

    let uninstall = mcp
        .run_adb(
            serial,
            ["uninstall".to_string(), manifest.package_name.clone()],
        )
        .await?;
    if !adb_uninstall_succeeded(&uninstall.stdout, &uninstall.stderr) {
        return Err(McpError::internal_error(
            format!(
                "adb uninstall did not report success for package {} before reinstall",
                manifest.package_name
            ),
            None,
        ));
    }

    let reinstall = mcp
        .run_adb(
            serial,
            [
                "install".to_string(),
                "-r".to_string(),
                apk_path.display().to_string(),
            ],
        )
        .await?;
    if !adb_install_succeeded(&reinstall.stdout, &reinstall.stderr) {
        return Err(McpError::internal_error(
            format!(
                "adb install did not report success for {} after uninstalling {}",
                apk_path.display(),
                manifest.package_name
            ),
            None,
        ));
    }

    Ok(InstallOutcome {
        install_stdout: format!(
            "{}\n[uninstall-before-reinstall]\n{}",
            initial.stdout.trim_end(),
            reinstall.stdout
        )
        .trim()
        .to_string(),
        install_stderr: format!(
            "{}\n[uninstall-before-reinstall]\n{}",
            initial.stderr.trim_end(),
            reinstall.stderr
        )
        .trim()
        .to_string(),
        uninstalled_existing_package: true,
    })
}

fn adb_install_succeeded(stdout: &str, stderr: &str) -> bool {
    stdout.contains("Success") || stderr.contains("Success")
}

fn adb_uninstall_succeeded(stdout: &str, stderr: &str) -> bool {
    stdout.contains("Success") || stderr.contains("Success")
}

fn adb_install_failed_due_to_signature_mismatch(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}");
    combined.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE")
        || combined.contains("signatures do not match newer version")
}

fn github_client(token: &str) -> Result<reqwest::Client, McpError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|err| McpError::internal_error(err.to_string(), None))?,
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("android-computer-use-mcp"));
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static(GITHUB_API_VERSION),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .map_err(|err| McpError::internal_error(err.to_string(), None))
}

fn extract_artifact_archive(build_dir: &Path, bytes: &[u8]) -> Result<(), McpError> {
    if build_dir.exists() {
        std::fs::remove_dir_all(build_dir)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    }
    std::fs::create_dir_all(build_dir)
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let Some(safe_name) = entry.enclosed_name().map(PathBuf::from) else {
            continue;
        };
        let output_path = build_dir.join(safe_name);
        if entry.name().ends_with('/') {
            std::fs::create_dir_all(&output_path)
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
            continue;
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        }
        let mut output = std::fs::File::create(&output_path)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    }
    Ok(())
}

fn find_single_file(root: &Path, filename: &str) -> Result<PathBuf, McpError> {
    let mut matches = Vec::new();
    for entry in walkdir(root)? {
        if entry.file_name().and_then(|value| value.to_str()) == Some(filename) {
            matches.push(entry);
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        count => Err(McpError::internal_error(
            format!(
                "expected exactly one {filename} under {}, found {count}",
                root.display()
            ),
            None,
        )),
    }
}

fn walkdir(root: &Path) -> Result<Vec<PathBuf>, McpError> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?
        {
            let entry = entry.map_err(|err| McpError::internal_error(err.to_string(), None))?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else {
                entries.push(entry_path);
            }
        }
    }
    Ok(entries)
}

fn manifest_apk_path(build_dir: &Path, manifest: &BuildManifest) -> Result<PathBuf, McpError> {
    find_single_file(build_dir, &manifest.apk_filename)
}

async fn resolve_bundle_dir(
    config: &InteractiveSessionConfig,
    manifest: &BuildManifest,
    expected_artifact_sha256: Option<&str>,
    expected_manifest_sha256: Option<&str>,
) -> Result<PathBuf, McpError> {
    let expected_artifact_sha256 = expected_artifact_sha256.ok_or_else(|| {
        McpError::invalid_params(
            "an exact cached artifact digest is required before relaunch",
            None,
        )
    })?;
    let expected_manifest_sha256 = expected_manifest_sha256.ok_or_else(|| {
        McpError::invalid_params(
            "an exact cached manifest digest is required before relaunch",
            None,
        )
    })?;
    let build_cache_root = config.session_root.join("build-cache");
    let manifest_files = walkdir(&build_cache_root)?
        .into_iter()
        .filter(|path| {
            path.file_name().and_then(|value| value.to_str())
                == Some("interactive-build-manifest.json")
        })
        .collect::<Vec<_>>();
    for manifest_path in manifest_files {
        let manifest_bytes = fs::read(&manifest_path)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let candidate: BuildManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        if candidate != *manifest || sha256_prefixed(&manifest_bytes) != expected_manifest_sha256 {
            continue;
        }
        let Some(build_dir) = manifest_path.parent() else {
            continue;
        };
        let cached_artifact_sha256 = fs::read_to_string(build_dir.join(CACHED_ARTIFACT_SHA256_FILE))
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        if cached_artifact_sha256.trim() != expected_artifact_sha256 {
            continue;
        }
        return Ok(build_dir.to_path_buf());
    }
    Err(McpError::internal_error(
        "could not locate a cached bundle matching the active build's exact digests".to_string(),
        None,
    ))
}

async fn verify_sha256(path: &Path, expected: &str) -> Result<(), McpError> {
    let bytes = fs::read(path)
        .await
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    let mut digest = Sha256::new();
    digest.update(&bytes);
    let observed = format!("{:x}", digest.finalize());
    if observed != expected {
        return Err(McpError::internal_error(
            format!(
                "sha256 mismatch for {}: expected {expected}, observed {observed}",
                path.display()
            ),
            None,
        ));
    }
    Ok(())
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("sha256:{:x}", digest.finalize())
}

async fn write_json_file(path: &Path, payload: &serde_json::Value) -> Result<(), McpError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(payload)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?,
    )
    .await
    .map_err(|err| McpError::internal_error(err.to_string(), None))
}

fn iso_now() -> String {
    chrono_like_iso(SystemTimeLike::now())
}

fn timestamp_slug() -> String {
    let now = chrono_like_iso(SystemTimeLike::now());
    now.replace('-', "").replace(':', "")
}

struct SystemTimeLike(std::time::SystemTime);

impl SystemTimeLike {
    fn now() -> Self {
        Self(std::time::SystemTime::now())
    }
}

fn chrono_like_iso(now: SystemTimeLike) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = now.0.into();
    datetime.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION, ActiveBuildState, AndroidExecutionTarget,
        BuildManifest, DownloadedBuildBundle, ExpectedBuildProvenance,
        InteractiveSessionInstallBuildArgs, InteractiveSessionInstallOptions,
        ResolvedAndroidExecutionTarget, active_build_matches_downloaded,
        active_build_relaunch_identity,
        adb_install_failed_due_to_signature_mismatch, adb_install_succeeded, compatibility_mode,
        finish_install_build_from_run_response, install_download_failure_response,
        install_failed_receipt, install_launch_lifecycle_receipt, install_receipt_response,
        install_terminal_receipt, install_terminal_receipt_with_effects,
        native_validation_failure_response, normalized_launch_after_install,
        observed_build_matches_requested, provenance_mismatch_receipt, reused_active_build_result,
    };

    fn expected_build() -> ExpectedBuildProvenance {
        ExpectedBuildProvenance {
            repository: "example/android-app".to_string(),
            commit_sha: "abcdef0123456789".to_string(),
            workflow_run_id: 42,
            artifact_name: "android-build".to_string(),
            artifact_sha256: "sha256:artifact".to_string(),
        }
    }

    fn native_install_args(
        expected_build: Option<ExpectedBuildProvenance>,
    ) -> InteractiveSessionInstallBuildArgs {
        InteractiveSessionInstallBuildArgs {
            repository: None,
            workflow_run_id: 42,
            artifact_name: "android-build".to_string(),
            launch_after_install: None,
            install: Some(InteractiveSessionInstallOptions {
                launch_after_install: true,
            }),
            serial: None,
            timeout_secs: None,
            contract_version: Some(ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION.to_string()),
            target: Some(AndroidExecutionTarget {
                expected_build,
                ..Default::default()
            }),
        }
    }

    fn downloaded_build_bundle() -> DownloadedBuildBundle {
        DownloadedBuildBundle {
            manifest: BuildManifest {
                schema_version: 1,
                artifact_name: "android-build".to_string(),
                repository: "example/android-app".to_string(),
                workflow: "build.yml".to_string(),
                run_id: "42".to_string(),
                run_attempt: "1".to_string(),
                checkout_ref: "main".to_string(),
                commit_sha: "abcdef0123456789".to_string(),
                android_validation_mode: "debug".to_string(),
                interactive_debug_profile: "debug".to_string(),
                package_name: "com.example.app".to_string(),
                activity_name: ".MainActivity".to_string(),
                version_name: "1.0".to_string(),
                apk_filename: "app-debug.apk".to_string(),
                apk_sha256: "apk-sha".to_string(),
                built_at: "2026-07-22T00:00:00Z".to_string(),
            },
            artifact_sha256: "sha256:artifact".to_string(),
            manifest_sha256: "sha256:manifest".to_string(),
        }
    }

    fn resolved_target() -> ResolvedAndroidExecutionTarget {
        ResolvedAndroidExecutionTarget {
            environment_id: "environment-1".to_string(),
            provider_instance_id: "provider-1".to_string(),
            session_id: "session-1".to_string(),
            device_serial: "emulator-5554".to_string(),
            app: None,
        }
    }

    fn active_build_state(downloaded: &DownloadedBuildBundle) -> ActiveBuildState {
        ActiveBuildState {
            schema_version: 2,
            status: "installed".to_string(),
            activated_at_iso: "2026-07-22T00:00:00Z".to_string(),
            manifest: downloaded.manifest.clone(),
            artifact_sha256: Some(downloaded.artifact_sha256.clone()),
            manifest_sha256: Some(downloaded.manifest_sha256.clone()),
            resolved_target: Some(resolved_target()),
            proof: serde_json::Value::Null,
            preflight: None,
        }
    }

    #[test]
    fn detects_signature_mismatch_install_failure() {
        let stderr = "adb: failed to install foo.apk: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: Existing package com.example signatures do not match newer version; ignoring!]";
        assert!(adb_install_failed_due_to_signature_mismatch("", stderr));
    }

    #[test]
    fn install_success_detects_success_token() {
        assert!(adb_install_succeeded("Success", ""));
        assert!(adb_install_succeeded("", "Success"));
        assert!(!adb_install_succeeded(
            "",
            "Failure [INSTALL_FAILED_VERSION_DOWNGRADE]"
        ));
    }

    #[test]
    fn native_contract_requires_a_complete_expected_build() {
        let err = compatibility_mode(&native_install_args(None))
            .expect_err("native installs must carry complete expected provenance");

        assert!(
            err.to_string()
                .contains("a native v1 install requires target.expected_build")
        );
    }

    #[test]
    fn native_contract_requires_explicit_nested_launch_intent() {
        let mut args = native_install_args(Some(expected_build()));
        args.install = None;

        let err = normalized_launch_after_install(
            &args,
            compatibility_mode(&args).expect("complete native target is valid"),
        )
        .expect_err("native installs must not infer launch intent");

        assert!(
            err.to_string()
                .contains("a native v1 install requires install.launch_after_install")
        );
    }

    #[test]
    fn native_contract_rejects_the_legacy_launch_field() {
        let mut args = native_install_args(Some(expected_build()));
        args.launch_after_install = Some(true);
        args.install = Some(InteractiveSessionInstallOptions {
            launch_after_install: false,
        });

        let err = normalized_launch_after_install(
            &args,
            compatibility_mode(&args).expect("complete native target is valid"),
        )
        .expect_err("native installs must not accept the legacy launch field");

        assert!(err.to_string().contains("must not use launch_after_install"));
    }

    #[test]
    fn build_provenance_matches_only_the_exact_downloaded_artifact() {
        let expected = expected_build();
        let downloaded = downloaded_build_bundle();

        assert!(observed_build_matches_requested(&expected, &downloaded));

        let mut mismatched = expected;
        mismatched.commit_sha = "different".to_string();
        assert!(!observed_build_matches_requested(&mismatched, &downloaded));
    }

    #[test]
    fn persisted_active_build_requires_exact_build_and_target_provenance() {
        let downloaded = downloaded_build_bundle();
        let active = active_build_state(&downloaded);

        assert!(active_build_matches_downloaded(
            Some(&active),
            &downloaded,
            &resolved_target(),
        ));

        let mut other_downloaded = downloaded.clone();
        other_downloaded.manifest.commit_sha = "different-commit".to_string();
        assert!(!active_build_matches_downloaded(
            Some(&active),
            &other_downloaded,
            &resolved_target(),
        ));
        let mut other_artifact = downloaded.clone();
        other_artifact.artifact_sha256 = "sha256:other-artifact".to_string();
        assert!(!active_build_matches_downloaded(
            Some(&active),
            &other_artifact,
            &resolved_target(),
        ));
        let mut other_target = resolved_target();
        other_target.session_id = "other-session".to_string();
        assert!(!active_build_matches_downloaded(
            Some(&active),
            &downloaded,
            &other_target,
        ));
        assert!(!active_build_matches_downloaded(
            None,
            &downloaded,
            &resolved_target(),
        ));
    }

    #[test]
    fn relaunch_requires_the_persisted_target_and_complete_digests() {
        let downloaded = downloaded_build_bundle();
        let active = active_build_state(&downloaded);

        let (artifact_sha256, manifest_sha256, target) =
            active_build_relaunch_identity(&active, None).expect("complete active build identity");
        assert_eq!(artifact_sha256, "sha256:artifact");
        assert_eq!(manifest_sha256, "sha256:manifest");
        assert_eq!(target.device_serial, "emulator-5554");
        assert!(active_build_relaunch_identity(&active, Some("emulator-5556")).is_err());

        let mut incomplete = active;
        incomplete.artifact_sha256 = None;
        assert!(active_build_relaunch_identity(&incomplete, None).is_err());
    }

    #[test]
    fn reused_active_build_reports_satisfied_install_state_without_claiming_a_new_install() {
        let expected = expected_build();
        let downloaded = downloaded_build_bundle();
        let response = finish_install_build_from_run_response(
            reused_active_build_result("emulator-5554", &downloaded.manifest),
            "native-v1",
            &resolved_target(),
            &expected,
            &downloaded,
            false,
        );

        assert_eq!(response["installed"], json!(true));
        assert_eq!(response["install_performed"], json!(false));
        assert_eq!(response["reused_existing_build"], json!(true));
        assert_eq!(
            response["install_receipt"],
            install_terminal_receipt_with_effects(
                &expected,
                &downloaded,
                false,
                false,
                None,
                false,
                true,
            ),
        );
    }

    #[test]
    fn reused_active_build_launch_receipt_does_not_claim_a_new_install() {
        let response = finish_install_build_from_run_response(
            json!({
                "ok": true,
                "serial": "emulator-5554",
                "installed": false,
                "install_performed": false,
                "reused_existing_build": true,
                "launched": true,
                "launch_attempted": true,
                "previous_app_state": "not_running",
            }),
            "native-v1",
            &resolved_target(),
            &expected_build(),
            &downloaded_build_bundle(),
            true,
        );

        assert_eq!(response["install_receipt"]["status"], json!("reused_existing_build"));
        assert_eq!(response["install_receipt"]["install_performed"], json!(false));
        assert_eq!(response["install_receipt"]["reused_existing_build"], json!(true));
        assert_eq!(response["install_receipt"]["launch"]["performed"], json!(true));
    }

    #[test]
    fn native_validation_failures_keep_the_versioned_install_envelope() {
        let response = native_validation_failure_response(
            None,
            Some(&expected_build()),
            false,
            "a native v1 install requires install.launch_after_install",
        );

        assert_eq!(response["contract_version"], json!(ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION));
        assert_eq!(response["compatibility_mode"], json!("native-v1"));
        assert_eq!(response["operation_kind"], json!("install_build_from_run"));
        assert_eq!(response["install_receipt"]["status"], json!("invalid_request"));
        assert_eq!(response["error"]["kind"], json!("invalid_request"));
        assert_eq!(response["error"]["retryability"], json!("do_not_replay"));
    }

    #[test]
    fn legacy_download_failure_uses_the_terminal_install_receipt_envelope() {
        assert_eq!(
            install_download_failure_response(
                "legacy-translated",
                &resolved_target(),
                None,
                true,
                "GitHub artifact download authentication is unavailable",
            ),
            json!({
                "ok": false,
                "contract_version": ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION,
                "compatibility_mode": "legacy-translated",
                "operation_kind": "install_build_from_run",
                "resolved_target": {
                    "environment_id": "environment-1",
                    "provider_instance_id": "provider-1",
                    "session_id": "session-1",
                    "device_serial": "emulator-5554",
                },
                "install_receipt": {
                    "status": "failed",
                    "launch": {
                        "requested": true,
                        "attempted": false,
                        "performed": false,
                    },
                },
                "error": {
                    "kind": "install_failed",
                    "retryability": "do_not_replay",
                    "message": "GitHub artifact download authentication is unavailable",
                },
            }),
        );
    }

    #[test]
    fn post_install_persistence_failure_retains_the_installed_launch_effect() {
        let response = finish_install_build_from_run_response(
            json!({
                "ok": false,
                "serial": "emulator-5554",
                "installed": true,
                "launched": true,
                "launch_attempted": true,
                "previous_app_state": "not_running",
                "post_install_persistence_errors": [{
                    "phase": "install_history",
                    "message": "history volume is read-only",
                }],
            }),
            "native-v1",
            &resolved_target(),
            &expected_build(),
            &downloaded_build_bundle(),
            true,
        );

        assert_eq!(
            response,
            json!({
                "ok": false,
                "serial": "emulator-5554",
                "installed": true,
                "launched": true,
                "launch_attempted": true,
                "previous_app_state": "not_running",
                "post_install_persistence_errors": [{
                    "phase": "install_history",
                    "message": "history volume is read-only",
                }],
                "contract_version": ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION,
                "compatibility_mode": "native-v1",
                "operation_kind": "install_build_from_run",
                "resolved_target": {
                    "environment_id": "environment-1",
                    "provider_instance_id": "provider-1",
                    "session_id": "session-1",
                    "device_serial": "emulator-5554",
                },
                "install_receipt": {
                    "status": "installed",
                    "requested_build": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": 42,
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                    },
                    "installed_build": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": "42",
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                        "package_name": "com.example.app",
                        "manifest_sha256": "sha256:manifest",
                    },
                    "launch": {
                        "requested": true,
                        "attempted": true,
                        "performed": true,
                        "lifecycle_receipt": {
                            "action": "launch",
                            "status": "applied",
                            "previous_app_state": "not_running",
                            "resulting_app_state": "running",
                        },
                    },
                },
                "error": {
                    "kind": "post_install_persistence_failed",
                    "retryability": "do_not_replay",
                    "receipt_path": "install_receipt",
                    "details": [{
                        "phase": "install_history",
                        "message": "history volume is read-only",
                    }],
                },
            }),
        );
    }

    #[test]
    fn lifecycle_and_persistence_failures_share_the_canonical_error_envelope() {
        let response = finish_install_build_from_run_response(
            json!({
                "ok": false,
                "serial": "emulator-5554",
                "installed": true,
                "install_performed": true,
                "launch_error": "postcondition failed after relaunch",
                "previous_app_state": "not_running",
                "post_install_persistence_errors": [{
                    "phase": "install_history",
                    "message": "history volume is read-only",
                }],
            }),
            "native-v1",
            &resolved_target(),
            &expected_build(),
            &downloaded_build_bundle(),
            true,
        );

        assert_eq!(response["error"]["kind"], json!("lifecycle_failed"));
        assert_eq!(response["error"]["persistence_errors"], json!([{
            "phase": "install_history",
            "message": "history volume is read-only",
        }]));
    }

    #[test]
    fn install_launch_lifecycle_receipts_preserve_known_prelaunch_state() {
        assert_eq!(
            install_launch_lifecycle_receipt(false, Some("not_running")),
            json!({
                "action": "launch",
                "status": "applied",
                "previous_app_state": "not_running",
                "resulting_app_state": "running",
            }),
        );
        assert_eq!(
            install_launch_lifecycle_receipt(true, Some("not_running")),
            json!({
                "action": "launch",
                "status": "failed",
                "previous_app_state": "not_running",
                "resulting_app_state": "unknown",
                "retryability": "do_not_replay",
            }),
        );
    }

    #[test]
    fn provenance_mismatch_response_preserves_exact_requested_and_observed_builds() {
        let mut requested_build = expected_build();
        requested_build.commit_sha = "different-commit".to_string();
        let downloaded = downloaded_build_bundle();

        assert_eq!(
            install_receipt_response(
                json!({ "ok": false, "serial": "emulator-5554" }),
                "native-v1",
                &resolved_target(),
                provenance_mismatch_receipt(&requested_build, &downloaded, true),
                Some(json!({
                    "kind": "build_provenance_mismatch",
                    "retryability": "do_not_replay",
                    "requested": {
                        "repository": "example/android-app",
                        "commit_sha": "different-commit",
                        "workflow_run_id": 42,
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                    },
                    "observed": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": "42",
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                        "package_name": "com.example.app",
                        "manifest_sha256": "sha256:manifest",
                    },
                })),
            ),
            json!({
                "ok": false,
                "serial": "emulator-5554",
                "contract_version": ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION,
                "compatibility_mode": "native-v1",
                "operation_kind": "install_build_from_run",
                "resolved_target": {
                    "environment_id": "environment-1",
                    "provider_instance_id": "provider-1",
                    "session_id": "session-1",
                    "device_serial": "emulator-5554",
                },
                "install_receipt": {
                    "status": "provenance_mismatch",
                    "requested_build": {
                        "repository": "example/android-app",
                        "commit_sha": "different-commit",
                        "workflow_run_id": 42,
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                    },
                    "installed_build": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": "42",
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                        "package_name": "com.example.app",
                        "manifest_sha256": "sha256:manifest",
                    },
                    "launch": {
                        "requested": true,
                        "attempted": false,
                        "performed": false,
                    },
                },
                "error": {
                    "kind": "build_provenance_mismatch",
                    "retryability": "do_not_replay",
                    "requested": {
                        "repository": "example/android-app",
                        "commit_sha": "different-commit",
                        "workflow_run_id": 42,
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                    },
                    "observed": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": "42",
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                        "package_name": "com.example.app",
                        "manifest_sha256": "sha256:manifest",
                    },
                },
            }),
        );
    }

    #[test]
    fn installed_launch_failed_response_has_only_the_nested_lifecycle_failure() {
        let expected = expected_build();
        let downloaded = downloaded_build_bundle();
        let launch_error = "postcondition failed after relaunch";
        let response = install_receipt_response(
            json!({
                "ok": false,
                "serial": "emulator-5554",
                "launch_error": launch_error,
                "previous_app_state": "not_running",
            }),
            "native-v1",
            &resolved_target(),
            install_terminal_receipt(&expected, &downloaded, true, true, Some("not_running")),
            Some(json!({
                "kind": "lifecycle_failed",
                "retryability": "do_not_replay",
                "receipt_path": "install_receipt.launch.lifecycle_receipt",
                "message": launch_error,
            })),
        );

        assert_eq!(
            response,
            json!({
                "ok": false,
                "serial": "emulator-5554",
                "launch_error": launch_error,
                "previous_app_state": "not_running",
                "contract_version": ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION,
                "compatibility_mode": "native-v1",
                "operation_kind": "install_build_from_run",
                "resolved_target": {
                    "environment_id": "environment-1",
                    "provider_instance_id": "provider-1",
                    "session_id": "session-1",
                    "device_serial": "emulator-5554",
                },
                "install_receipt": {
                    "status": "installed_launch_failed",
                    "requested_build": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": 42,
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                    },
                    "installed_build": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": "42",
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                        "package_name": "com.example.app",
                        "manifest_sha256": "sha256:manifest",
                    },
                    "launch": {
                        "requested": true,
                        "attempted": true,
                        "performed": false,
                        "lifecycle_receipt": {
                            "action": "launch",
                            "status": "failed",
                            "previous_app_state": "not_running",
                            "resulting_app_state": "unknown",
                            "retryability": "do_not_replay",
                        },
                    },
                },
                "error": {
                    "kind": "lifecycle_failed",
                    "retryability": "do_not_replay",
                    "receipt_path": "install_receipt.launch.lifecycle_receipt",
                    "message": launch_error,
                },
            }),
        );
        assert!(response.get("lifecycle_receipt").is_none());
    }

    #[test]
    fn installed_launch_success_and_install_only_receipts_cover_terminal_combinations() {
        let expected = expected_build();
        let downloaded = downloaded_build_bundle();
        let successful_response = install_receipt_response(
            json!({
                "ok": true,
                "serial": "emulator-5554",
                "previous_app_state": "not_running",
            }),
            "native-v1",
            &resolved_target(),
            install_terminal_receipt(&expected, &downloaded, true, false, Some("not_running")),
            None,
        );

        assert_eq!(
            successful_response["operation_kind"],
            json!("install_build_from_run"),
        );

        assert_eq!(
            install_terminal_receipt(&expected, &downloaded, true, false, Some("not_running")),
            json!({
                "status": "installed",
                "requested_build": {
                    "repository": "example/android-app",
                    "commit_sha": "abcdef0123456789",
                    "workflow_run_id": 42,
                    "artifact_name": "android-build",
                    "artifact_sha256": "sha256:artifact",
                },
                "installed_build": {
                    "repository": "example/android-app",
                    "commit_sha": "abcdef0123456789",
                    "workflow_run_id": "42",
                    "artifact_name": "android-build",
                    "artifact_sha256": "sha256:artifact",
                    "package_name": "com.example.app",
                    "manifest_sha256": "sha256:manifest",
                },
                "launch": {
                    "requested": true,
                    "attempted": true,
                    "performed": true,
                    "lifecycle_receipt": {
                        "action": "launch",
                        "status": "applied",
                        "previous_app_state": "not_running",
                        "resulting_app_state": "running",
                    },
                },
            }),
        );
        assert_eq!(
            install_terminal_receipt(&expected, &downloaded, false, false, None),
            json!({
                "status": "installed",
                "requested_build": {
                    "repository": "example/android-app",
                    "commit_sha": "abcdef0123456789",
                    "workflow_run_id": 42,
                    "artifact_name": "android-build",
                    "artifact_sha256": "sha256:artifact",
                },
                "installed_build": {
                    "repository": "example/android-app",
                    "commit_sha": "abcdef0123456789",
                    "workflow_run_id": "42",
                    "artifact_name": "android-build",
                    "artifact_sha256": "sha256:artifact",
                    "package_name": "com.example.app",
                    "manifest_sha256": "sha256:manifest",
                },
                "launch": {
                    "requested": false,
                    "attempted": false,
                    "performed": false,
                },
            }),
        );
    }

    #[test]
    fn failed_install_receipt_keeps_the_requested_build_and_target() {
        let expected = expected_build();

        assert_eq!(
            install_receipt_response(
                json!({ "ok": false }),
                "native-v1",
                &resolved_target(),
                install_failed_receipt(Some(&expected), true),
                Some(json!({
                    "kind": "install_failed",
                    "retryability": "do_not_replay",
                })),
            ),
            json!({
                "ok": false,
                "contract_version": ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION,
                "compatibility_mode": "native-v1",
                "operation_kind": "install_build_from_run",
                "resolved_target": {
                    "environment_id": "environment-1",
                    "provider_instance_id": "provider-1",
                    "session_id": "session-1",
                    "device_serial": "emulator-5554",
                },
                "install_receipt": {
                    "status": "failed",
                    "requested_build": {
                        "repository": "example/android-app",
                        "commit_sha": "abcdef0123456789",
                        "workflow_run_id": 42,
                        "artifact_name": "android-build",
                        "artifact_sha256": "sha256:artifact",
                    },
                    "launch": {
                        "requested": true,
                        "attempted": false,
                        "performed": false,
                    },
                },
                "error": {
                    "kind": "install_failed",
                    "retryability": "do_not_replay",
                },
            }),
        );
    }
}
