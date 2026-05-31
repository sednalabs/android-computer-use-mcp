//! Local emulator discovery helpers.
//!
//! ## Rationale
//! The Android emulator publishes runtime metadata under the per-user
//! `avd/running` directory. We use that local-only surface to find the gRPC
//! port and optional auth token for a given adb serial.
//!
//! ## Security Boundaries
//! * Operates strictly on local `avd/running` metadata.
//! * No network I/O; treats filesystem metadata as untrusted inputs.
//!
//! ## References
//! * [Android Emulator Console](https://developer.android.com/studio/run/emulator-console)

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcEndpoint {
    pub port: u16,
    pub auth_token: Option<String>,
}

pub fn grpc_endpoint_for_serial(serial: &str) -> Option<GrpcEndpoint> {
    let serial_port = serial.strip_prefix("emulator-")?.parse::<u16>().ok()?;
    running_ini_paths()
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|contents| parse_ini_map(&contents))
        .find_map(|entry| grpc_endpoint_from_entry(&entry, serial_port))
}

pub fn any_grpc_endpoint_published() -> bool {
    running_ini_paths().into_iter().any(|path| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| parse_ini_map(&contents))
            .and_then(|entry| {
                entry
                    .get("grpc.port")
                    .and_then(|value| value.parse::<u16>().ok())
            })
            .is_some()
    })
}

fn grpc_endpoint_from_entry(
    entry: &HashMap<String, String>,
    serial_port: u16,
) -> Option<GrpcEndpoint> {
    let published_serial = entry.get("port.serial")?.parse::<u16>().ok()?;
    if published_serial != serial_port {
        return None;
    }
    let port = entry.get("grpc.port")?.parse::<u16>().ok()?;
    let auth_token = entry
        .get("grpc.token")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Some(GrpcEndpoint { port, auth_token })
}

fn running_ini_paths() -> Vec<PathBuf> {
    let mut paths = running_dirs()
        .into_iter()
        .filter_map(|running_dir| std::fs::read_dir(running_dir).ok())
        .flat_map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        })
        .filter(|path| is_running_ini(path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn is_running_ini(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with("pid_") && name.ends_with(".ini"))
        .unwrap_or(false)
}

fn running_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        let candidate = runtime_dir.join("avd/running");
        if candidate.is_dir() {
            dirs.push(candidate);
        }
    }

    if let Some(uid_dir) =
        env::var_os("UID").map(|uid| PathBuf::from(format!("/run/user/{}", uid.to_string_lossy())))
    {
        let candidate = uid_dir.join("avd/running");
        if candidate.is_dir() {
            dirs.push(candidate);
        }
    }

    if let Ok(user_dirs) = std::fs::read_dir("/run/user") {
        for user_dir in user_dirs.flatten().map(|entry| entry.path()) {
            let candidate = user_dir.join("avd/running");
            if candidate.is_dir() {
                dirs.push(candidate);
            }
        }
    }

    dirs.sort();
    dirs.dedup();
    dirs
}

fn parse_ini_map(contents: &str) -> Option<HashMap<String, String>> {
    let mut map = HashMap::new();
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let (key, value) = line.split_once('=')?;
        map.insert(
            key.trim().to_string(),
            value.trim().trim_matches('"').to_string(),
        );
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_grpc_endpoint_from_matching_serial_entry() {
        let mut entry = HashMap::new();
        entry.insert("port.serial".to_string(), "5554".to_string());
        entry.insert("grpc.port".to_string(), "8554".to_string());
        entry.insert("grpc.token".to_string(), "token-123".to_string());
        assert_eq!(
            grpc_endpoint_from_entry(&entry, 5554),
            Some(GrpcEndpoint {
                port: 8554,
                auth_token: Some("token-123".to_string()),
            })
        );
        assert_eq!(grpc_endpoint_from_entry(&entry, 5556), None);
    }

    #[test]
    fn parses_ini_map_with_quoted_values() {
        let parsed = parse_ini_map("port.serial=5554\ngrpc.token=\"abc\"\n").unwrap();
        assert_eq!(parsed.get("port.serial").map(String::as_str), Some("5554"));
        assert_eq!(parsed.get("grpc.token").map(String::as_str), Some("abc"));
    }
}
