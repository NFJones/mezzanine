//! MCP initialization counts, discovery filtering, labels, and metadata parsing.

use super::*;

/// Counts enabled MCP servers by startup-readiness state.
pub(super) struct RuntimeMcpInitializationCounts {
    /// Number of configured servers that are enabled.
    pub(super) enabled_servers: usize,
    /// Number of enabled servers ready to execute tool calls.
    pub(super) available_servers: usize,
    /// Number of enabled servers still waiting for discovery.
    pub(super) configured_servers: usize,
    /// Number of enabled servers marked as currently starting.
    pub(super) starting_servers: usize,
    /// Number of enabled servers marked unavailable.
    pub(super) unavailable_status_servers: usize,
    /// Number of enabled servers blacklisted for this session.
    pub(super) blacklisted_servers: usize,
    /// Number of enabled servers marked failed.
    pub(super) failed_servers: usize,
    /// Total tools exposed by enabled available servers.
    pub(super) available_tools: usize,
}

impl RuntimeMcpInitializationCounts {
    /// Builds readiness counts from the live MCP registry.
    pub(super) fn from_registry(registry: &McpRegistry) -> Self {
        let mut counts = Self {
            enabled_servers: 0,
            available_servers: 0,
            configured_servers: 0,
            starting_servers: 0,
            unavailable_status_servers: 0,
            blacklisted_servers: 0,
            failed_servers: 0,
            available_tools: 0,
        };
        for server in registry
            .list_servers()
            .into_iter()
            .filter(|server| server.configured.enabled)
        {
            counts.enabled_servers = counts.enabled_servers.saturating_add(1);
            match server.status {
                McpServerStatus::Configured => {
                    counts.configured_servers = counts.configured_servers.saturating_add(1)
                }
                McpServerStatus::Starting => {
                    counts.starting_servers = counts.starting_servers.saturating_add(1)
                }
                McpServerStatus::Available => {
                    counts.available_servers = counts.available_servers.saturating_add(1);
                    counts.available_tools =
                        counts.available_tools.saturating_add(server.tools.len());
                }
                McpServerStatus::Unavailable => {
                    counts.unavailable_status_servers =
                        counts.unavailable_status_servers.saturating_add(1)
                }
                McpServerStatus::Blacklisted => {
                    counts.blacklisted_servers = counts.blacklisted_servers.saturating_add(1)
                }
                McpServerStatus::Failed => {
                    counts.failed_servers = counts.failed_servers.saturating_add(1)
                }
            }
        }
        counts
    }

    /// Returns enabled servers that are not ready because startup failed.
    pub(super) fn unavailable_servers(&self) -> usize {
        self.unavailable_status_servers
            .saturating_add(self.blacklisted_servers)
            .saturating_add(self.failed_servers)
    }

    /// Returns enabled servers that still have not completed discovery.
    pub(super) fn pending_servers(&self) -> usize {
        self.configured_servers
            .saturating_add(self.starting_servers)
    }
}

/// Returns enabled MCP servers that still need runtime discovery.
pub(super) fn runtime_mcp_pending_discovery_server_ids<F>(
    registry: &McpRegistry,
    has_live_auth_recovery: F,
) -> Vec<String>
where
    F: Fn(&mez_agent::mcp::McpServerState) -> bool,
{
    registry
        .list_servers()
        .into_iter()
        .filter(|server| {
            runtime_mcp_server_needs_runtime_discovery(server, has_live_auth_recovery(server))
        })
        .map(|server| server.configured.id.clone())
        .collect()
}

/// Returns whether one enabled MCP server should re-enter runtime discovery.
pub(super) fn runtime_mcp_server_needs_runtime_discovery(
    server: &mez_agent::mcp::McpServerState,
    has_live_auth_recovery: bool,
) -> bool {
    server.configured.enabled
        && (server.status == McpServerStatus::Configured
            || runtime_mcp_server_needs_live_auth_rediscovery(server, has_live_auth_recovery))
}

/// Returns whether one MCP server should retry live discovery after auth changes.
pub(super) fn runtime_mcp_server_needs_live_auth_rediscovery(
    server: &mez_agent::mcp::McpServerState,
    has_live_auth_recovery: bool,
) -> bool {
    has_live_auth_recovery
        && server.configured.kind == mez_agent::mcp::McpServerKind::Http
        && server.configured.bearer_token_env.is_none()
        && matches!(
            server.status,
            McpServerStatus::Unavailable | McpServerStatus::Blacklisted | McpServerStatus::Failed
        )
}

