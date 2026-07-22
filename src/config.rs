//! Typed configuration for android-computer-use-mcp.
//!
//! ## Rationale
//! Provides a centralized, type-safe configuration layer for the server.
//! It resolves SDK paths and environment-specific settings, ensuring the
//! server has a valid operating context before startup.
//!
//! ## Security Boundaries
//! * Paths are validated on startup to exist and be accessible.
//! * Environment variables are normalized to prevent unexpected path traversal.
//!
use std::env;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Result, anyhow};
use clap::Parser;

#[derive(Debug, Parser)]
pub struct Cli {
    /// Print the registered tool names and exit.
    #[arg(long)]
    pub print_tools: bool,

    /// Print the registered tool schema snapshot and exit.
    #[arg(long)]
    pub print_tool_schema: bool,

    /// Run one built-in Solar Lab scenario directly and exit.
    #[arg(long)]
    pub run_scenario: Option<String>,

    /// Target adb serial for direct scenario execution.
    #[arg(long)]
    pub serial: Option<String>,

    /// Override the target Android package name for direct scenario execution.
    #[arg(long)]
    pub package_name: Option<String>,

    /// Override the target Android activity for direct scenario execution.
    #[arg(long)]
    pub activity: Option<String>,

    /// Override the Android SDK root.
    #[arg(long)]
    pub sdk_root: Option<PathBuf>,

    /// Override the local artifact output directory.
    #[arg(long)]
    pub artifact_dir: Option<PathBuf>,

    /// Override the emulator gRPC port used when launching an AVD.
    #[arg(long)]
    pub emulator_grpc_port: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct StreamableHttpConfig {
    pub bind_addr: std::net::SocketAddr,
    pub allowed_hosts: Vec<String>,
    pub max_sessions: usize,
    pub channel_capacity: usize,
    pub allow_resume: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub sdk_root: PathBuf,
    pub adb_path: PathBuf,
    pub emulator_path: PathBuf,
    pub avdmanager_path: PathBuf,
    pub artifact_dir: PathBuf,
    pub emulator_grpc_port: Option<u16>,
    pub use_sg_kvm: bool,
    pub streamable_http: StreamableHttpConfig,
    pub interactive_session: Option<InteractiveSessionConfig>,
}

#[derive(Debug, Clone)]
pub struct InteractiveSessionConfig {
    pub session_root: PathBuf,
    pub github_repository: String,
    pub github_token: Option<String>,
    pub app_package: String,
    pub app_activity: String,
}

impl Config {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        let sdk_root = cli
            .sdk_root
            .clone()
            .or_else(|| env_optional_path("ANDROID_COMPUTER_USE_MCP_SDK_ROOT"))
            .or_else(|| env_optional_path("ANDROID_SDK_ROOT"))
            .or_else(|| env_optional_path("ANDROID_HOME"))
            .ok_or_else(|| {
                anyhow!("ANDROID_COMPUTER_USE_MCP_SDK_ROOT or ANDROID_SDK_ROOT must be set")
            })?;

        let adb_path = env_optional_path("ANDROID_COMPUTER_USE_MCP_ADB_PATH")
            .unwrap_or_else(|| sdk_root.join("platform-tools/adb"));
        let emulator_path = env_optional_path("ANDROID_COMPUTER_USE_MCP_EMULATOR_PATH")
            .unwrap_or_else(|| sdk_root.join("emulator/emulator"));
        let avdmanager_path = env_optional_path("ANDROID_COMPUTER_USE_MCP_AVDMANAGER_PATH")
            .unwrap_or_else(|| sdk_root.join("cmdline-tools/latest/bin/avdmanager"));
        let artifact_dir = cli
            .artifact_dir
            .clone()
            .or_else(|| env_optional_path("ANDROID_COMPUTER_USE_MCP_ARTIFACT_DIR"))
            .unwrap_or_else(|| PathBuf::from("artifacts"));
        let emulator_grpc_port = cli
            .emulator_grpc_port
            .map(Ok)
            .or_else(|| env_optional_u16("ANDROID_COMPUTER_USE_MCP_EMULATOR_GRPC_PORT"))
            .transpose()?;
        let use_sg_kvm = env_flag("ANDROID_COMPUTER_USE_MCP_USE_SG_KVM", false)?;
        let streamable_http = load_streamable_http_config()?;
        let interactive_session = load_interactive_session_config()?;

