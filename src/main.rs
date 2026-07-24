//! # android-computer-use-mcp Main
//!
//! Entrypoint for the local loopback Streamable HTTP Android computer-use MCP server.
//!
//! ## Rationale
//! Start with the smallest reliable long-lived MCP transport surface for local Android UX automation:
//! loopback Streamable HTTP, typed config, explicit tool registration, and easy operator restart.
//!
//! ## Security Boundaries
//! * The server binds only to loopback in this slice.
//! * Tool execution is restricted to configured local Android SDK binaries.
//!
use anyhow::Result;
use clap::Parser;
use mcp_toolkit_core::tool_schema::{tool_names, tool_schema_snapshot_value};
use tracing_subscriber::EnvFilter;

use android_computer_use_mcp::config::{Cli, Config};
use android_computer_use_mcp::{http_runtime, server::AndroidEmulatorMcp};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("android-computer-use-mcp failed to start: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    if cli.print_tools {
        let tools = AndroidEmulatorMcp::tool_router_android().list_all();
        println!("{}", serde_json::to_string_pretty(&tool_names(&tools)?)?);
        return Ok(());
    }

    if cli.print_tool_schema {
        let tools = AndroidEmulatorMcp::tool_router_android().list_all();
        println!(
            "{}",
            serde_json::to_string_pretty(&tool_schema_snapshot_value(&tools)?)?
        );
        return Ok(());
    }

    let config = Config::from_cli(&cli)?;
    let server = AndroidEmulatorMcp::new(config.clone());

    if let Some(name) = cli.run_scenario.as_deref() {
        let result = server
            .run_solarlab_scenario(
                name,
                cli.serial.as_deref(),
                cli.package_name.as_deref(),
                cli.activity.as_deref(),
            )
            .await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    http_runtime::serve(config).await
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rmcp=warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
