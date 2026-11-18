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
    CallToolRequestMethod, CallToolRequestParams, CallToolResponse, Implementation,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::common::http_header::HEADER_SESSION_ID;
use rmcp::{RoleServer, ServerHandler};

use crate::config::Config;
use crate::{resources, tool_surface};

#[derive(Clone)]
pub struct AndroidEmulatorMcp {
    pub config: Arc<Config>,
    pub(crate) workflow: Arc<crate::execution::ExecutionState>,
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
            workflow: Arc::new(crate::execution::ExecutionState::default()),
            tool_router: Self::tool_router_all(),
            tool_inventory,
            tool_inventory_policy: ToolInventoryPolicy::strict(),
            tool_list_tracker: Arc::new(ToolListTracker::new()),
        }
    }

    pub fn tool_router_all() -> ToolRouter<Self> {
        Self::tool_router_android() + Self::tool_router_workflow() + Self::tool_router_execution()
    }

    pub(crate) fn new_session(&self) -> Self {
        let mut session = self.clone();
        // Device state is shared; discovery tracking expires with its session.
        session.tool_list_tracker = Arc::new(ToolListTracker::new());
        session
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
        if matches!(update, ToolListUpdate::Changed { .. })
            && let Err(err) = peer.notify_tool_list_changed().await
        {
            tracing::debug!(error = %err, session_id, "tools list_changed notification failed");
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
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        // An SDK upgrade must not opt this provider into a new lifecycle before
        // its cancellation and image-bearing consumer contracts are qualified.
        std::borrow::Cow::Borrowed(ProtocolVersion::known_up_to(&Self::protocol_version()))
    }

    fn get_info(&self) -> ServerConfig {
        rmcp_models::server_info(
            Self::protocol_version(),
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_tool_list_changed()
                .build(),
            Implementation::from_build_env(),
            Some(
                "Local Android computer-use MCP for semantic Android interaction, raw input fallback, and scenario tools."
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
        std::future::ready(Ok(ListToolsResult::with_all_items(tools)))
    }

    fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, rmcp::ErrorData>> + Send + '_ {
        let call_allowed = self.tool_inventory.is_allowed(
            &request.name,
            ToolOperation::Call,
            &self.tool_inventory_policy,
        );
        let session_id = session_id_from_context(&context);
        let peer = context.peer.clone();
        let cancellation = context.ct.clone();
        async move {
            if !call_allowed {
                return Err(rmcp::ErrorData::method_not_found::<CallToolRequestMethod>());
            }
            let _leases = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(cancelled_before_dispatch()),
                leases = self.acquire_operation_leases(&mut request) => leases?,
            };
            if cancellation.is_cancelled() {
                return Err(cancelled_before_dispatch());
            }
            let tool_context = ToolCallContext::new(self, request, context);
            let result = crate::execution::REQUEST_CANCELLATION
                .scope(cancellation, self.tool_router.call(tool_context))
                .await;
            if let Ok(CallToolResponse::Complete(payload)) = &result
                && !payload.is_error.unwrap_or(false)
            {
                self.maybe_notify_tool_list_changed(session_id.as_deref(), &peer)
                    .await;
            }
            result
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult::with_all_items(
            resources::list_resources(),
        )))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, rmcp::ErrorData>> + Send + '_ {
        std::future::ready(resources::read_resource(&request.uri).map(Into::into))
    }
}

