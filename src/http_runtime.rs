//! Loopback Streamable HTTP runtime for android-computer-use-mcp.
//!
//! ## Rationale
//! Implements a loopback HTTP transport layer for the MCP server, providing
//! a reliable, streamable interface for local communication between the
//! LLM controller and the emulator server.
//!
//! ## Security Boundaries
//! * Server binds strictly to loopback interface.
//! * Does not expose external network ports.
//!
//! ## References
//! * [Model Context Protocol Specification](https://modelcontextprotocol.io)

use std::sync::Arc;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use mcp_toolkit_http::{
    oauth::protected_resource_well_known_paths,
    session::{BoundedSessionManager, RecordingSessionManager, SessionStats},
    streamable::{
        LocalStreamableHttpServiceConfig, build_local_streamable_http_service,
        handle_stateful_mcp_request,
    },
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::SessionConfig,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{config::Config, server::AndroidEmulatorMcp};

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    service: StreamableHttpService<AndroidEmulatorMcp, RecordingSessionManager>,
    session_manager: Arc<BoundedSessionManager>,
}

pub async fn serve(config: Config) -> Result<()> {
    let config = Arc::new(config);
    let shutdown = CancellationToken::new();
    let mut session_config = SessionConfig::default();
    session_config.channel_capacity = config.streamable_http.channel_capacity;
    let runtime = build_local_streamable_http_service(
        {
            let config = config.clone();
            move || Ok(AndroidEmulatorMcp::new((*config).clone()))
        },
        LocalStreamableHttpServiceConfig {
            max_sessions: config.streamable_http.max_sessions,
            allow_resume: config.streamable_http.allow_resume,
            session_config,
            server_config: StreamableHttpServerConfig::default()
                .with_allowed_hosts(config.streamable_http.allowed_hosts.clone())
                .with_cancellation_token(shutdown.child_token()),
        },
    );

    let state = AppState {
        config: config.clone(),
        service: runtime.service.clone(),
        session_manager: runtime.session_manager.clone(),
    };

    let mut router = Router::new();
    for path in protected_resource_well_known_paths("/mcp") {
        router = router.route(&path, get(oauth_protected_resource_not_configured));
    }
    let router = router
        .route("/health", get(health))
        .route("/mcp", any(handle_mcp))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.streamable_http.bind_addr).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(bind_addr = %local_addr, "android-computer-use-mcp listening");

    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            tokio::signal::ctrl_c().await.ok();
            shutdown.cancel();
        }
    });

    axum::serve(listener, router)
        .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
        .await?;

    Ok(())
}

async fn handle_mcp(State(state): State<AppState>, req: Request) -> Response {
    handle_stateful_mcp_request(state.service.clone(), state.session_manager.clone(), req).await
}

async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let stats = state.session_manager.stats().await;
    Json(json!({
        "status": "ok",
        "bind_addr": state.config.streamable_http.bind_addr.to_string(),
        "artifact_dir": state.config.artifact_dir.display().to_string(),
        "sdk_root": state.config.sdk_root.display().to_string(),
        "resume_enabled": state.config.streamable_http.allow_resume,
        "session": session_stats_json(stats),
    }))
}

fn session_stats_json(stats: SessionStats) -> serde_json::Value {
    json!({
        "active_sessions": stats.active_sessions,
        "max_sessions": stats.max_sessions,
        "resume_enabled": stats.resume_enabled,
        "lifecycle_mode": format!("{:?}", stats.lifecycle_mode).to_lowercase(),
        "lifecycle_connected_streams": stats.lifecycle_connected_streams,
        "lifecycle_disconnected_sessions": stats.lifecycle_disconnected_sessions,
        "lifecycle_expired_sessions_total": stats.lifecycle_expired_sessions_total,
    })
}

async fn oauth_protected_resource_not_configured() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({
            "status": "not_configured",
            "error": "OAuth protected resource metadata is not configured for this local loopback slice."
        })),
    )
}
