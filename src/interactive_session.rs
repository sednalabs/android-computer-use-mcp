//! Hosted interactive-session helpers for runner-backed Android reuse.
//!
//! ## Rationale
//! Keep GitHub artifact download, active-build state, and live-session relaunch
//! logic out of the core Android tool implementations while still exposing the
//! resulting controls through the same MCP endpoint.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARCHIVE_MEMBERS: usize = 1024;
const MAX_ARCHIVE_DEPTH: usize = 12;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_GITHUB_METADATA_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
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
#[allow(dead_code)] // Fields describe the external schema; requests use the public type.
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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
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
    workflow_head_sha: String,
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
    id: u64,
    name: String,
    expired: bool,
}

#[derive(Debug, Deserialize)]
struct GitHubRun {
    id: u64,
    head_sha: String,
    run_attempt: u64,
    repository: GitHubRepository,
}

#[derive(Debug, Deserialize)]
struct GitHubRepository {
    full_name: String,
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
        self.resolve_android_execution_target(
            Some(&target_request(resolved_target)),
            Some(&resolved_target.device_serial),
        )
        .await?;
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
        self.interactive_session_install_build_from_run_inner(args)
            .await
            .map(|mut result| {
                let ok = result["ok"].as_bool().unwrap_or(false);
                if result.get("outcome").is_none() {
                    result["outcome"] =
                        Value::String(if ok { "verified" } else { "not_dispatched" }.to_string());
                }
                if result.get("do_not_replay").is_none() {
                    result["do_not_replay"] = Value::Bool(ok);
                    result["retry_safe"] = Value::Bool(!ok);
                }
                let dispatched = result["install_attempted"].as_bool().unwrap_or(false)
                    || result["install_performed"].as_bool().unwrap_or(false)
                    || result["launch_attempted"].as_bool().unwrap_or(false);
                result["mutation_dispatched"] = Value::Bool(dispatched);
                result
            })
            .map_err(|mut error| {
                // Every propagated error happens before install/launch dispatch.
                // Once a command is attempted the inner implementation retains a
                // phase-aware result instead of losing its side-effect evidence.
                error.data = Some(json!({
                    "phase": "validation",
                    "outcome": "not_dispatched",
                    "mutation_dispatched": false,
                    "retry_safe": true,
                    "do_not_replay": false,
                }));
                error
            })
    }

    async fn interactive_session_install_build_from_run_inner(
        &self,
        args: &InteractiveSessionInstallBuildArgs,
    ) -> Result<serde_json::Value, McpError> {
        let native_v1_requested =
            args.contract_version.as_deref() == Some(ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION);
        if args.artifact_name.trim().is_empty() {
            if native_v1_requested {
                return Ok(native_validation_failure_response(
                    None,
                    None,
                    args.install
                        .as_ref()
                        .map(|install| install.launch_after_install)
                        .unwrap_or(false),
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
                    args.install
                        .as_ref()
                        .map(|install| install.launch_after_install)
                        .unwrap_or(false),
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
                    args.install
                        .as_ref()
                        .map(|install| install.launch_after_install)
                        .unwrap_or(false),
                    &err.to_string(),
                ));
            }
            Err(err) => return Err(err),
        };
        let launch_after_install = match normalized_launch_after_install(args, compatibility_mode) {
            Ok(launch_after_install) => launch_after_install,
            Err(err) if compatibility_mode == "native-v1" => {
                return Ok(native_validation_failure_response(
                    None,
                    None,
                    false,
                    &err.to_string(),
                ));
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
        validate_download_request(
            config,
            &repository,
            args.workflow_run_id,
            &args.artifact_name,
        )?;
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
                requested_build
                    .as_ref()
                    .map(|build| build.commit_sha.as_str()),
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
            if active_build_matches_downloaded(current.as_ref(), &downloaded, &resolved_target) {
                self.installed_apk_matches(&resolved_target.device_serial, &downloaded.manifest)
                    .await?
            } else {
                false
            };
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
        let mut result = finish_install_build_from_run_response(
            raw_result,
            compatibility_mode,
            &resolved_target,
            &requested_build,
            &downloaded,
            launch_after_install,
        );
        result["workflow_head_sha"] = Value::String(downloaded.workflow_head_sha.clone());
        Ok(result)
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
        let content = read_bounded_file(&path, MAX_GITHUB_METADATA_BYTES)?;
        let state = serde_json::from_slice(&content)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        Ok(Some(state))
    }

    async fn download_build_bundle(
        &self,
        config: &InteractiveSessionConfig,
        repository: &str,
        workflow_run_id: u64,
        artifact_name: &str,
        selected_commit_sha: Option<&str>,
    ) -> Result<DownloadedBuildBundle, McpError> {
        let token = config.github_token.as_deref().ok_or_else(|| {
            McpError::invalid_params(
                "interactive session download requires ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_TOKEN",
                None,
            )
        })?;
        validate_download_request(config, repository, workflow_run_id, artifact_name)?;
        let client = github_client(token)?;
        let run: GitHubRun = github_json(
            &client,
            &format!("https://api.github.com/repos/{repository}/actions/runs/{workflow_run_id}"),
        )
        .await?;
        if run.id != workflow_run_id || run.repository.full_name != repository {
            return Err(McpError::invalid_params(
                "GitHub run identity does not match the requested repository and run",
                None,
            ));
        }
        let artifacts: GitHubArtifactList = github_json(
            &client,
            &format!(
                "https://api.github.com/repos/{repository}/actions/runs/{workflow_run_id}/artifacts?per_page=100"
            ),
        )
        .await?;
        let mut matches = artifacts
            .artifacts
            .into_iter()
            .filter(|candidate| candidate.name == artifact_name && !candidate.expired);
        let artifact = matches.next().ok_or_else(|| {
            McpError::invalid_params(
                format!("artifact {artifact_name} was not found on workflow run {workflow_run_id}"),
                None,
            )
        })?;
        if matches.next().is_some() {
            return Err(McpError::invalid_params(
                "multiple matching artifacts require an unambiguous artifact name",
                None,
            ));
        }
        // Construct the authenticated API endpoint from the observed numeric ID;
        // artifact metadata never supplies an arbitrary destination for the token.
        let response = client
            .get(format!(
                "https://api.github.com/repos/{repository}/actions/artifacts/{}/zip",
                artifact.id
            ))
            .send()
            .await
            .map_err(github_request_error)?
            .error_for_status()
            .map_err(github_request_error)?;
        let bytes = bounded_response_bytes(response, MAX_ARCHIVE_BYTES).await?;
        let artifact_sha256 = sha256_prefixed(&bytes);
        let build_dir = bundle_cache_dir(config, workflow_run_id, &artifact_sha256)?;
        let extraction_dir = build_dir.clone();
        tokio::task::spawn_blocking(move || extract_artifact_archive(&extraction_dir, &bytes))
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))??;
        fs::write(
            build_dir.join(CACHED_ARTIFACT_SHA256_FILE),
            format!("{artifact_sha256}\n"),
        )
        .await
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let manifest_path = find_single_file(&build_dir, "interactive-build-manifest.json")?;
        let manifest_bytes = read_bounded_file(&manifest_path, MAX_MANIFEST_BYTES)?;
        let manifest: BuildManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        validate_downloaded_manifest(
            config,
            repository,
            workflow_run_id,
            artifact_name,
            &run,
            &manifest,
            selected_commit_sha,
        )?;
        let apk_path = manifest_apk_path(&build_dir, &manifest)?;
        verify_sha256(&apk_path, &manifest.apk_sha256).await?;
        Ok(DownloadedBuildBundle {
            manifest,
            artifact_sha256,
            manifest_sha256: sha256_prefixed(&manifest_bytes),
            workflow_head_sha: run.head_sha,
        })
    }

    #[allow(clippy::too_many_arguments)] // Keep the already-validated identity tuple explicit at this boundary.
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
        if let Some(target) = resolved_target {
            self.resolve_android_execution_target(Some(&target_request(target)), Some(&serial))
                .await?;
        }
        if manifest.repository != config.github_repository
            || manifest.package_name != config.app_package
            || manifest.activity_name != config.app_activity
        {
            return Err(McpError::invalid_params(
                "cached build does not match the configured repository and app",
                None,
            ));
        }
        let build_dir =
            resolve_bundle_dir(config, manifest, artifact_sha256, manifest_sha256).await?;
        let apk_path = manifest_apk_path(&build_dir, manifest)?;
        if !mode.installs_apk() && !self.installed_apk_matches(&serial, manifest).await? {
            return Err(McpError::invalid_params(
                "the active build is no longer installed on the resolved device; install the exact build before relaunch",
                None,
            ));
        }
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
        if crate::execution::request_cancelled() {
            return Ok(json!({
                "ok": false, "outcome": "cancelled", "cancelled": true,
                "phase": "before_dispatch", "serial": serial,
                "installed": false, "install_performed": false,
                "reused_existing_build": matches!(mode, InstallLaunchMode::LaunchExisting),
                "launched": false, "launch_attempted": false,
                "manifest": manifest, "resolved_target": resolved_target,
                "retry_safe": true, "do_not_replay": false,
                "mutation_dispatched": false,
            }));
        }
        if mode.installs_apk() {
            let output =
                match install_apk_without_destructive_recovery(self, &serial, &apk_path).await {
                    Ok(output) => output,
                    Err(error) => {
                        return Ok(failed_install_result(&serial, manifest, error, false));
                    }
                };
            install_stdout = output.install_stdout;
            install_stderr = output.install_stderr;
            uninstalled_existing_package = output.uninstalled_existing_package;
            match self.installed_apk_matches(&serial, manifest).await {
                Ok(true) => {}
                observed => {
                    let message = match observed {
                        Ok(false) => {
                            "installed package bytes do not match the requested APK".to_string()
                        }
                        Err(error) => error.to_string(),
                        Ok(true) => unreachable!(),
                    };
                    return Ok(failed_install_result(
                        &serial,
                        manifest,
                        InstallDispatchFailure {
                            kind: "installed_verification_failed",
                            message,
                            outcome_unknown: true,
                        },
                        true,
                    ));
                }
            }
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
        let mut launch_cancelled = mode.launches_app() && crate::execution::request_cancelled();
        let mut launch_attempted = false;
        let (launch_stdout, launch_stderr, postcondition, launch_error) =
            if mode.launches_app() && !launch_cancelled {
                launch_attempted = true;
                let force_stop_result = self
                    .run_adb_shell(
                        &serial,
                        ["am", "force-stop", manifest.package_name.as_str()],
                    )
                    .await;
                match force_stop_result {
                    Err(err) => (String::new(), String::new(), None, Some(err.to_string())),
                    Ok(_) if crate::execution::request_cancelled() => {
                        previous_app_state = Some(INSTALL_LAUNCH_PREVIOUS_APP_STATE);
                        launch_cancelled = true;
                        (String::new(), String::new(), None, None)
                    }
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
            if launch_error.is_none()
                && let Err(err) = self
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
            "ok": !launch_cancelled && launch_error.is_none() && post_install_persistence_errors.is_empty(),
            "serial": serial,
            "installed": mode.installs_apk(),
            "install_performed": mode.installs_apk(),
            "reused_existing_build": matches!(mode, InstallLaunchMode::LaunchExisting),
            "launched": mode.launches_app() && !launch_cancelled && launch_error.is_none(),
            "launch_attempted": launch_attempted,
            "cancelled": launch_cancelled,
            "previous_app_state": previous_app_state,
            "install_stdout": install_stdout,
            "install_stderr": install_stderr,
            "uninstalled_existing_package": uninstalled_existing_package,
            "apk_path": apk_path.display().to_string(),
            "manifest": manifest,
            "stdout": launch_stdout,
            "stderr": launch_stderr,
            "postcondition": postcondition.as_ref().map(tool_postcondition_json),
            "resolved_target": resolved_target,
            "outcome": if launch_cancelled { "cancelled" } else if launch_error.is_some() { "postcondition_failed" } else if !post_install_persistence_errors.is_empty() { "partially_applied" } else { "verified" },
            "retry_safe": !mode.installs_apk() && !launch_attempted,
            "do_not_replay": mode.installs_apk() || launch_attempted,
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

    async fn installed_apk_matches(
        &self,
        serial: &str,
        manifest: &BuildManifest,
    ) -> Result<bool, McpError> {
        let paths = self
            .run_adb_shell(serial, ["pm", "path", manifest.package_name.as_str()])
            .await?;
        if paths.stdout.trim().is_empty() && paths.stderr.trim().is_empty() {
            return Ok(false);
        }
        let paths = paths
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        if paths.len() != 1 {
            return Err(McpError::internal_error(
                "cannot verify a single-APK build against an ambiguous package path",
                None,
            ));
        }
        let path = paths[0]
            .strip_prefix("package:")
            .filter(|path| path.starts_with('/'))
            .ok_or_else(|| {
                McpError::internal_error(
                    "package manager did not return an absolute APK path",
                    None,
                )
            })?;
        let digest = self.run_adb_shell(serial, ["sha256sum", path]).await?;
        let observed = digest
            .stdout
            .split_whitespace()
            .next()
            .filter(|value| is_hex_digest(value, 64))
            .ok_or_else(|| {
                McpError::internal_error("could not verify the installed APK digest", None)
            })?;
        Ok(observed == manifest.apk_sha256)
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

fn target_request(target: &ResolvedAndroidExecutionTarget) -> AndroidExecutionTarget {
    AndroidExecutionTarget {
        environment_id: Some(target.environment_id.clone()),
        provider_instance_id: Some(target.provider_instance_id.clone()),
        provider_epoch: Some(target.provider_epoch.clone()),
        session_id: Some(target.session_id.clone()),
        device_serial: Some(target.device_serial.clone()),
        app: target.app.clone(),
        expected_build: None,
    }
}

fn validate_component(value: &str, field: &str) -> Result<(), McpError> {
    if value.is_empty()
        || value.len() > 200
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(McpError::invalid_params(
            format!(
                "{field} must be a bounded identifier containing only letters, numbers, dots, underscores and hyphens"
            ),
            None,
        ));
    }
    Ok(())
}

fn validate_download_request(
    config: &InteractiveSessionConfig,
    repository: &str,
    run_id: u64,
    artifact: &str,
) -> Result<(), McpError> {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || repository != config.github_repository || run_id == 0 {
        return Err(McpError::invalid_params(
            "install repository must exactly match the configured repository and the run ID must be positive",
            None,
        ));
    }
    for part in parts {
        validate_component(part, "repository component")?;
    }
    validate_component(artifact, "artifact_name")
}

fn is_hex_digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_downloaded_manifest(
    config: &InteractiveSessionConfig,
    repository: &str,
    run_id: u64,
    artifact: &str,
    run: &GitHubRun,
    manifest: &BuildManifest,
    selected_commit_sha: Option<&str>,
) -> Result<(), McpError> {
    if manifest.schema_version != 1
        || manifest.repository != repository
        || manifest.run_id != run_id.to_string()
        || manifest.run_attempt != run.run_attempt.to_string()
        || manifest.artifact_name != artifact
        || manifest.commit_sha != selected_commit_sha.unwrap_or(&run.head_sha)
        || !is_hex_digest(&manifest.commit_sha, 40)
        || !is_hex_digest(&manifest.apk_sha256, 64)
        || manifest.package_name != config.app_package
        || manifest.activity_name != config.app_activity
    {
        return Err(McpError::invalid_params(
            "build manifest does not match the requested repository, run, attempt, commit, artifact and configured app",
            None,
        ));
    }
    validate_component(&manifest.apk_filename, "apk_filename")?;
    if !manifest.apk_filename.ends_with(".apk") {
        return Err(McpError::invalid_params(
            "manifest APK filename must end with .apk",
            None,
        ));
    }
    Ok(())
}

fn bundle_cache_dir(
    config: &InteractiveSessionConfig,
    run_id: u64,
    artifact_digest: &str,
) -> Result<PathBuf, McpError> {
    let digest = artifact_digest
        .strip_prefix("sha256:")
        .filter(|value| is_hex_digest(value, 64))
        .ok_or_else(|| {
            McpError::invalid_params(
                "artifact digest must be sha256 followed by 64 lowercase hex characters",
                None,
            )
        })?;
    if run_id == 0 {
        return Err(McpError::invalid_params(
            "workflow run ID must be positive",
            None,
        ));
    }
    std::fs::create_dir_all(&config.session_root).map_err(io_error)?;
    let session_root = std::fs::canonicalize(&config.session_root).map_err(io_error)?;
    let cache_root = session_root.join("build-cache");
    let run_root = cache_root.join(format!("run-{run_id}"));
    for path in [&cache_root, &run_root] {
        if !path.try_exists().map_err(io_error)? {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(error)),
            }
        }
        require_plain_directory(path)?;
    }
    Ok(run_root.join(format!("artifact-{digest}")))
}

fn github_request_error(error: reqwest::Error) -> McpError {
    // Artifact redirects contain signed query strings. Keep them out of logs and receipts.
    McpError::internal_error(error.without_url().to_string(), None)
}

async fn bounded_response_bytes(
    mut response: reqwest::Response,
    limit: u64,
) -> Result<Vec<u8>, McpError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(McpError::invalid_params(
            "GitHub response exceeds its size limit",
            None,
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(github_request_error)? {
        if (bytes.len() as u64).saturating_add(chunk.len() as u64) > limit {
            return Err(McpError::invalid_params(
                "GitHub response exceeded its size limit",
                None,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn github_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, McpError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(github_request_error)?
        .error_for_status()
        .map_err(github_request_error)?;
    let bytes = bounded_response_bytes(response, MAX_GITHUB_METADATA_BYTES).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| McpError::internal_error(error.to_string(), None))
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
    if resolved_target.provider_epoch.is_empty()
        || !matches!(active_build.status.as_str(), "installed" | "ready")
    {
        return Err(McpError::invalid_params(
            "active build state has no current provider epoch or verified installation",
            None,
        ));
    }
    let artifact_sha256 = active_build.artifact_sha256.as_deref().ok_or_else(|| {
        McpError::invalid_params("active build state is missing its artifact digest", None)
    })?;
    let manifest_sha256 = active_build.manifest_sha256.as_deref().ok_or_else(|| {
        McpError::invalid_params("active build state is missing its manifest digest", None)
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
        || !is_hex_digest(&expected_build.commit_sha, 40)
        || !expected_build
            .artifact_sha256
            .strip_prefix("sha256:")
            .is_some_and(|value| is_hex_digest(value, 64))
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
        && matches!(current.status.as_str(), "installed" | "ready")
        && !resolved_target.provider_epoch.is_empty()
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
    if raw_result["cancelled"].as_bool() == Some(true) {
        let install_performed = raw_result["install_performed"].as_bool() == Some(true);
        let reused = raw_result["reused_existing_build"].as_bool() == Some(true);
        let mut receipt = json!({
            "status": if install_performed { "installed_cancelled" } else { "cancelled" },
            "requested_build": requested_build_json(requested_build),
            "observed_artifact": observed_build_json(downloaded),
            "install_performed": install_performed,
            "reused_existing_build": reused,
            "launch": {
                "requested": launch_requested,
                "attempted": raw_result["launch_attempted"].as_bool().unwrap_or(false),
                "performed": false,
                "cancelled": true,
            },
        });
        if install_performed || reused {
            receipt["installed_build"] = observed_build_json(downloaded);
        }
        let error = json!({
            "kind": "cancelled",
            "message": "Request cancelled; no remaining install or launch phase was dispatched",
            "do_not_replay": raw_result["do_not_replay"],
            "retry_safe": raw_result["retry_safe"],
        });
        return install_receipt_response(
            raw_result,
            compatibility_mode,
            resolved_target,
            receipt,
            Some(error),
        );
    }
    if let Some(error) = raw_result.get("install_error").cloned() {
        let outcome_unknown = error["outcome_unknown"].as_bool().unwrap_or(true);
        let install_performed = raw_result["install_performed"].clone();
        let receipt = json!({
            "status": if outcome_unknown { "outcome_unknown" } else { "failed" },
            "requested_build": requested_build_json(requested_build),
            "observed_artifact": observed_build_json(downloaded),
            "install_attempted": true,
            "install_performed": install_performed,
            "launch": { "requested": launch_requested, "attempted": false, "performed": false },
        });
        return install_receipt_response(
            raw_result,
            compatibility_mode,
            resolved_target,
            receipt,
            Some(error),
        );
    }
    let launch_failed = raw_result.get("launch_error").is_some();
    let previous_app_state = raw_result.get("previous_app_state").and_then(Value::as_str);
    let persistence_errors = raw_result.get("post_install_persistence_errors").cloned();
    let install_performed = raw_result
        .get("install_performed")
        .and_then(Value::as_bool)
        .or_else(|| raw_result.get("installed").and_then(Value::as_bool))
        .unwrap_or(false);
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
        "observed_artifact": observed_build_json(downloaded),
        "launch": {
            "requested": launch_requested,
            "attempted": false,
            "performed": false,
        },
    })
}

#[cfg(test)]
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

struct InstallDispatchFailure {
    kind: &'static str,
    message: String,
    outcome_unknown: bool,
}

fn failed_install_result(
    serial: &str,
    manifest: &BuildManifest,
    error: InstallDispatchFailure,
    install_performed: bool,
) -> Value {
    json!({
        "ok": false,
        "serial": serial,
        "manifest": manifest,
        "outcome": if error.outcome_unknown { "outcome_unknown" } else { "rejected" },
        "retry_safe": false,
        "do_not_replay": true,
        "install_attempted": true,
        "install_performed": if install_performed { Some(true) } else if error.outcome_unknown { None } else { Some(false) },
        "installed": if install_performed { Some(true) } else if error.outcome_unknown { None } else { Some(false) },
        "launch_attempted": false,
        "launched": false,
        "uninstalled_existing_package": false,
        "install_error": {
            "kind": error.kind,
            "phase": if install_performed { "installed_build_verification" } else { "install_dispatch" },
            "message": error.message,
            "outcome_unknown": error.outcome_unknown,
            "retryability": "do_not_replay",
        },
    })
}

async fn install_apk_without_destructive_recovery(
    mcp: &AndroidEmulatorMcp,
    serial: &str,
    apk_path: &Path,
) -> Result<InstallOutcome, InstallDispatchFailure> {
    let initial = mcp
        .run_adb_allow_failure(
            serial,
            [
                "install".to_string(),
                "-r".to_string(),
                apk_path.display().to_string(),
            ],
        )
        .await
        .map_err(|error| InstallDispatchFailure {
            kind: "install_dispatch_unknown",
            message: error.to_string(),
            outcome_unknown: true,
        })?;
    if adb_install_succeeded(&initial.stdout, &initial.stderr) {
        return Ok(InstallOutcome {
            install_stdout: initial.stdout,
            install_stderr: initial.stderr,
            uninstalled_existing_package: false,
        });
    }
    if adb_install_failed_due_to_signature_mismatch(&initial.stdout, &initial.stderr) {
        return Err(InstallDispatchFailure {
            kind: "signature_mismatch",
            message: "The installed package has a different signing identity. No uninstall was performed. Uninstalling deletes app data and requires a separate explicit action.".to_string(),
            outcome_unknown: false,
        });
    }
    let rejected = initial.stdout.contains("Failure [") || initial.stderr.contains("Failure [");
    Err(InstallDispatchFailure {
        kind: if rejected { "install_rejected" } else { "install_dispatch_unknown" },
        message: if rejected { "Android rejected the APK installation" } else { "ADB did not provide a definitive installation result; inspect the device before another install" }.to_string(),
        outcome_unknown: !rejected,
    })
}

fn adb_install_succeeded(stdout: &str, stderr: &str) -> bool {
    let success = stdout
        .lines()
        .chain(stderr.lines())
        .any(|line| line.trim() == "Success");
    success && !stdout.contains("Failure [") && !stderr.contains("Failure [")
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
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("android-computer-use-mcp"),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static(GITHUB_API_VERSION),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|err| McpError::internal_error(err.to_string(), None))
}

fn extract_artifact_archive(build_dir: &Path, bytes: &[u8]) -> Result<(), McpError> {
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(McpError::invalid_params(
            "artifact archive exceeds the download limit",
            None,
        ));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    if archive.len() > MAX_ARCHIVE_MEMBERS {
        return Err(McpError::invalid_params(
            "artifact archive contains too many members",
            None,
        ));
    }
    let staging = temporary_sibling(build_dir, "extract")?;
    std::fs::create_dir(&staging).map_err(io_error)?;
    let result = extract_archive_members(&mut archive, &staging);
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    // Replace only this digest-addressed cache directory. A failed extraction
    // never deletes the previous usable bundle.
    let previous = if build_dir.try_exists().map_err(io_error)? {
        require_plain_directory(build_dir)?;
        let previous = temporary_sibling(build_dir, "previous")?;
        std::fs::rename(build_dir, &previous).map_err(io_error)?;
        Some(previous)
    } else {
        None
    };
    if let Err(error) = std::fs::rename(&staging, build_dir) {
        if let Some(previous) = &previous {
            let _ = std::fs::rename(previous, build_dir);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(io_error(error));
    }
    if let Some(previous) = previous {
        std::fs::remove_dir_all(previous).map_err(io_error)?;
    }
    Ok(())
}

fn extract_archive_members(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    build_dir: &Path,
) -> Result<(), McpError> {
    let mut total_bytes = 0_u64;
    let mut seen_paths = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        let safe_name = entry.enclosed_name().ok_or_else(|| {
            McpError::invalid_params("artifact member is outside its extraction directory", None)
        })?;
        validate_archive_member(&safe_name, entry.unix_mode(), entry.size(), entry.is_dir())?;
        if !seen_paths.insert(safe_name.clone()) {
            return Err(McpError::invalid_params(
                "artifact archive contains duplicate paths",
                None,
            ));
        }
        total_bytes = total_bytes.checked_add(entry.size()).ok_or_else(|| {
            McpError::invalid_params("artifact uncompressed size is invalid", None)
        })?;
        if total_bytes > MAX_EXTRACTED_BYTES {
            return Err(McpError::invalid_params(
                "artifact archive exceeds the extraction limit",
                None,
            ));
        }
        let output_path = build_dir.join(safe_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&output_path)
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
            continue;
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .map_err(io_error)?;
        let expected = entry.size();
        let copied = std::io::copy(
            &mut entry.by_ref().take(expected.saturating_add(1)),
            &mut output,
        )
        .map_err(io_error)?;
        if copied != expected {
            return Err(McpError::invalid_params(
                "artifact member did not match its declared size",
                None,
            ));
        }
        output.flush().map_err(io_error)?;
    }
    Ok(())
}

fn validate_archive_member(
    path: &Path,
    mode: Option<u32>,
    size: u64,
    is_dir: bool,
) -> Result<(), McpError> {
    if path.as_os_str().is_empty()
        || path.components().count() > MAX_ARCHIVE_DEPTH
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        || path.to_string_lossy().contains('\\')
        || size > MAX_ARCHIVE_BYTES
        || (path.file_name().and_then(|part| part.to_str())
            == Some("interactive-build-manifest.json")
            && size > MAX_MANIFEST_BYTES)
    {
        return Err(McpError::invalid_params(
            "artifact member exceeds path or size limits",
            None,
        ));
    }
    let kind = mode.unwrap_or_default() & 0o170000;
    if kind != 0 && kind != if is_dir { 0o040000 } else { 0o100000 } {
        return Err(McpError::invalid_params(
            "artifact archive may contain only regular files and directories",
            None,
        ));
    }
    Ok(())
}

fn temporary_sibling(path: &Path, purpose: &str) -> Result<PathBuf, McpError> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| McpError::internal_error("cache path has no filename", None))?;
    Ok(path.with_file_name(format!(
        ".{name}-{purpose}-{}-{}",
        crate::config::process_epoch(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )))
}

fn io_error(error: std::io::Error) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

fn require_plain_directory(path: &Path) -> Result<(), McpError> {
    if !std::fs::symlink_metadata(path)
        .map_err(io_error)?
        .file_type()
        .is_dir()
    {
        return Err(McpError::invalid_params(
            "build cache entries must be plain directories",
            None,
        ));
    }
    Ok(())
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, McpError> {
    let metadata = std::fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(McpError::invalid_params(
            "build cache file is not a bounded regular file",
            None,
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(io_error)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > maximum {
        return Err(McpError::invalid_params(
            "build cache file exceeded its size limit",
            None,
        ));
    }
    Ok(bytes)
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
    require_plain_directory(root)?;
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0_usize;
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path)
            .map_err(|err| McpError::internal_error(err.to_string(), None))?
        {
            let entry = entry.map_err(|err| McpError::internal_error(err.to_string(), None))?;
            let entry_path = entry.path();
            visited += 1;
            if visited > MAX_ARCHIVE_MEMBERS * (MAX_ARCHIVE_DEPTH + 1) {
                return Err(McpError::invalid_params(
                    "build cache contains too many entries",
                    None,
                ));
            }
            let file_type = entry.file_type().map_err(io_error)?;
            if file_type.is_dir() {
                stack.push(entry_path);
            } else if file_type.is_file() {
                entries.push(entry_path);
            } else {
                return Err(McpError::invalid_params(
                    "build cache contains a non-regular entry",
                    None,
                ));
            }
        }
    }
    Ok(entries)
}

fn manifest_apk_path(build_dir: &Path, manifest: &BuildManifest) -> Result<PathBuf, McpError> {
    validate_component(&manifest.apk_filename, "apk_filename")?;
    if !manifest.apk_filename.ends_with(".apk") {
        return Err(McpError::invalid_params(
            "manifest APK filename must end with .apk",
            None,
        ));
    }
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
    let run_id = manifest.run_id.parse::<u64>().map_err(|_| {
        McpError::invalid_params("cached manifest run ID must be a positive integer", None)
    })?;
    let build_dir = bundle_cache_dir(config, run_id, expected_artifact_sha256)?;
    let manifest_path = find_single_file(&build_dir, "interactive-build-manifest.json")?;
    let manifest_bytes = read_bounded_file(&manifest_path, MAX_MANIFEST_BYTES)?;
    let candidate: BuildManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    let cached_artifact_sha256 =
        read_bounded_file(&build_dir.join(CACHED_ARTIFACT_SHA256_FILE), 128)?;
    if candidate == *manifest
        && sha256_prefixed(&manifest_bytes) == expected_manifest_sha256
        && String::from_utf8_lossy(&cached_artifact_sha256).trim() == expected_artifact_sha256
    {
        let apk_path = manifest_apk_path(&build_dir, manifest)?;
        verify_sha256(&apk_path, &manifest.apk_sha256).await?;
        return Ok(build_dir);
    }
    Err(McpError::internal_error(
        "could not locate a cached bundle matching the active build's exact digests".to_string(),
        None,
    ))
}

async fn verify_sha256(path: &Path, expected: &str) -> Result<(), McpError> {
    use tokio::io::AsyncReadExt;
    if !is_hex_digest(expected, 64) {
        return Err(McpError::invalid_params(
            "APK SHA-256 must contain 64 lowercase hex characters",
            None,
        ));
    }
    let metadata = fs::symlink_metadata(path).await.map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(McpError::invalid_params(
            "APK must be a bounded regular file",
            None,
        ));
    }
    let mut file = fs::File::open(path).await.map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).await.map_err(io_error)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_ARCHIVE_BYTES {
            return Err(McpError::invalid_params(
                "APK exceeded its size limit",
                None,
            ));
        }
        digest.update(&buffer[..read]);
    }
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
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        require_plain_directory(parent)?;
    }
    let temporary = temporary_sibling(path, "write")?;
    let bytes = serde_json::to_vec_pretty(payload)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
    let result = async {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(io_error)?;
        file.write_all(&bytes).await.map_err(io_error)?;
        file.sync_all().await.map_err(io_error)?;
        drop(file);
        fs::rename(&temporary, path).await.map_err(io_error)
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

fn iso_now() -> String {
    chrono_like_iso(SystemTimeLike::now())
}

fn timestamp_slug() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}",
        crate::config::process_epoch(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
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

    use super::*;

    const FIXTURE_APK: &[u8] = b"bounded local installation fixture";

    #[cfg(unix)]
    struct InstallFixture {
        root: PathBuf,
        mcp: AndroidEmulatorMcp,
        bundle: DownloadedBuildBundle,
    }

    #[cfg(unix)]
    impl Drop for InstallFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    impl InstallFixture {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = temporary_sibling(
                &std::env::temp_dir().join("android-mcp-install-test"),
                "fixture",
            )
            .unwrap();
            std::fs::create_dir(&root).unwrap();
            let mut bundle = downloaded_build_bundle();
            bundle.manifest.commit_sha = "a".repeat(40);
            bundle.manifest.apk_sha256 = sha256_prefixed(FIXTURE_APK)
                .trim_start_matches("sha256:")
                .to_string();
            bundle.artifact_sha256 = sha256_prefixed(b"bounded artifact fixture");
            bundle.workflow_head_sha = bundle.manifest.commit_sha.clone();
            let manifest = serde_json::to_vec(&bundle.manifest).unwrap();
            bundle.manifest_sha256 = sha256_prefixed(&manifest);
            let interactive_session = InteractiveSessionConfig {
                session_root: root.join("session"),
                github_repository: bundle.manifest.repository.clone(),
                github_token: None,
                app_package: bundle.manifest.package_name.clone(),
                app_activity: bundle.manifest.activity_name.clone(),
            };
            let cache =
                bundle_cache_dir(&interactive_session, 42, &bundle.artifact_sha256).unwrap();
            std::fs::create_dir(&cache).unwrap();
            std::fs::write(cache.join("interactive-build-manifest.json"), manifest).unwrap();
            std::fs::write(cache.join(&bundle.manifest.apk_filename), FIXTURE_APK).unwrap();
            std::fs::write(
                cache.join(CACHED_ARTIFACT_SHA256_FILE),
                &bundle.artifact_sha256,
            )
            .unwrap();
            let adb = root.join("adb");
            let script = r#"#!/bin/sh
dir=${0%/*}
printf '%s\n' "$*" >> "$dir/calls.log"
if [ "$1" = devices ]; then
  printf 'List of devices attached\nemulator-5554 device transport_id:1\n'
  exit 0
fi
if [ "$1" = -s ]; then shift 2; fi
if [ "$1" = install ]; then
  if [ -f "$dir/signature-mismatch" ]; then
    printf 'Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE]\n' >&2
    exit 1
  fi
  if [ -f "$dir/unknown-install" ]; then exit 1; fi
  printf 'Success\n'
  if [ -f "$dir/hold-install-ack" ]; then
    : > "$dir/install-applied"
    while [ ! -f "$dir/release-install-ack" ]; do sleep 0.01; done
  fi
  exit 0
fi
case "$*" in
  *pm*path*) printf 'package:/data/app/com.example.app/base.apk\n' ;;
  *sha256sum*) printf '__APK_SHA__  /data/app/com.example.app/base.apk\n' ;;
  *pidof*) exit 1 ;;
  *am*start*) if [ -f "$dir/launch-failure" ]; then exit 1; fi ;;
esac
exit 0
"#
            .replace("__APK_SHA__", &bundle.manifest.apk_sha256);
            std::fs::write(&adb, script).unwrap();
            std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o700)).unwrap();
            let config = crate::config::Config {
                sdk_root: root.clone(),
                adb_path: adb,
                emulator_path: PathBuf::from("/bin/true"),
                avdmanager_path: PathBuf::from("/bin/true"),
                artifact_dir: root.join("artifacts"),
                emulator_grpc_port: None,
                use_sg_kvm: false,
                streamable_http: crate::config::StreamableHttpConfig {
                    bind_addr: "127.0.0.1:9526".parse().unwrap(),
                    allowed_hosts: vec!["localhost".to_string()],
                    max_sessions: 4,
                    channel_capacity: 16,
                    allow_resume: true,
                },
                interactive_session: Some(interactive_session),
                execution_identity: crate::config::ProviderExecutionIdentity {
                    environment_id: "environment-1".to_string(),
                    provider_instance_id: "provider-1".to_string(),
                    provider_epoch: "epoch-1".to_string(),
                    session_id: "session-1".to_string(),
                },
            };
            Self {
                root,
                mcp: AndroidEmulatorMcp::new(config),
                bundle,
            }
        }

        async fn install(&self, mode: InstallLaunchMode) -> Value {
            self.mcp
                .install_or_launch_manifest(
                    &self.bundle.manifest,
                    Some(&self.bundle.artifact_sha256),
                    Some(&self.bundle.manifest_sha256),
                    Some(&resolved_target()),
                    Some("emulator-5554"),
                    mode,
                    Some(1),
                )
                .await
                .expect("phase-aware install result")
        }

        fn calls(&self) -> String {
            std::fs::read_to_string(self.root.join("calls.log")).unwrap()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn install_only_never_launches_or_stops_the_app() {
        let fixture = InstallFixture::new();
        let result = fixture.install(InstallLaunchMode::InstallOnly).await;
        assert_eq!(result["ok"], true);
        assert_eq!(result["installed"], true);
        assert_eq!(result["launch_attempted"], false);
        assert_eq!(result["do_not_replay"], true);
        let calls = fixture.calls();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains(" install "))
                .count(),
            1
        );
        assert!(!calls.contains("force-stop"));
        assert!(!calls.contains("start"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signature_mismatch_preserves_app_data_and_does_not_retry() {
        let fixture = InstallFixture::new();
        std::fs::write(fixture.root.join("signature-mismatch"), "").unwrap();
        let result = fixture.install(InstallLaunchMode::InstallOnly).await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["install_error"]["kind"], "signature_mismatch");
        assert_eq!(result["installed"], false);
        assert!(!fixture.calls().contains("uninstall"));
        assert_eq!(
            fixture
                .calls()
                .lines()
                .filter(|line| line.contains(" install "))
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_install_acknowledgement_reports_unknown_without_replay() {
        let fixture = InstallFixture::new();
        std::fs::write(fixture.root.join("unknown-install"), "").unwrap();
        let result = fixture.install(InstallLaunchMode::InstallOnly).await;
        assert_eq!(result["outcome"], "outcome_unknown");
        assert!(result["installed"].is_null());
        assert_eq!(result["do_not_replay"], true);
        assert_eq!(result["retry_safe"], false);
        assert_eq!(
            fixture
                .calls()
                .lines()
                .filter(|line| line.contains(" install "))
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launch_failure_keeps_the_known_installed_build() {
        let fixture = InstallFixture::new();
        std::fs::write(fixture.root.join("launch-failure"), "").unwrap();
        let result = fixture.install(InstallLaunchMode::InstallAndLaunch).await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["installed"], true);
        assert_eq!(result["launched"], false);
        assert_eq!(result["do_not_replay"], true);
        assert_eq!(
            fixture
                .calls()
                .lines()
                .filter(|line| line.contains(" install "))
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistence_failure_keeps_the_known_install_effect() {
        let fixture = InstallFixture::new();
        std::fs::create_dir(fixture.root.join("session/active-build.json")).unwrap();
        let result = fixture.install(InstallLaunchMode::InstallOnly).await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["installed"], true);
        assert_eq!(result["outcome"], "partially_applied");
        assert_eq!(result["do_not_replay"], true);
        assert!(
            !result["post_install_persistence_errors"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_install_before_dispatch_has_no_device_effect() {
        let fixture = InstallFixture::new();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let result = crate::execution::with_cancellation(
            token,
            fixture.install(InstallLaunchMode::InstallAndLaunch),
        )
        .await;
        assert_eq!(result["outcome"], "cancelled");
        assert_eq!(result["mutation_dispatched"], false);
        assert_eq!(result["install_performed"], false);
        assert_eq!(result["retry_safe"], true);
        assert!(!fixture.calls().contains(" install "));
        assert!(!fixture.calls().contains("force-stop"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_after_install_preserves_the_build_and_skips_launch() {
        let fixture = InstallFixture::new();
        std::fs::write(fixture.root.join("hold-install-ack"), "").unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        let control = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !fixture.root.join("install-applied").is_file() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("installation should reach its acknowledgement boundary");
            token.cancel();
            std::fs::write(fixture.root.join("release-install-ack"), "").unwrap();
        };
        let (result, ()) = tokio::join!(
            crate::execution::with_cancellation(
                token.clone(),
                fixture.install(InstallLaunchMode::InstallAndLaunch)
            ),
            control,
        );
        assert_eq!(result["outcome"], "cancelled");
        assert_eq!(result["installed"], true);
        assert_eq!(result["launch_attempted"], false);
        assert_eq!(result["do_not_replay"], true);
        assert!(!fixture.calls().contains("force-stop"));
        let receipt = finish_install_build_from_run_response(
            result,
            "native-v1",
            &resolved_target(),
            &expected_build(),
            &fixture.bundle,
            true,
        );
        assert_eq!(receipt["install_receipt"]["status"], "installed_cancelled");
        assert_eq!(
            receipt["install_receipt"]["installed_build"]["commit_sha"],
            fixture.bundle.manifest.commit_sha
        );
        assert_eq!(receipt["install_receipt"]["launch"]["performed"], false);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_provider_epoch_is_rejected_before_install_dispatch() {
        let fixture = InstallFixture::new();
        let mut stale = resolved_target();
        stale.provider_epoch = "previous-process".to_string();
        let result = fixture
            .mcp
            .install_or_launch_manifest(
                &fixture.bundle.manifest,
                Some(&fixture.bundle.artifact_sha256),
                Some(&fixture.bundle.manifest_sha256),
                Some(&stale),
                Some("emulator-5554"),
                InstallLaunchMode::InstallOnly,
                Some(1),
            )
            .await;
        assert!(result.is_err());
        assert!(!fixture.calls().contains(" install "));
    }

    #[cfg(unix)]
    #[test]
    fn failed_archive_download_does_not_remove_a_previous_cache() {
        let fixture = InstallFixture::new();
        let archive_dir = fixture.root.join("archive-cache");
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("bundle/app.apk", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(FIXTURE_APK).unwrap();
        let archive = writer.finish().unwrap().into_inner();
        extract_artifact_archive(&archive_dir, &archive).unwrap();
        assert!(extract_artifact_archive(&archive_dir, b"incomplete archive download").is_err());
        assert_eq!(
            std::fs::read(archive_dir.join("bundle/app.apk")).unwrap(),
            FIXTURE_APK
        );
    }

    #[cfg(unix)]
    #[test]
    fn manifest_is_bound_to_the_downloaded_run_and_selected_checkout() {
        let fixture = InstallFixture::new();
        let config = fixture.mcp.config.interactive_session.as_ref().unwrap();
        let run = GitHubRun {
            id: 42,
            head_sha: "b".repeat(40),
            run_attempt: 1,
            repository: GitHubRepository {
                full_name: config.github_repository.clone(),
            },
        };
        let manifest = &fixture.bundle.manifest;
        assert!(
            validate_downloaded_manifest(
                config,
                &config.github_repository,
                42,
                "android-build",
                &run,
                manifest,
                None
            )
            .is_err()
        );
        assert!(
            validate_downloaded_manifest(
                config,
                &config.github_repository,
                42,
                "android-build",
                &run,
                manifest,
                Some(&manifest.commit_sha)
            )
            .is_ok()
        );
        assert!(
            validate_downloaded_manifest(
                config,
                &config.github_repository,
                43,
                "android-build",
                &run,
                manifest,
                Some(&manifest.commit_sha)
            )
            .is_err()
        );
        assert!(
            validate_downloaded_manifest(
                config,
                &config.github_repository,
                42,
                "different-artifact",
                &run,
                manifest,
                Some(&manifest.commit_sha)
            )
            .is_err()
        );
        assert!(
            validate_download_request(config, "different/repository", 42, "android-build").is_err()
        );
    }

    #[test]
    fn archive_metadata_is_bounded_and_regular() {
        assert!(
            validate_archive_member(Path::new("bundle/app.apk"), Some(0o100600), 1024, false)
                .is_ok()
        );
        assert!(
            validate_archive_member(Path::new("bundle/app.apk"), Some(0o120777), 1024, false)
                .is_err()
        );
        assert!(
            validate_archive_member(
                Path::new("app.apk"),
                Some(0o100600),
                MAX_ARCHIVE_BYTES + 1,
                false
            )
            .is_err()
        );
        assert!(
            validate_archive_member(
                Path::new("interactive-build-manifest.json"),
                Some(0o100600),
                MAX_MANIFEST_BYTES + 1,
                false
            )
            .is_err()
        );
    }

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
            workflow_head_sha: "abcdef0123456789".to_string(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cached_bundle_resolution_rechecks_the_exact_manifest_and_apk() {
        let fixture = InstallFixture::new();
        let config = fixture.mcp.config.interactive_session.as_ref().unwrap();
        let path = resolve_bundle_dir(
            config,
            &fixture.bundle.manifest,
            Some(&fixture.bundle.artifact_sha256),
            Some(&fixture.bundle.manifest_sha256),
        )
        .await
        .expect("exact cache resolves");
        std::fs::write(
            path.join(&fixture.bundle.manifest.apk_filename),
            b"different fixture bytes",
        )
        .unwrap();
        assert!(
            resolve_bundle_dir(
                config,
                &fixture.bundle.manifest,
                Some(&fixture.bundle.artifact_sha256),
                Some(&fixture.bundle.manifest_sha256)
            )
            .await
            .is_err(),
            "changed APK bytes must be rejected before installation"
        );
        assert!(
            !fixture.root.join("calls.log").exists(),
            "cache validation must not call ADB"
        );
    }

    fn resolved_target() -> ResolvedAndroidExecutionTarget {
        ResolvedAndroidExecutionTarget {
            environment_id: "environment-1".to_string(),
            provider_instance_id: "provider-1".to_string(),
            provider_epoch: "epoch-1".to_string(),
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

        assert!(
            err.to_string()
                .contains("must not use launch_after_install")
        );
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

        assert_eq!(
            response["install_receipt"]["status"],
            json!("reused_existing_build")
        );
        assert_eq!(
            response["install_receipt"]["install_performed"],
            json!(false)
        );
        assert_eq!(
            response["install_receipt"]["reused_existing_build"],
            json!(true)
        );
        assert_eq!(
            response["install_receipt"]["launch"]["performed"],
            json!(true)
        );
    }

    #[test]
    fn native_validation_failures_keep_the_versioned_install_envelope() {
        let response = native_validation_failure_response(
            None,
            Some(&expected_build()),
            false,
            "a native v1 install requires install.launch_after_install",
        );

        assert_eq!(
            response["contract_version"],
            json!(ANDROID_PROVIDER_EXECUTION_CONTRACT_VERSION)
        );
        assert_eq!(response["compatibility_mode"], json!("native-v1"));
        assert_eq!(response["operation_kind"], json!("install_build_from_run"));
        assert_eq!(
            response["install_receipt"]["status"],
            json!("invalid_request")
        );
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
                    "provider_epoch": "epoch-1",
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
                    "provider_epoch": "epoch-1",
                    "session_id": "session-1",
                    "device_serial": "emulator-5554",
                },
                "install_receipt": {
                    "status": "installed",
                    "install_performed": true,
                    "reused_existing_build": false,
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
        assert_eq!(
            response["error"]["persistence_errors"],
            json!([{
                "phase": "install_history",
                "message": "history volume is read-only",
            }])
        );
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
                    "provider_epoch": "epoch-1",
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
                    "observed_artifact": {
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
                    "provider_epoch": "epoch-1",
                    "session_id": "session-1",
                    "device_serial": "emulator-5554",
                },
                "install_receipt": {
                    "status": "installed_launch_failed",
                    "install_performed": true,
                    "reused_existing_build": false,
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
                "install_performed": true,
                "reused_existing_build": false,
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
                "install_performed": true,
                "reused_existing_build": false,
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
                    "provider_epoch": "epoch-1",
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