impl AndroidEmulatorMcp {
    async fn acquire_operation_leases(
        &self,
        request: &mut CallToolRequestParams,
    ) -> Result<Vec<tokio::sync::OwnedMutexGuard<()>>, crate::McpError> {
        let name = request.name.to_string();
        if matches!(
            name.as_str(),
            "android.health"
                | "android.list_avds"
                | "android.list_devices"
                | "android.read_artifact"
                | "android.get_action_status"
                | "android.resolve_target"
        ) {
            return Ok(Vec::new());
        }
        let mut leases = Vec::new();
        let hosted =
            name.starts_with("interactive_session.") || name == "android.install_build_from_run";
        // All hosted installs share active-build.json regardless of device.
        if hosted {
            leases.push(self.operation_lease("hosted-build-state").await?);
            if matches!(
                name.as_str(),
                "interactive_session.get_status" | "interactive_session.get_current_build"
            ) {
                return Ok(leases);
            }
        }
        if name.starts_with("android.launch_avd") {
            leases.push(self.operation_lease("emulator-launch").await?);
            return Ok(leases);
        }
        let arguments = request.arguments.get_or_insert_with(Default::default);
        let mut nested = serde_json::Map::new();
        let values = if name == "android.install_build_from_run" {
            nested = arguments
                .get("build")
                .and_then(serde_json::Value::as_object)
                .cloned()
                .ok_or_else(|| crate::McpError::invalid_params("build must be an object", None))?;
            &mut nested
        } else {
            &mut *arguments
        };
        let serial = match values.get("serial") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            _ => {
                return Err(crate::McpError::invalid_params(
                    "serial must be a nonempty string",
                    None,
                ));
            }
        };
        let target: Option<crate::config::AndroidExecutionTarget> = values
            .get("target")
            .filter(|v| !v.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| crate::McpError::invalid_params(format!("invalid target: {e}"), None))?;
        let target = self
            .resolve_android_execution_target(target.as_ref(), serial.as_deref())
            .await?;
        let serial = target.device_serial;
        values.insert("serial".into(), serde_json::json!(serial));
        if name == "android.install_build_from_run" {
            arguments.insert("build".into(), serde_json::Value::Object(nested));
        }
        leases.push(self.operation_lease(&format!("device:{serial}")).await?);
        if is_direct_device_mutation(&name) {
            self.workflow.invalidate(&serial).await;
        }
        Ok(leases)
    }
    async fn operation_lease(
        &self,
        key: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, crate::McpError> {
        tokio::time::timeout(std::time::Duration::from_secs(30), self.workflow.lock(key)).await
            .map_err(|_| crate::McpError::internal_error("device busy; no action dispatched", Some(serde_json::json!({"outcome":"provider_busy","retry_safe":true,"do_not_replay":false}))))
    }
}

fn is_direct_device_mutation(name: &str) -> bool {
    !matches!(
        name,
        "android.observe"
            | "android.step"
            | "android.install_build_from_run"
            | "android.list_apps"
            | "android.get_orientation"
            | "android.wait_for_boot"
            | "android.capture_screenshot"
            | "android.dump_ui_hierarchy"
            | "android.inspect_ui"
            | "android.wait_for_stable_ui"
            | "android.find_ui_element"
            | "android.wait_for_ui_element"
            | "android.collect_logcat"
    )
}

fn cancelled_before_dispatch() -> crate::McpError {
    crate::McpError::internal_error(
        "request cancelled before dispatch",
        Some(serde_json::json!({
            "outcome":"cancelled", "retry_safe":true, "do_not_replay":false,
            "mutation_dispatched":false,
        })),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::AndroidEmulatorMcp;
    use crate::config::{Config, StreamableHttpConfig};
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
            execution_identity: crate::config::ProviderExecutionIdentity {
                environment_id: "test".into(),
                provider_instance_id: "test-provider".into(),
                provider_epoch: "test-epoch".into(),
                session_id: "test-session".into(),
            },
        }
    }

    #[test]
    fn transport_sessions_share_leases_but_expire_their_own_discovery_state() {
        let server = AndroidEmulatorMcp::new(test_config());
        let session = server.new_session();
        assert!(std::sync::Arc::ptr_eq(&server.workflow, &session.workflow));
        assert!(!std::sync::Arc::ptr_eq(
            &server.tool_list_tracker,
            &session.tool_list_tracker
        ));
    }

    #[test]
    fn sdk_upgrade_preserves_the_qualified_protocol_ceiling() {
        use rmcp::ServerHandler;
        let server = AndroidEmulatorMcp::new(test_config());
        let request: rmcp::model::InitializeRequestParams =
            serde_json::from_value(serde_json::json!({
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": { "name": "protocol-ceiling-test", "version": "1" }
            }))
            .expect("valid initialization request");
        let response = server
            .negotiate_initialize(&request)
            .expect("legacy fallback");
        assert_eq!(
            response.protocol_version,
            super::ProtocolVersion::V_2025_06_18
        );
        assert!(
            !server
                .supported_protocol_versions()
                .contains(&super::ProtocolVersion::V_2026_07_28)
        );
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
