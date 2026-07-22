//! Hosted interactive-session helpers for runner-backed Android reuse.
//!
//! ## Rationale
//! Keep GitHub artifact download, active-build state, and live-session relaunch
//! logic out of the core Android tool implementations while still exposing the
//! resulting controls through the same MCP endpoint.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::McpError;
use crate::config::InteractiveSessionConfig;
use crate::server::AndroidEmulatorMcp;
use crate::tools::{DEFAULT_ACTION_TIMEOUT_SECS, tool_deadline};
use crate::verification::{
    ToolPostconditionRequest, ensure_tool_postcondition_satisfied, tool_postcondition_json,
};

const GITHUB_API_VERSION: &str = "2022-11-28";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InteractiveSessionInstallBuildArgs {
    pub workflow_run_id: u64,
    pub artifact_name: String,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default = "default_launch_after_install")]
    pub launch_after_install: bool,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct InteractiveSessionRelaunchArgs {
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn default_launch_after_install() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActiveBuildState {
    schema_version: u64,
    status: String,
    activated_at_iso: String,
    manifest: BuildManifest,
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
        self.install_or_launch_manifest(
            &active_build.manifest,
            args.serial.as_deref(),
            false,
            args.timeout_secs,
        )
        .await
    }

    pub(crate) async fn interactive_session_install_build_from_run_json(
        &self,
        args: &InteractiveSessionInstallBuildArgs,
    ) -> Result<serde_json::Value, McpError> {
        let config = self.interactive_session_config()?;
        if args.artifact_name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "artifact_name must not be empty",
                None,
            ));
        }
        let repository = args
            .repository
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(config.github_repository.as_str())
            .to_string();
        let manifest = self
            .download_build_bundle(
                config,
                &repository,
                args.workflow_run_id,
                args.artifact_name.trim(),
            )
            .await?;
        let current = self.read_active_build_state(config).await?;
        let already_active = current
            .as_ref()
            .map(|current| current.manifest.apk_sha256 == manifest.apk_sha256)
            .unwrap_or(false);
        if already_active && !args.launch_after_install {
            return Ok(json!({
                "ok": true,
                "reused_existing_build": true,
                "manifest": manifest,
            }));
        }
        self.install_or_launch_manifest(
            &manifest,
            args.serial.as_deref(),
            !already_active,
            args.timeout_secs,
        )
        .await
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
    ) -> Result<BuildManifest, McpError> {
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
        extract_artifact_archive(&build_dir, &bytes)?;
        let manifest_path = find_single_file(&build_dir, "interactive-build-manifest.json")?;
        let manifest: BuildManifest = serde_json::from_slice(
            &fs::read(&manifest_path)
                .await
                .map_err(|err| McpError::internal_error(err.to_string(), None))?,
        )
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
        Ok(manifest)
    }

    async fn install_or_launch_manifest(
        &self,
        manifest: &BuildManifest,
        serial_hint: Option<&str>,
        install_apk: bool,
        timeout_secs: Option<u64>,
    ) -> Result<serde_json::Value, McpError> {
        let config = self.interactive_session_config()?;
        let serial = self.resolve_serial_for_tools(serial_hint).await?;
        let build_dir = resolve_bundle_dir(config, manifest).await?;
        let apk_path = manifest_apk_path(&build_dir, manifest)?;
        let mut install_stdout = String::new();
        let mut install_stderr = String::new();
        let mut uninstalled_existing_package = false;
        if install_apk {
            let output =
                install_apk_with_signature_mismatch_fallback(self, &serial, &apk_path, manifest)
                    .await?;
            install_stdout = output.install_stdout;
            install_stderr = output.install_stderr;
            uninstalled_existing_package = output.uninstalled_existing_package;
        }
        let output = self
            .run_adb_shell(
                &serial,
                [
                    "am",
                    "start",
                    "-n",
                    &format!("{}/{}", manifest.package_name, manifest.activity_name),
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
        self.write_active_build_state(config, manifest, &postcondition)
            .await?;
        self.append_install_history(config, manifest, &postcondition)
            .await?;
        Ok(json!({
            "ok": true,
            "serial": serial,
            "installed": install_apk,
            "install_stdout": install_stdout,
            "install_stderr": install_stderr,
            "uninstalled_existing_package": uninstalled_existing_package,
            "apk_path": apk_path.display().to_string(),
            "manifest": manifest,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "postcondition": tool_postcondition_json(&postcondition),
        }))
    }

    async fn write_active_build_state(
        &self,
        config: &InteractiveSessionConfig,
        manifest: &BuildManifest,
        postcondition: &crate::verification::ToolPostconditionResult,
    ) -> Result<(), McpError> {
        let path = config.session_root.join("active-build.json");
        let payload = json!({
            "schema_version": 1,
            "status": "ready",
            "activated_at_iso": iso_now(),
            "manifest": manifest,
            "proof": {
                "postcondition": tool_postcondition_json(postcondition),
            },
            "preflight": serde_json::Value::Null,
        });
        write_json_file(&path, &payload).await
    }

    async fn append_install_history(
        &self,
        config: &InteractiveSessionConfig,
        manifest: &BuildManifest,
        postcondition: &crate::verification::ToolPostconditionResult,
    ) -> Result<(), McpError> {
        let path = config
            .session_root
            .join("install-history")
            .join(format!("{}-tool-install.json", timestamp_slug()));
        let payload = json!({
            "schema_version": 1,
            "recorded_at_iso": iso_now(),
            "status": "ready",
            "manifest": manifest,
            "postcondition": tool_postcondition_json(postcondition),
        });
        write_json_file(&path, &payload).await
    }
}

struct InstallOutcome {
    install_stdout: String,
    install_stderr: String,
    uninstalled_existing_package: bool,
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
) -> Result<PathBuf, McpError> {
    let build_cache_root = config.session_root.join("build-cache");
    let manifest_files = walkdir(&build_cache_root)?
        .into_iter()
        .filter(|path| {
            path.file_name().and_then(|value| value.to_str())
                == Some("interactive-build-manifest.json")
        })
        .collect::<Vec<_>>();
    for manifest_path in manifest_files {
        let candidate: BuildManifest = serde_json::from_slice(
            &fs::read(&manifest_path)
                .await
                .map_err(|err| McpError::internal_error(err.to_string(), None))?,
        )
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
        if candidate.run_id == manifest.run_id && candidate.artifact_name == manifest.artifact_name
        {
            return manifest_path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| {
                    McpError::internal_error(
                        "build manifest had no parent directory".to_string(),
                        None,
                    )
                });
        }
    }
    Err(McpError::internal_error(
        format!(
            "could not locate cached bundle for run {} and artifact {}",
            manifest.run_id, manifest.artifact_name
        ),
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
    use super::{adb_install_failed_due_to_signature_mismatch, adb_install_succeeded};

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
}
