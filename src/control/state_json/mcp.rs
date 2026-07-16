//! Event-kind, observer, and MCP server/tool state serialization.

use super::approvals::optional_rfc3339_timestamp_json;
use super::clients::generic_client_terminal_descriptor_json;
use super::snapshots::observer_state_name;
use super::*;

/// Runs the event kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn event_kind_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::ClientAttached => "client_attached",
        EventKind::ClientDetached => "client_detached",
        EventKind::ObserverRequested => "observer_requested",
        EventKind::ObserverDecided => "observer_decided",
        EventKind::WindowChanged => "window_changed",
        EventKind::PaneChanged => "pane_changed",
        EventKind::AgentStatus => "agent_status",
        EventKind::Message => "message",
        EventKind::ConfigChanged => "config_changed",
        EventKind::SnapshotChanged => "snapshot_changed",
        EventKind::ApprovalChanged => "approval_changed",
        EventKind::McpServerChanged => "mcp_server_changed",
        EventKind::HookFailed => "hook_failed",
        EventKind::Diagnostic => "diagnostic",
    }
}

/// Runs the observer json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn observer_json(session: &Session, observer_id: &str) -> Result<String> {
    let observer = session
        .observers()
        .iter()
        .find(|observer| observer.id.as_str() == observer_id)
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "observer not found"))?;
    Ok(observer_json_by_ref(observer))
}

/// Runs the observer json by ref operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn observer_json_by_ref(
    observer: &mez_mux::session::ObserverRequest,
) -> String {
    let visible_from_event_id = observer
        .visible_from_event_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "null".to_string());
    let visible_from_time = optional_rfc3339_timestamp_json(observer.visible_from_unix_seconds);
    format!(
        r#"{{"id":"{}","version":1,"observer_request_id":"{}","client_id":"{}","state":"{}","requested_at":{},"decided_at":{},"decided_by_client_id":{},"visible_from_event_id":{},"visible_from_time":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"reason":{}}}"#,
        json_escape(&observer.id.to_string()),
        json_escape(&observer.id.to_string()),
        json_escape(&observer.client_id.to_string()),
        observer_state_name(observer.state),
        optional_rfc3339_timestamp_json(observer.requested_at_unix_seconds),
        optional_rfc3339_timestamp_json(observer.decided_at_unix_seconds),
        json_optional_string(observer.decided_by_client_id.as_deref()),
        visible_from_event_id,
        visible_from_time,
        json_escape(&observer.descriptor_name),
        observer.descriptor_interactive,
        generic_client_terminal_descriptor_json(observer.descriptor_terminal.as_ref()),
        json_optional_string(observer.reason.as_deref())
    )
}

/// Runs the mcp servers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_servers_json(registry: &McpRegistry) -> String {
    let servers = registry
        .list_servers()
        .iter()
        .map(|server| {
            let blacklisted = matches!(server.status, McpServerStatus::Blacklisted)
                || server.blacklist_reason.is_some();
            format!(
                r#"{{"id":"{}","version":1,"server_id":"{}","name":"{}","state":"{}","status":"{}","configured":true,"blacklisted":{},"transport":{{"kind":"{}"}},"kind":"{}","tools":{},"last_checked_at":{},"diagnostics":{},"enabled":{},"blacklist_reason":{},"retryable":{},"external_purpose":"{}"}}"#,
                json_escape(&server.configured.id),
                json_escape(&server.configured.id),
                json_escape(&server.configured.name),
                mcp_server_state_name(server),
                mcp_status_name(server.status),
                blacklisted,
                mcp_kind_name(server.configured.kind),
                mcp_kind_name(server.configured.kind),
                mcp_server_tool_ids_json(server),
                optional_rfc3339_timestamp_json(server.last_checked_at_unix_seconds),
                mcp_server_diagnostics_json(server),
                server.configured.enabled,
                json_optional_string(server.blacklist_reason.as_deref()),
                server.configured.enabled
                    && matches!(
                        server.status,
                        McpServerStatus::Unavailable
                            | McpServerStatus::Blacklisted
                            | McpServerStatus::Failed
                    ),
                json_escape(&server.configured.external_capability.purpose)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", servers.join(","))
}

/// Runs the mcp tools json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_tools_json(registry: &McpRegistry) -> String {
    let tools = registry
        .list_servers()
        .iter()
        .flat_map(|server| {
            server.tools.iter().map(|tool| {
                format!(
                    r#"{{"id":"{}","version":1,"server_id":"{}","name":"{}","available":{},"blacklisted":{},"permission_required":{},"effects":{},"description":"{}","input_schema":{},"approval":"{}"}}"#,
                    json_escape(&mcp_tool_id(tool)),
                    json_escape(&tool.server_id),
                    json_escape(&tool.name),
                    tool.available,
                    tool.blacklisted,
                    tool.permission_required,
                    mcp_tool_effects_json(tool.effects),
                    json_escape(&tool.description),
                    tool.input_schema_json,
                    mcp_approval_name(tool.approval)
                )
            })
        })
        .collect::<Vec<_>>();
    format!("[{}]", tools.join(","))
}

/// Runs the mcp tool effects json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_tool_effects_json(effects: mez_agent::mcp::McpToolEffects) -> String {
    format!(
        r#"{{"reads_filesystem":{},"mutates_filesystem":{},"executes_processes":{},"accesses_credentials":{},"uses_network":{},"has_side_effects":{}}}"#,
        effects.reads_filesystem,
        effects.mutates_filesystem,
        effects.executes_processes,
        effects.accesses_credentials,
        effects.uses_network,
        effects.has_side_effects
    )
}

/// Runs the mcp server tool ids json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_server_tool_ids_json(
    server: &mez_agent::mcp::McpServerState,
) -> String {
    let ids = server
        .tools
        .iter()
        .map(|tool| format!(r#""{}""#, json_escape(&mcp_tool_id(tool))))
        .collect::<Vec<_>>();
    format!("[{}]", ids.join(","))
}

/// Runs the mcp tool id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_tool_id(tool: &mez_agent::mcp::McpToolState) -> String {
    format!("{}:{}", tool.server_id, tool.name)
}

/// Runs the mcp server diagnostics json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_server_diagnostics_json(
    server: &mez_agent::mcp::McpServerState,
) -> String {
    match server.blacklist_reason.as_deref() {
        Some(reason) => format!(
            r#"[{{"severity":"warning","code":"mcp_blacklisted","message":"{}"}}]"#,
            json_escape(reason)
        ),
        None => "[]".to_string(),
    }
}

/// Runs the mcp kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_kind_name(kind: McpServerKind) -> &'static str {
    match kind {
        McpServerKind::Stdio => "stdio",
        McpServerKind::Http => "streamable_http",
    }
}

/// Runs the mcp server state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_server_state_name(
    server: &mez_agent::mcp::McpServerState,
) -> &'static str {
    if !server.configured.enabled {
        return "disabled";
    }
    match server.status {
        McpServerStatus::Configured => "enabled",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Runs the mcp status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Runs the mcp approval name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn mcp_approval_name(
    approval: mez_agent::mcp::McpApprovalSetting,
) -> &'static str {
    match approval {
        mez_agent::mcp::McpApprovalSetting::Inherit => "inherit",
        mez_agent::mcp::McpApprovalSetting::Prompt => "prompt",
        mez_agent::mcp::McpApprovalSetting::Allow => "allow",
        mez_agent::mcp::McpApprovalSetting::Deny => "deny",
    }
}
