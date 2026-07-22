//! # android-computer-use-mcp
//!
//! Local Android computer-use MCP server for ADB-first automation and artifact capture.
//!
//! ## Rationale
//! Keep the harness slice small, explicit, and reusable:
//! * loopback Streamable HTTP transport
//! * typed local config
//! * focused tool surface for emulator boot, app launch, screenshots, UI dump, and input
//!
//! ## Security Boundaries
//! * Shell execution is restricted to explicit `adb`, `emulator`, and `avdmanager` paths from config.
//! * Artifacts are written only under the configured artifact directory.
//! * This slice is local-only and binds only to loopback.
//!
pub mod config;
pub mod discovery;
pub mod emulator_grpc;
pub mod grpc_backend;
pub mod http_runtime;
pub mod interactive_session;
pub mod resources;
pub mod server;
pub mod tool_surface;
pub mod tools;
pub mod ui;
pub mod verification;
pub mod window_views;

pub type McpError = rmcp::ErrorData;
