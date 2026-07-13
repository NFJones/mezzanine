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

#[cfg(test)]
mod tests {
    use super::{McpExecutionRequest, McpExecutionResponse};

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
}