/// Returns whether a server has stored OAuth state that can recover live discovery.
pub(super) fn runtime_mcp_server_has_live_auth_recovery(
    server: &mez_agent::mcp::McpServerState,
    auth_store: Option<&crate::auth::AuthStore>,
) -> bool {
    auth_store.is_some_and(|store| {
        store.mcp_access_token(&server.configured.id).is_ok()
            || store
                .mcp_refresh_token(&server.configured.id)
                .ok()
                .flatten()
                .is_some()
    })
}

/// Returns how many MCP servers are enabled in the registry.
pub(super) fn runtime_mcp_enabled_server_count(registry: &McpRegistry) -> usize {
    registry
        .list_servers()
        .into_iter()
        .filter(|server| server.configured.enabled)
        .count()
}

/// Returns a human-readable MCP discovery result message for event logs.
pub(super) fn runtime_mcp_discovery_message(
    server_id: &str,
    status: McpServerStatus,
    tools: usize,
    reason: Option<&str>,
) -> String {
    match status {
        McpServerStatus::Available => format!(
            "MCP server {server_id} is ready to field requests with {tools} available {}.",
            runtime_mcp_tool_word(tools)
        ),
        McpServerStatus::Blacklisted => match reason {
            Some(reason) if !reason.trim().is_empty() => {
                format!("MCP server {server_id} is unavailable for this session: {reason}.")
            }
            _ => format!("MCP server {server_id} is unavailable for this session."),
        },
        McpServerStatus::Configured => {
            format!("MCP server {server_id} is configured and waiting for startup.")
        }
        McpServerStatus::Starting => {
            format!("MCP server {server_id} is starting.")
        }
        McpServerStatus::Unavailable | McpServerStatus::Failed => match reason {
            Some(reason) if !reason.trim().is_empty() => {
                format!("MCP server {server_id} is unavailable: {reason}.")
            }
            _ => format!("MCP server {server_id} is unavailable."),
        },
    }
}

/// Returns the readable singular or plural MCP server noun.
pub(super) fn runtime_mcp_server_word(count: usize) -> &'static str {
    if count == 1 { "server" } else { "servers" }
}

/// Returns the readable singular or plural MCP tool noun.
pub(super) fn runtime_mcp_tool_word(count: usize) -> &'static str {
    if count == 1 { "tool" } else { "tools" }
}

/// Returns the normalized MCP status name used in runtime discovery events.
pub(super) fn runtime_mcp_service_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Parses persisted agent shell visibility metadata.
pub(super) fn runtime_agent_session_metadata_visibility(
    value: &str,
) -> Result<AgentShellVisibility> {
    match value {
        "hidden" => Ok(AgentShellVisibility::Hidden),
        "visible" => Ok(AgentShellVisibility::Visible),
        "hide-pending-task-completion" => Ok(AgentShellVisibility::HidePendingTaskCompletion),
        _ => Err(MezError::invalid_args(
            "agent session metadata visibility is invalid",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies lazy runtime discovery retries blacklisted streamable HTTP MCP
    /// servers when stored OAuth state can recover auth. This regression keeps
    /// `/list-mcp` and MCP tool usage live-reloadable after `mez mcp login`
    /// without requiring a Mez restart.
    #[test]
    pub(super) fn runtime_mcp_pending_discovery_retries_blacklisted_http_server_with_auth_recovery()
    {
        let mut registry = McpRegistry::default();
        registry
            .add_server(mez_agent::mcp::McpServerConfig::streamable_http(
                "fixture",
                "fixture",
                "https://example.invalid/mcp",
            ))
            .unwrap();
        registry
            .blacklist_for_session("fixture", "oauth expired", 1)
            .unwrap();

        assert_eq!(
            runtime_mcp_pending_discovery_server_ids(&registry, |server| {
                server.configured.id == "fixture"
            }),
            vec!["fixture".to_string()]
        );
    }

    /// Verifies lazy runtime discovery leaves blacklisted streamable HTTP MCP
    /// servers excluded when no stored OAuth recovery path exists. This avoids
    /// broadly clearing explicit session blacklists for unrelated failures.
    #[test]
    pub(super) fn runtime_mcp_pending_discovery_excludes_blacklisted_http_server_without_auth_recovery()
     {
        let mut registry = McpRegistry::default();
        registry
            .add_server(mez_agent::mcp::McpServerConfig::streamable_http(
                "fixture",
                "fixture",
                "https://example.invalid/mcp",
            ))
            .unwrap();
        registry
            .blacklist_for_session("fixture", "oauth expired", 1)
            .unwrap();

        assert!(runtime_mcp_pending_discovery_server_ids(&registry, |_| false).is_empty());
    }
}
