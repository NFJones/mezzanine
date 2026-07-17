//! Canonical MCP protocol, registry state, and agent-facing projections.
//!
//! This module owns secret-safe configuration policy, server/tool state,
//! availability transitions, bounded pagination, tool-call planning, JSON-RPC
//! construction/parsing, and prompt/display projections. Product adapters own
//! configuration persistence, environment and credential resolution, process
//! and HTTP transports, runtime handles, retry timers, and audit emission.

mod error;
mod prompt;
mod protocol;
mod registry;
mod types;

pub use error::{McpError, McpErrorKind, McpResult};
pub use prompt::{
    AgentShellMcpServerSummary, AgentShellMcpSummary, AgentShellMcpToolSummary,
    McpExecutionRequest, McpExecutionResponse, McpPromptServer, McpPromptSummary, McpPromptTool,
    McpPromptUnavailableServer,
};
pub use protocol::{
    McpJsonRpcOperation, build_mcp_default_initialize_request, build_mcp_initialize_request,
    build_mcp_initialized_notification, build_mcp_tools_call_request, build_mcp_tools_list_request,
    json_id_matches, mcp_initialize_operation, mcp_tools_call_operation, mcp_tools_list_operation,
    object_field, parse_mcp_initialize_response, parse_mcp_json, parse_mcp_tools_call_response,
    parse_mcp_tools_list_response, string_field,
};
pub use registry::McpRegistry;
pub use types::{
    DEFAULT_MCP_MAX_MESSAGE_BYTES, DEFAULT_MCP_MAX_TOOL_LIST_PAGES, DEFAULT_MCP_PROTOCOL_VERSION,
    DEFAULT_MCP_STARTUP_TIMEOUT_MS, DEFAULT_MCP_TOOL_TIMEOUT_MS, McpApprovalSetting,
    McpDiscoveredTool, McpEnvironmentPlan, McpExternalCapability, McpInitializeResponse,
    McpServerConfig, McpServerKind, McpServerState, McpServerStatus, McpStartupPlan,
    McpStartupTransportPlan, McpStdioDiscovery, McpStreamableHttpDiscovery,
    McpStreamableHttpResponse, McpToolCallPlan, McpToolCallRequest, McpToolCallResponse,
    McpToolEffects, McpToolListPagination, McpToolState, McpToolsListResponse,
};

/// Parses and validates one MCP tool argument schema before it becomes callable.
///
/// The schema must be valid JSON with an object root. When the root declares a
/// `type`, it must declare `object`, because MCP tool arguments are JSON
/// objects. Callers use the static diagnostic to keep rejected-tool reporting
/// bounded and secret-safe.
pub(crate) fn validate_mcp_tool_input_schema(
    input_schema_json: &str,
) -> std::result::Result<serde_json::Value, &'static str> {
    let schema = serde_json::from_str::<serde_json::Value>(input_schema_json)
        .map_err(|_| "schema is not valid JSON")?;
    let object = schema
        .as_object()
        .ok_or("schema root is not a JSON object")?;
    if object
        .get("type")
        .is_some_and(|schema_type| schema_type != "object")
    {
        return Err("schema root type is not object");
    }
    Ok(schema)
}

impl From<&McpToolCallPlan> for McpExecutionRequest {
    fn from(plan: &McpToolCallPlan) -> Self {
        Self {
            server_id: plan.server_id.clone(),
            tool_name: plan.tool_name.clone(),
            arguments_json: plan.arguments_json.clone(),
            timeout_ms: plan.timeout_ms,
        }
    }
}

impl From<McpToolCallResponse> for McpExecutionResponse {
    fn from(response: McpToolCallResponse) -> Self {
        Self {
            content_json: response.content_json,
            structured_content_json: response.structured_content_json,
            is_error: response.is_error,
        }
    }
}

#[cfg(test)]
mod tests;
