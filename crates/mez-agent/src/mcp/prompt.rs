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

/// Code-owned model-facing diagnostic state, never arbitrary transport text.
/// Raw startup/protocol errors remain in operator-owned registry evidence;
/// model context receives only these bounded reasons and recovery categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerDiagnostic {
    /// A complete live discovery is available.
    Available,
    /// Initial discovery has not run.
    Configured,
    /// Discovery is currently in progress.
    Starting,
    /// Operator configuration disabled the server.
    Disabled,
    /// The transport is not currently available.
    Unavailable,
    /// Startup or protocol processing failed.
    Failed,
    /// This session requires explicit recovery before reuse.
    Blacklisted,
    /// A requested name is not configured.
    Unknown,
    /// A requested name needs exact casing to resolve.
    Ambiguous,
}

impl McpServerDiagnostic {
    /// Returns a bounded explanation derived solely from typed state.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Configured => "runtime discovery pending",
            Self::Starting => "runtime discovery in progress",
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
            Self::Failed => "startup or protocol failure",
            Self::Blacklisted => "blacklisted for this session",
            Self::Unknown => "configured MCP server name did not match a configured server",
            Self::Ambiguous => {
                "configured MCP server name is ambiguous; use the exact configured identifier casing"
            }
        }
    }

    /// Returns actionable guidance without exposing error bodies or credentials.
    pub fn recovery(self) -> &'static str {
        match self {
            Self::Available => "none",
            Self::Configured => "inspect_with_list_mcp",
            Self::Starting => "wait_for_discovery",
            Self::Disabled => "enable_server_explicitly",
            Self::Unavailable | Self::Failed | Self::Blacklisted => {
                "inspect_operator_diagnostics_then_explicit_retry"
            }
            Self::Unknown | Self::Ambiguous => "use_exact_configured_id",
        }
    }
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
    /// Typed state whose reason/recovery projection cannot reflect raw errors.
    pub diagnostic: McpServerDiagnostic,
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
    /// Bounded identity of the tool schema generation the arguments were validated against.
    ///
    /// Product execution identity compares this value with the generation bound
    /// at approval time before any transport is invoked, so a schema refresh
    /// cannot reuse an approval that covered different metadata.
    pub schema_generation: String,
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
    /// Model-safe configured server purpose.
    pub purpose: String,
    /// Model-safe configured server usage guidance.
    pub usage_instructions: String,
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
    /// Validated JSON Schema accepted by the tool's object arguments.
    pub input_schema_json: String,
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
            schema_generation: crate::McpSchemaGeneration::derive(
                "filesystem",
                "read_file",
                r#"{"type":"object"}"#,
            )
            .into_string(),
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
                purpose: "Read project files".to_string(),
                usage_instructions: "Use read_file for one path.".to_string(),
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
                    input_schema_json:
                        r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.to_string(),
                }],
            }],
        };

        assert_eq!(summary.servers[0].server_id, "filesystem");
        assert_eq!(summary.servers[0].tools[0].effects, "read-fs");
    }
}
