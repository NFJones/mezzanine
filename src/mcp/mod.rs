//! MCP integration state.
//!
//! MCP servers are external integrations from the agent perspective. This module
//! models configured server state, availability checks, startup planning,
//! permission-gated tool call planning, session blacklisting, exposed tool
//! visibility, bounded stdio transport execution for local MCP subprocesses,
//! and streamable HTTP execution through a crate-backed HTTP client.

/// Exposes the audit module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod audit;
/// Exposes the config commands module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod config_commands;
/// Exposes the protocol module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod protocol;
/// Exposes the registry module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod registry;
/// Exposes the stdio module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod stdio;
/// Exposes the streamable http module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod streamable_http;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use audit::{
    McpToolAuditCallContext, call_stdio_mcp_tool_with_audit,
    call_streamable_http_mcp_tool_with_audit,
};
pub use config_commands::{
    McpConfigCommand, McpConfigCommandReport, McpConfigSetting, McpConfigTransport,
    mcp_config_command_display, mcp_config_command_from_words, mcp_config_command_mutations,
    mcp_config_command_report, mcp_config_setting_from_user, persist_mcp_config_command,
    validate_mcp_config_identifier,
};
pub use mez_agent::{McpPromptServer, McpPromptSummary, McpPromptTool, McpPromptUnavailableServer};
pub(crate) use protocol::mcp_tools_call_operation;
pub use protocol::{
    build_mcp_default_initialize_request, build_mcp_initialize_request,
    build_mcp_initialized_notification, build_mcp_tools_call_request, build_mcp_tools_list_request,
    parse_mcp_initialize_response, parse_mcp_tools_call_response, parse_mcp_tools_list_response,
};
pub use registry::McpRegistry;
pub use stdio::{
    McpStdioConnection, discover_stdio_mcp_server, discover_stdio_mcp_server_into_registry,
    spawn_stdio_mcp_connection, spawn_stdio_mcp_connection_with_limit,
};
pub use streamable_http::{
    call_streamable_http_mcp_tool, discover_streamable_http_mcp_server,
    discover_streamable_http_mcp_server_into_registry,
    discover_streamable_http_mcp_server_with_auth_token, execute_streamable_http_exchange,
    initialize_streamable_http_mcp_server,
};
pub use types::{
    DEFAULT_MCP_MAX_MESSAGE_BYTES, DEFAULT_MCP_MAX_TOOL_LIST_PAGES, DEFAULT_MCP_PROTOCOL_VERSION,
    DEFAULT_MCP_STARTUP_TIMEOUT_MS, DEFAULT_MCP_TOOL_TIMEOUT_MS, McpApprovalSetting,
    McpDiscoveredTool, McpEnvironmentPlan, McpExternalCapability, McpInitializeResponse,
    McpServerConfig, McpServerKind, McpServerState, McpServerStatus, McpStartupPlan,
    McpStartupTransportPlan, McpStdioDiscovery, McpStreamableHttpDiscovery,
    McpStreamableHttpResponse, McpToolCallPlan, McpToolCallRequest, McpToolCallResponse,
    McpToolEffects, McpToolListPagination, McpToolState, McpToolsListResponse,
};

impl From<&McpToolCallPlan> for mez_agent::McpExecutionRequest {
    fn from(plan: &McpToolCallPlan) -> Self {
        Self {
            server_id: plan.server_id.clone(),
            tool_name: plan.tool_name.clone(),
            arguments_json: plan.arguments_json.clone(),
            timeout_ms: plan.timeout_ms,
        }
    }
}

impl From<McpToolCallResponse> for mez_agent::McpExecutionResponse {
    fn from(response: McpToolCallResponse) -> Self {
        Self {
            content_json: response.content_json,
            structured_content_json: response.structured_content_json,
            is_error: response.is_error,
        }
    }
}

#[cfg(test)]
use stdio::read_bounded_protocol_line;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