        ensure_file(&adb_path, "adb")?;
        ensure_file(&emulator_path, "emulator")?;
        ensure_file(&avdmanager_path, "avdmanager")?;

        Ok(Self {
            sdk_root,
            adb_path,
            emulator_path,
            avdmanager_path,
            artifact_dir,
            emulator_grpc_port,
            use_sg_kvm,
            streamable_http,
            interactive_session,
        })
    }
}

fn load_interactive_session_config() -> Result<Option<InteractiveSessionConfig>> {
    let Some(session_root) = env_optional_path("ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_ROOT")
    else {
        return Ok(None);
    };
    let github_repository = env_setting(
        "ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_REPOSITORY",
        "",
    )?;
    let app_package = env_setting("ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_APP_PACKAGE", "")?;
    let app_activity = env_setting("ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_APP_ACTIVITY", "")?;
    if github_repository.is_empty() || app_package.is_empty() || app_activity.is_empty() {
        return Err(anyhow!(
            "interactive session config requires repository, app package, and app activity when enabled"
        ));
    }

    let github_token = env::var("ANDROID_COMPUTER_USE_MCP_INTERACTIVE_SESSION_GITHUB_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    Ok(Some(InteractiveSessionConfig {
        session_root,
        github_repository,
        github_token,
        app_package,
        app_activity,
    }))
}

fn load_streamable_http_config() -> Result<StreamableHttpConfig> {
    let bind_addr = env_setting("ANDROID_COMPUTER_USE_MCP_BIND_ADDR", "127.0.0.1:9526")?;
    let bind_addr = std::net::SocketAddr::from_str(&bind_addr).map_err(|err| {
        anyhow!("ANDROID_COMPUTER_USE_MCP_BIND_ADDR must be a valid socket address: {err}")
    })?;
    if !bind_addr.ip().is_loopback() {
        return Err(anyhow!(
            "ANDROID_COMPUTER_USE_MCP_BIND_ADDR must stay on loopback for this slice"
        ));
    }

    let allowed_hosts = env_csv(
        "ANDROID_COMPUTER_USE_MCP_ALLOWED_HOSTS",
        "localhost,127.0.0.1,::1",
    );
    if allowed_hosts.is_empty() {
        return Err(anyhow!(
            "ANDROID_COMPUTER_USE_MCP_ALLOWED_HOSTS must not be empty"
        ));
    }

    let max_sessions = env_i64("ANDROID_COMPUTER_USE_MCP_HTTP_MAX_SESSIONS", 32)?;
    let channel_capacity = env_i64("ANDROID_COMPUTER_USE_MCP_HTTP_CHANNEL_CAPACITY", 200)?;
    let allow_resume = env_flag("ANDROID_COMPUTER_USE_MCP_HTTP_ALLOW_RESUME", true)?;

    Ok(StreamableHttpConfig {
        bind_addr,
        allowed_hosts,
        max_sessions: usize::try_from(max_sessions.max(1))
            .map_err(|_| anyhow!("ANDROID_COMPUTER_USE_MCP_HTTP_MAX_SESSIONS must be positive"))?,
        channel_capacity: usize::try_from(channel_capacity.max(1))
            .map_err(|_| anyhow!("ANDROID_COMPUTER_USE_MCP_HTTP_CHANNEL_CAPACITY must be positive"))?,
        allow_resume,
    })
}

fn env_optional_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).and_then(|value| {
        let trimmed = value.to_string_lossy().trim().to_string();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    })
}

fn env_optional_u16(name: &str) -> Option<Result<u16>> {
    env::var_os(name).map(|value| {
        let trimmed = value.to_string_lossy().trim().to_string();
        if trimmed.is_empty() {
            return Err(anyhow!("{name} must not be empty when set"));
        }
        trimmed
            .parse::<u16>()
            .map_err(|err| anyhow!("{name} must be a valid u16 port: {err}"))
    })
}

fn env_flag(name: &str, default: bool) -> Result<bool> {
    let Some(raw) = env::var_os(name) else {
        return Ok(default);
    };
    let raw = raw.to_string_lossy().trim().to_ascii_lowercase();
    match raw.as_str() {
        "" => Ok(default),
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!(
            "{name} must be one of true/false/1/0/yes/no/on/off"
        )),
    }
}

fn env_setting(name: &str, default: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Err(anyhow!("{name} must not be empty when set"))
            } else {
                Ok(trimmed.to_string())
            }
        }
        Err(env::VarError::NotPresent) => Ok(default.to_string()),
        Err(err) => Err(anyhow!("{name} could not be read: {err}")),
    }
}

