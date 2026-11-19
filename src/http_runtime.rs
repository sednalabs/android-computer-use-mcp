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

use std::{error::Error, sync::Arc};

use anyhow::Result;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode, header},
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
use rmcp::{
    model::{ClientJsonRpcMessage, ClientRequest, ErrorCode, GetMeta, ProtocolVersion, RequestId},
    transport::{
        common::http_header::HEADER_SESSION_ID,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::SessionConfig,
        },
    },
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, server::AndroidEmulatorMcp};

// Match the toolkit's bound before inspecting any sessionless request.
const SESSIONLESS_BODY_LIMIT: usize = 64 * 1024;

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
            // Every HTTP session must share device leases and observation IDs.
            let server = AndroidEmulatorMcp::new((*config).clone());
            move || Ok(server.new_session())
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
    if req.method() == Method::POST && !req.headers().contains_key(HEADER_SESSION_ID) {
        let (parts, body) = req.into_parts();
        let bytes = match to_bytes(body, SESSIONLESS_BODY_LIMIT).await {
            Ok(bytes) => bytes,
            Err(error) => {
                let too_large = error
                    .source()
                    .is_some_and(|source| source.is::<http_body_util::LengthLimitError>());
                return (
                    if too_large {
                        StatusCode::PAYLOAD_TOO_LARGE
                    } else {
                        StatusCode::BAD_REQUEST
                    },
                    Json(json!({
                        "status": "error",
                        "error": if too_large { "Request body too large." } else { "Failed to read request body." },
                    })),
                )
                    .into_response();
            }
        };
        let probe = modern_discovery_probe(&bytes);
        let req = Request::from_parts(parts, Body::from(bytes));
        if let Some((id, requested_version)) = probe {
            // The SDK still performs Host/Origin, media-type and protocol-header
            // checks. Only its exact qualified-version rejection is adapted;
            // this route cannot invoke an Android tool or allocate a session.
            let response = state.service.handle(req).await.map(Body::new);
            return legacy_discovery_response(response, &id, &requested_version).await;
        }
        return handle_stateful_mcp_request(
            state.service.clone(),
            state.session_manager.clone(),
            req,
        )
        .await;
    }
    handle_stateful_mcp_request(state.service.clone(), state.session_manager.clone(), req).await
}

fn modern_discovery_probe(body: &[u8]) -> Option<(RequestId, ProtocolVersion)> {
    let ClientJsonRpcMessage::Request(request) = serde_json::from_slice(body).ok()? else {
        return None;
    };
    if !matches!(&request.request, ClientRequest::DiscoverRequest(_)) {
        return None;
    }
    let requested_version = request.request.get_meta().protocol_version()?;
    // Do not enable the SDK's stateless discovery lifecycle for legacy versions.
    if requested_version.has_initialize() {
        return None;
    }
    Some((request.id, requested_version))
}

fn legacy_discovery_error(
    body: &[u8],
    id: &RequestId,
    requested_version: &ProtocolVersion,
) -> Option<Value> {
    let response: Value = serde_json::from_slice(body).ok()?;
    if response.get("jsonrpc") != Some(&json!("2.0"))
        || response.get("id") != Some(&json!(id))
        || response.get("result").is_some()
        || response["error"]["code"] != json!(ErrorCode::UNSUPPORTED_PROTOCOL_VERSION)
        || response["error"]["data"]["requested"] != json!(requested_version)
        || response["error"]["data"]["supported"]
            != json!(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_06_18))
    {
        return None;
    }
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": ErrorCode::METHOD_NOT_FOUND,
            "message": "Method not found. This provider requires the initialize handshake.",
        },
    }))
}

async fn legacy_discovery_response(
    response: Response,
    id: &RequestId,
    requested_version: &ProtocolVersion,
) -> Response {
    if response.status() != StatusCode::BAD_REQUEST
        || response.headers().contains_key(HEADER_SESSION_ID)
        || !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("application/json"))
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, SESSIONLESS_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                "Failed to read the bounded discovery rejection.",
            )
                .into_response();
        }
    };
    if let Some(error) = legacy_discovery_error(&bytes, id, requested_version) {
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::from(error.to_string()));
    }
    Response::from_parts(parts, Body::from(bytes))
}

