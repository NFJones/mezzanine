//! Intrinsic tests for the canonical MCP policy and protocol boundary.

mod protocol;
mod registry;

use super::{McpApprovalSetting, McpServerConfig, McpToolEffects, McpToolState};

/// Builds the secret-safe stdio server fixture shared by registry tests.
fn config() -> McpServerConfig {
    McpServerConfig::stdio("fs", "filesystem", "mcp-fs", Vec::new())
}

/// Builds one risky filesystem tool fixture shared by registry tests.
fn tool() -> McpToolState {
    McpToolState {
        server_id: String::new(),
        name: "read_file".to_string(),
        available: false,
        blacklisted: false,
        permission_required: true,
        effects: McpToolEffects {
            reads_filesystem: true,
            ..McpToolEffects::none()
        },
        approval: McpApprovalSetting::Inherit,
        description: "Read a file".to_string(),
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }
}