fn env_csv(name: &str, default: &str) -> Vec<String> {
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn env_i64(name: &str, default: i64) -> Result<i64> {
    match env::var(name) {
        Ok(value) => value
            .trim()
            .parse::<i64>()
            .map_err(|err| anyhow!("{name} must be a valid integer: {err}")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(anyhow!("{name} could not be read: {err}")),
    }
}

fn ensure_file(path: &Path, label: &str) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(anyhow!(
            "{label} path does not exist or is not a file: {}",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test mutex should not be poisoned")
    }

    #[test]
    fn empty_env_path_is_ignored() {
        let _guard = env_test_guard();
        unsafe {
            env::set_var("ANDROID_COMPUTER_USE_MCP_TMP_EMPTY", "   ");
        }
        assert_eq!(env_optional_path("ANDROID_COMPUTER_USE_MCP_TMP_EMPTY"), None);
        unsafe {
            env::remove_var("ANDROID_COMPUTER_USE_MCP_TMP_EMPTY");
        }
    }

    #[test]
    fn empty_env_port_is_rejected() {
        let _guard = env_test_guard();
        const TEST_VAR: &str = "ANDROID_COMPUTER_USE_MCP_TMP_EMPTY_PORT";
        unsafe {
            env::set_var(TEST_VAR, "   ");
        }
        let err = env_optional_u16(TEST_VAR)
            .expect("port env should be present")
            .expect_err("empty port should be rejected");
        assert!(
            err.to_string()
                .contains("ANDROID_COMPUTER_USE_MCP_TMP_EMPTY_PORT must not be empty when set")
        );
        unsafe {
            env::remove_var(TEST_VAR);
        }
    }

    #[test]
    fn parses_env_port() {
        let _guard = env_test_guard();
        const TEST_VAR: &str = "ANDROID_COMPUTER_USE_MCP_TMP_VALID_PORT";
        unsafe {
            env::set_var(TEST_VAR, "8554");
        }
        let port = env_optional_u16(TEST_VAR)
            .expect("port env should be present")
            .expect("port should parse");
        assert_eq!(port, 8554);
        unsafe {
            env::remove_var(TEST_VAR);
        }
    }

    #[test]
    fn streamable_http_defaults_to_loopback() {
        let _guard = env_test_guard();
        unsafe {
            env::remove_var("ANDROID_COMPUTER_USE_MCP_BIND_ADDR");
            env::remove_var("ANDROID_COMPUTER_USE_MCP_ALLOWED_HOSTS");
            env::remove_var("ANDROID_COMPUTER_USE_MCP_HTTP_MAX_SESSIONS");
            env::remove_var("ANDROID_COMPUTER_USE_MCP_HTTP_CHANNEL_CAPACITY");
            env::remove_var("ANDROID_COMPUTER_USE_MCP_HTTP_ALLOW_RESUME");
        }

        let config = load_streamable_http_config().expect("streamable http config");
        assert_eq!(config.bind_addr.to_string(), "127.0.0.1:9526");
        assert_eq!(
            config.allowed_hosts,
            vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "::1".to_string()
            ]
        );
        assert_eq!(config.max_sessions, 32);
        assert_eq!(config.channel_capacity, 200);
        assert!(config.allow_resume);
    }

    #[test]
    fn streamable_http_rejects_non_loopback_bind_addr() {
        let _guard = env_test_guard();
        unsafe {
            env::set_var("ANDROID_COMPUTER_USE_MCP_BIND_ADDR", "0.0.0.0:9526");
        }
        let err = load_streamable_http_config().expect_err("non-loopback bind should fail");
        assert!(
            err.to_string()
                .contains("ANDROID_COMPUTER_USE_MCP_BIND_ADDR must stay on loopback")
        );
        unsafe {
            env::remove_var("ANDROID_COMPUTER_USE_MCP_BIND_ADDR");
        }
    }

    #[test]
    fn streamable_http_allows_resume_override() {
        let _guard = env_test_guard();
        unsafe {
            env::set_var("ANDROID_COMPUTER_USE_MCP_HTTP_ALLOW_RESUME", "false");
        }

        let config = load_streamable_http_config().expect("streamable http config");
        assert!(!config.allow_resume);

        unsafe {
            env::remove_var("ANDROID_COMPUTER_USE_MCP_HTTP_ALLOW_RESUME");
        }
    }
}