async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let stats = state.session_manager.stats().await;
    Json(json!({
        "status": "ok",
        "bind_addr": state.config.streamable_http.bind_addr.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery_request(id: Value, version: ProtocolVersion) -> Value {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "server/discover",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": version,
                "io.modelcontextprotocol/clientInfo": { "name": "fixture", "version": "1" },
                "io.modelcontextprotocol/clientCapabilities": {},
            } },
        })
    }

    #[test]
    fn discovery_adapter_only_probes_valid_modern_requests() {
        for id in [json!(0), json!("discovery-probe")] {
            let request = discovery_request(id.clone(), ProtocolVersion::V_2026_07_28);
            let (actual_id, _) = modern_discovery_probe(&serde_json::to_vec(&request).unwrap())
                .expect("valid modern discovery request");
            assert_eq!(json!(actual_id), id);
        }

        let valid = discovery_request(json!(0), ProtocolVersion::V_2026_07_28);
        let mut notification = valid.clone();
        notification.as_object_mut().unwrap().remove("id");
        let mut unrelated = valid.clone();
        unrelated["method"] = json!("tools/list");
        let mut malformed = valid.clone();
        malformed["jsonrpc"] = json!("1.0");
        let legacy = discovery_request(json!(0), ProtocolVersion::V_2025_06_18);
        for request in [notification, unrelated, malformed, legacy, json!([valid])] {
            assert!(modern_discovery_probe(&serde_json::to_vec(&request).unwrap()).is_none());
        }
    }

    fn unsupported_discovery_error(id: &RequestId) -> Value {
        json!({
            "jsonrpc": "2.0", "id": id,
            "error": {
                "code": ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
                "message": "Unsupported protocol version",
                "data": {
                    "requested": ProtocolVersion::V_2026_07_28,
                    "supported": ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_06_18),
                },
            },
        })
    }

    #[test]
    fn discovery_adapter_requires_correlated_exact_qualified_version_rejection() {
        let id = RequestId::Number(0);
        let version = ProtocolVersion::V_2026_07_28;
        let response = unsupported_discovery_error(&id);
        let adapted =
            legacy_discovery_error(&serde_json::to_vec(&response).unwrap(), &id, &version)
                .expect("qualified rejection");
        assert_eq!(adapted["id"], json!(id));
        assert_eq!(adapted["error"]["code"], json!(ErrorCode::METHOD_NOT_FOUND));
        assert!(adapted["error"].get("data").is_none());
        assert!(adapted.get("result").is_none());

        let mut wrong_id = response.clone();
        wrong_id["id"] = json!(1);
        let mut header_error = response.clone();
        header_error["error"]["code"] = json!(ErrorCode::HEADER_MISMATCH);
        let mut wrong_requested = response.clone();
        wrong_requested["error"]["data"]["requested"] = json!("2025-11-25");
        let mut changed_ceiling = response.clone();
        changed_ceiling["error"]["data"]["supported"] =
            json!(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25));
        let mut partial_support = response.clone();
        partial_support["error"]["data"]["supported"] = json!(["2025-06-18"]);
        let mut mixed_result = response;
        mixed_result["result"] = json!({});
        for response in [
            wrong_id,
            header_error,
            wrong_requested,
            changed_ceiling,
            partial_support,
            mixed_result,
        ] {
            assert!(
                legacy_discovery_error(&serde_json::to_vec(&response).unwrap(), &id, &version)
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn discovery_adapter_preserves_transport_rejections_and_allocated_sessions() {
        let id = RequestId::Number(0);
        let version = ProtocolVersion::V_2026_07_28;
        let body = unsupported_discovery_error(&id).to_string();
        for (status, media_type, session) in [
            (StatusCode::UNAUTHORIZED, "application/json", None),
            (StatusCode::FORBIDDEN, "application/json", None),
            (StatusCode::BAD_REQUEST, "text/plain", None),
            (StatusCode::BAD_REQUEST, "application/json", Some("session")),
        ] {
            let mut response = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, media_type);
            if let Some(session) = session {
                response = response.header(HEADER_SESSION_ID, session);
            }
            let response = response.body(Body::from(body.clone())).unwrap();
            let response = legacy_discovery_response(response, &id, &version).await;
            assert_eq!(response.status(), status);
            assert_eq!(
                to_bytes(response.into_body(), SESSIONLESS_BODY_LIMIT)
                    .await
                    .unwrap()
                    .as_ref(),
                body.as_bytes()
            );
        }
    }
}
