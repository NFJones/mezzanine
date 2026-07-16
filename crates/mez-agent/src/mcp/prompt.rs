//! Dependency-neutral MCP prompt manifest contracts.
//!
//! These records describe the bounded, secret-safe MCP server and tool
//! metadata exposed to model context and provider schemas. MCP discovery,
//! transport configuration, credentials, and tool execution remain product
//! responsibilities.

/// One MCP tool exposed to the agent prompt and provider schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptTool {
    /// Stable identifier of the server that owns the tool.
    pub server_id: String,
    /// Tool name advertised by the server.
    pub tool_name: String,
    /// Model-visible tool description.
    pub description: String,
    /// Whether product policy requires approval before execution.
    pub approval_required: bool,
    /// JSON Schema describing accepted tool arguments.
    pub input_schema_json: String,
}

/// One available MCP server summarized for agent context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptServer {
    /// Stable server identifier.
    pub server_id: String,
    /// Human-readable server name.
    pub display_name: String,
    /// Concise model-visible purpose.
    pub purpose: String,
    /// User-authored model usage guidance.
    pub usage_instructions: String,
    /// Number of tools exposed by the server.
    pub tool_count: usize,
    /// Number of exposed tools that require approval.
    pub approval_required_tool_count: usize,
}

/// One configured MCP server unavailable to the current agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptUnavailableServer {
    /// Stable server identifier.
    pub server_id: String,
    /// Concise model-visible purpose.
    pub purpose: String,
    /// User-authored model usage guidance.
    pub usage_instructions: String,
    /// Model-safe explanation of why the server is unavailable.
    pub reason: String,
    /// Whether a later attempt may make the server available.
    pub retryable: bool,
}

/// Bounded MCP availability summary supplied to the agent harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptSummary {
    /// Available servers visible to the current turn.
    pub available_servers: Vec<McpPromptServer>,
    /// Available tools visible to the current turn.
    pub available_tools: Vec<McpPromptTool>,
    /// Configured servers that could not be made available.
    pub unavailable_servers: Vec<McpPromptUnavailableServer>,
}

/// Dependency-neutral request for one approved MCP tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpExecutionRequest {
    /// Stable identifier of the server that owns the tool.
    pub server_id: String,
    /// Tool name advertised by the server.
    pub tool_name: String,
    /// Model-authored tool arguments encoded as a JSON object.
    pub arguments_json: String,
    /// Product-selected execution timeout in milliseconds.
    pub timeout_ms: u64,
}

/// Dependency-neutral response from one MCP tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpExecutionResponse {
    /// MCP content blocks encoded as JSON.
    pub content_json: String,
    /// Optional MCP structured content encoded as JSON.
    pub structured_content_json: Option<String>,
    /// Whether the MCP server reported a tool-level error.
    pub is_error: bool,
}

/// Bounded live MCP state shown by the agent shell.
///
/// Product discovery, configuration, credentials, approval enforcement,
/// transport ownership, and execution remain outside the agent harness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentShellMcpSummary {
    /// Configured servers in deterministic product-registry order.
    pub servers: Vec<AgentShellMcpServerSummary>,
}

/// User-visible state for one configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentShellMcpServerSummary {
    /// Stable configured server identifier.
    pub server_id: String,
    /// Human-readable configured server name.
    pub display_name: String,
    /// Effective state after applying configuration and runtime status.
    pub state: String,
    /// Raw normalized runtime status.
    pub status: String,
    /// Whether the server is enabled by configuration.
    pub enabled: bool,
    /// Normalized transport name.
    pub transport: String,
    /// Whether configuration or runtime state marks the server unavailable.
    pub blacklisted: bool,
    /// Whether runtime state blacklisted the server for this session.
    pub session_blacklisted: bool,
    /// Whether retrying startup may restore availability.
    pub retryable: bool,
    /// Optional model-safe unavailability reason.
    pub reason: Option<String>,
    /// Tools discovered for this server.
    pub tools: Vec<AgentShellMcpToolSummary>,
}

/// User-visible state for one discovered MCP tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentShellMcpToolSummary {
    /// Tool name advertised by the server.
    pub name: String,
    /// Effective availability state after configuration and runtime policy.
    pub state: String,
    /// Normalized approval setting.
    pub approval: String,
    /// Whether product policy requires permission enforcement.
    pub permission_required: bool,
    /// Compact comma-separated external-effect summary.
    pub effects: String,
    /// Model-safe tool description.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::{
        AgentShellMcpServerSummary, AgentShellMcpSummary, AgentShellMcpToolSummary,
        McpExecutionRequest, McpExecutionResponse,
    };

    #[test]
    /// Verifies MCP execution contracts preserve only the approved request and
    /// normalized response fields needed by the agent harness.
    fn mcp_execution_contracts_preserve_agent_visible_fields() {
        let request = McpExecutionRequest {
            server_id: "filesystem".to_string(),
            tool_name: "read_file".to_string(),
            arguments_json: r#"{"path":"README.md"}"#.to_string(),
            timeout_ms: 30_000,
        };
        let response = McpExecutionResponse {
            content_json: r#"[{"type":"text","text":"ok"}]"#.to_string(),
            structured_content_json: Some(r#"{"bytes":2}"#.to_string()),
            is_error: false,
        };

        assert_eq!(request.server_id, "filesystem");
        assert_eq!(request.timeout_ms, 30_000);
        assert_eq!(
            response.structured_content_json.as_deref(),
            Some(r#"{"bytes":2}"#)
        );
        assert!(!response.is_error);
    }

    #[test]
    /// Verifies agent-shell MCP summaries preserve bounded display fields
    /// without exposing registry, transport, credential, or approval owners.
    fn agent_shell_mcp_summary_preserves_display_fields() {
        let summary = AgentShellMcpSummary {
            servers: vec![AgentShellMcpServerSummary {
                server_id: "filesystem".to_string(),
                display_name: "Filesystem".to_string(),
                state: "available".to_string(),
                status: "available".to_string(),
                enabled: true,
                transport: "stdio".to_string(),
                blacklisted: false,
                session_blacklisted: false,
                retryable: false,
                reason: None,
                tools: vec![AgentShellMcpToolSummary {
                    name: "read_file".to_string(),
                    state: "available".to_string(),
                    approval: "inherit".to_string(),
                    permission_required: true,
                    effects: "read-fs".to_string(),
                    description: "Read a file".to_string(),
                }],
            }],
        };

        assert_eq!(summary.servers[0].server_id, "filesystem");
        assert_eq!(summary.servers[0].tools[0].effects, "read-fs");
    }
}
