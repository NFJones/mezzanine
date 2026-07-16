//! Product MCP transport, configuration, credential, and audit adapters.
//!
//! Canonical protocol records, registry transitions, tool-call planning, and
//! prompt projection live in `mez_agent::mcp`. Root retains concrete stdio and
//! HTTP exchange, environment/credential resolution, persisted configuration
//! commands, runtime transport ownership, and audit emission.

#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

/// Exposes the audit module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod audit;
/// Exposes the config commands module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod config_commands;
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

#[cfg(test)]
pub use audit::{
    McpToolAuditCallContext, call_stdio_mcp_tool_with_audit,
    call_streamable_http_mcp_tool_with_audit,
};

pub use config_commands::{
    McpConfigCommand, McpConfigCommandReport, McpConfigTransport, mcp_config_command_display,
    mcp_config_command_from_words, mcp_config_command_mutations, mcp_config_command_report,
    mcp_config_setting_from_user, persist_mcp_config_command,
};
pub use stdio::{McpStdioConnection, spawn_stdio_mcp_connection};
#[cfg(test)]
pub use stdio::{discover_stdio_mcp_server, discover_stdio_mcp_server_into_registry};
#[cfg(test)]
pub use streamable_http::{
    call_streamable_http_mcp_tool, discover_streamable_http_mcp_server_into_registry,
    initialize_streamable_http_mcp_server,
};
pub use streamable_http::{
    discover_streamable_http_mcp_server_with_auth_token, execute_streamable_http_exchange,
};

/// Returns the product clock value supplied to deterministic MCP transitions.
#[cfg(test)]
pub(crate) fn current_mcp_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
#[cfg(test)]
use stdio::read_bounded_protocol_line;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
