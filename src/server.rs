//! MCP server handler and tool routing.
//!
//! ## Rationale
//! Orchestrates the MCP server lifecycle, including capability registration
//! and the dispatching of incoming tool calls to the appropriate handlers.
//!
//! ## Security Boundaries
//! * The server trust is anchored to tools registered via the `ToolRouter`.
//! * All tool calls are validated against the `ToolCallContext`.
//!
use std::future::Future;
use std::sync::Arc;

use axum::http::request::Parts;
use mcp_toolkit_core::notifications::{ToolListTracker, ToolListUpdate};
use mcp_toolkit_core::rmcp_models;
use mcp_toolkit_core::tool_inventory::{ToolInventory, ToolInventoryPolicy, ToolOperation};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolRequestMethod, CallToolRequestParams, CallToolResult, Implementation,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResult, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::common::http_header::HEADER_SESSION_ID;
use rmcp::{RoleServer, ServerHandler};

use crate::config::Config;
use crate::{resources, tool_surface};

#[derive(Clone)]
pub struct AndroidEmulatorMcp {
    pub config: Arc<Config>,
    tool_router: ToolRouter<AndroidEmulatorMcp>,
    tool_inventory: ToolInventory,
    tool_inventory_policy: ToolInventoryPolicy,
    tool_list_tracker: Arc<ToolListTracker>,
}

impl AndroidEmulatorMcp {
    fn protocol_version() -> ProtocolVersion {
        ProtocolVersion::V_2025_06_18
    }

    /// Summary
    /// Create a new Android emulator MCP server with the registered tool router.
    ///
    /// # Errors
    /// This constructor does not currently fail.
    ///
    /// # Security
    /// The server trusts only explicit tool paths configured at startup.
    ///
    /// # Panics
    /// Does not panic.
    pub fn new(config: Config) -> Self {
        let tool_inventory = tool_surface::build_tool_inventory()
            .expect("android-computer-use-mcp tool inventory registration must remain valid");
        Self {
            config: Arc::new(config),
            tool_router: Self::tool_router_android(),
            tool_inventory,
            tool_inventory_policy: ToolInventoryPolicy::strict(),
            tool_list_tracker: Arc::new(ToolListTracker::new()),
        }
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.filtered_tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    fn filtered_tools(&self) -> Vec<Tool> {
        self.tool_inventory.filter_tools(
            self.tool_router.list_all(),
            ToolOperation::List,
            &self.tool_inventory_policy,
            |tool| tool.name.as_ref(),
        )
    }

    async fn maybe_notify_tool_list_changed(
        &self,
        session_id: Option<&str>,
        peer: &rmcp::service::Peer<RoleServer>,
    ) {
        let session_id = session_id.map(str::trim).filter(|value| !value.is_empty());
        let Some(session_id) = session_id else {
            return;
        };
        let tools = self.filtered_tools();
        let update = self
            .tool_list_tracker
            .observe(session_id, tools.iter().map(|tool| tool.name.as_ref()));
        if matches!(update, ToolListUpdate::Changed { .. }) {
            if let Err(err) = peer.notify_tool_list_changed().await {
                tracing::debug!(error = %err, session_id, "tools list_changed notification failed");
            }
        }
    }
}

fn session_id_from_context(context: &RequestContext<RoleServer>) -> Option<String> {
    context.extensions.get::<Parts>().and_then(|parts| {
        parts
            .headers
            .get(HEADER_SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

impl ServerHandler for AndroidEmulatorMcp {
    fn get_info(&self) -> ServerInfo {
        rmcp_models::server_info(
            Self::protocol_version(),
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_tool_list_changed()
                .build(),
            Implementation::from_build_env(),
            Some(
                "Local Android computer-use MCP for semantic interaction, raw input fallback, and optional scenario tools."
                    .to_string(),
            ),
        )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
        let tools = self.filtered_tools();
        if let Some(session_id) = session_id_from_context(&context) {
            let _ = self
                .tool_list_tracker
                .observe(&session_id, tools.iter().map(|tool| tool.name.as_ref()));
        }
        std::future::ready(Ok(ListToolsResult {
            meta: None,
            tools,
            next_cursor: None,
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_ {
        let call_allowed = self.tool_inventory.is_allowed(
            &request.name,
            ToolOperation::Call,
            &self.tool_inventory_policy,
        );
        let session_id = session_id_from_context(&context);
        let peer = context.peer.clone();
        let tool_context = ToolCallContext::new(self, request, context);
        async move {
            if !call_allowed {
                return Err(rmcp::ErrorData::method_not_found::<CallToolRequestMethod>());
            }
            let result = self.tool_router.call(tool_context).await;
            if let Ok(payload) = &result {
                if !payload.is_error.unwrap_or(false) {
                    self.maybe_notify_tool_list_changed(session_id.as_deref(), &peer)
                        .await;
                }
            }
            result
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult {
            resources: resources::list_resources(),
            next_cursor: None,
            meta: None,
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_ {
        std::future::ready(resources::read_resource(&request.uri))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::AndroidEmulatorMcp;
    use crate::config::{Config, ProviderExecutionIdentity, StreamableHttpConfig};
    use crate::tool_surface::build_tool_inventory;
    use mcp_toolkit_core::tool_inventory::{ToolInventoryPolicy, ToolOperation};

    fn test_config() -> Config {
        Config {
            sdk_root: PathBuf::from("/tmp/android-sdk"),
            adb_path: PathBuf::from("/tmp/android-sdk/platform-tools/adb"),
            emulator_path: PathBuf::from("/tmp/android-sdk/emulator/emulator"),
            avdmanager_path: PathBuf::from("/tmp/android-sdk/cmdline-tools/latest/bin/avdmanager"),
            artifact_dir: PathBuf::from("/tmp/android-computer-use-mcp-artifacts"),
            emulator_grpc_port: Some(8554),
            use_sg_kvm: false,
            streamable_http: StreamableHttpConfig {
                bind_addr: "127.0.0.1:9526".parse().expect("bind addr"),
                allowed_hosts: vec!["localhost".to_string()],
                max_sessions: 8,
                channel_capacity: 32,
                allow_resume: true,
            },
            interactive_session: None,
            execution_identity: ProviderExecutionIdentity {
                environment_id: "test-environment".to_string(),
                provider_instance_id: "test-provider".to_string(),
                session_id: "test-session".to_string(),
            },
        }
    }

    #[test]
    fn strict_inventory_denies_unregistered_tools() {
        let server = AndroidEmulatorMcp::new(test_config());
        assert!(!server.tool_inventory_policy.include_unregistered);
        assert!(!server.tool_inventory.is_allowed(
            "android.unknown",
            ToolOperation::List,
            &server.tool_inventory_policy,
        ));
        assert!(!server.tool_inventory.is_allowed(
            "android.unknown",
            ToolOperation::Call,
            &server.tool_inventory_policy,
        ));
    }

    #[test]
    fn inventory_registration_covers_router_surface() {
        let server = AndroidEmulatorMcp::new(test_config());
        let inventory = build_tool_inventory().expect("inventory should build");
        let expected = inventory.filter_tools(
            server.tool_router.list_all(),
            ToolOperation::List,
            &ToolInventoryPolicy::strict(),
            |tool| tool.name.as_ref(),
        );

        let mut expected_names = expected
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        let mut actual_names = server.tool_names();
        expected_names.sort();
        actual_names.sort();

        assert_eq!(actual_names, expected_names);
        for tool_name in expected_names {
            assert!(server.tool_inventory.is_allowed(
                &tool_name,
                ToolOperation::Call,
                &server.tool_inventory_policy,
            ));
        }
    }
}
