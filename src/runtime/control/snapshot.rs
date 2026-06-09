//! Runtime control snapshot metadata serialization helpers.
//!
//! Snapshot capture code in the runtime control adapter needs to translate live
//! runtime state into durable snapshot metadata records. This module owns those
//! small conversion helpers so the main control dispatcher does not also own
//! every snapshot enum/string mapping.

use super::super::{
    AgentShellVisibility, ApprovalDecision, ApprovalGrant, ApprovalScope, BlockedApprovalRequest,
    BlockedApprovalState, ConfigScope, McpApprovalSetting, McpExternalCapability, McpServerKind,
    McpServerStatus, McpToolEffects, McpToolState, SnapshotApprovalGrantMetadata,
    SnapshotApprovalRequestMetadata, SnapshotMcpExternalCapability, SnapshotMcpServerState,
    SnapshotMcpToolEffects, SnapshotMcpToolState, TerminalFramePosition, TerminalFrameStyle,
};

/// Returns the durable snapshot name for one runtime config layer scope.
pub(super) fn runtime_snapshot_config_scope_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Primary => "primary",
        ConfigScope::ProjectOverlay => "project-overlay",
        ConfigScope::LiveOverride => "live-override",
    }
}

/// Returns the durable snapshot name for one terminal frame position.
pub(super) fn runtime_snapshot_frame_position_name(
    position: TerminalFramePosition,
) -> &'static str {
    match position {
        TerminalFramePosition::Top => "top",
        TerminalFramePosition::Bottom => "bottom",
    }
}

/// Returns the durable snapshot name for one terminal frame style.
pub(super) fn runtime_snapshot_frame_style_name(style: TerminalFrameStyle) -> &'static str {
    match style {
        TerminalFrameStyle::Default => "default",
        TerminalFrameStyle::Bold => "bold",
        TerminalFrameStyle::Underline => "underline",
        TerminalFrameStyle::Inverse => "inverse",
    }
}

/// Converts one live approval grant into snapshot metadata.
pub(super) fn runtime_snapshot_approval_grant(
    grant: &ApprovalGrant,
) -> SnapshotApprovalGrantMetadata {
    SnapshotApprovalGrantMetadata {
        id: grant.id.clone(),
        command_prefix: grant.command_prefix.clone(),
        scope: runtime_snapshot_approval_scope_name(grant.scope).to_string(),
        decision: runtime_snapshot_approval_decision_name(grant.decision).to_string(),
    }
}

/// Converts one blocked approval request into snapshot metadata.
pub(super) fn runtime_snapshot_approval_request(
    request: &BlockedApprovalRequest,
) -> SnapshotApprovalRequestMetadata {
    SnapshotApprovalRequestMetadata {
        id: request.id.clone(),
        requesting_agent_id: request.requesting_agent_id.clone(),
        pane_id: request.pane_id.clone(),
        parent_agent_chain: request.parent_agent_chain.clone(),
        action_kind: request.action_kind.clone(),
        action_summary: request.action_summary.clone(),
        declared_effects: request.declared_effects.clone(),
        matched_rules: request.matched_rules.clone(),
        read_scopes: request.read_scopes.clone(),
        write_scopes: request.write_scopes.clone(),
        created_at_unix_seconds: request.created_at_unix_seconds,
        decided_at_unix_seconds: request.decided_at_unix_seconds,
        decided_by_client_id: request.decided_by_client_id.clone(),
        state: runtime_snapshot_blocked_approval_state_name(request.state).to_string(),
        decision: request
            .decision
            .map(runtime_snapshot_approval_decision_name)
            .map(ToOwned::to_owned),
        redirect_instruction: request.redirect_instruction.clone(),
    }
}

/// Converts one live MCP server state into snapshot metadata.
pub(super) fn runtime_snapshot_mcp_server_state(
    server: &crate::mcp::McpServerState,
) -> SnapshotMcpServerState {
    SnapshotMcpServerState {
        id: server.configured.id.clone(),
        name: server.configured.name.clone(),
        kind: runtime_snapshot_mcp_kind_name(server.configured.kind).to_string(),
        enabled: server.configured.enabled,
        status: runtime_snapshot_mcp_status_name(server.status).to_string(),
        last_checked_at_unix_seconds: server.last_checked_at_unix_seconds,
        blacklist_reason: server.blacklist_reason.clone(),
        external_capability: runtime_snapshot_mcp_external_capability(
            &server.configured.external_capability,
        ),
        tools: server.tools.iter().map(runtime_snapshot_mcp_tool).collect(),
    }
}

/// Converts one live MCP external capability declaration into snapshot metadata.
fn runtime_snapshot_mcp_external_capability(
    capability: &McpExternalCapability,
) -> SnapshotMcpExternalCapability {
    SnapshotMcpExternalCapability {
        mutates_filesystem_outside_shell: capability.mutates_filesystem_outside_shell,
        executes_processes_outside_shell: capability.executes_processes_outside_shell,
        accesses_credentials_outside_shell: capability.accesses_credentials_outside_shell,
        purpose: capability.purpose.clone(),
    }
}

/// Converts one live MCP tool state into snapshot metadata.
fn runtime_snapshot_mcp_tool(tool: &McpToolState) -> SnapshotMcpToolState {
    SnapshotMcpToolState {
        server_id: tool.server_id.clone(),
        name: tool.name.clone(),
        available: tool.available,
        blacklisted: tool.blacklisted,
        permission_required: tool.permission_required,
        effects: runtime_snapshot_mcp_effects(tool.effects),
        approval: runtime_snapshot_mcp_approval_name(tool.approval).to_string(),
        description: tool.description.clone(),
        input_schema_json: tool.input_schema_json.clone(),
    }
}

/// Converts live MCP tool effects into snapshot metadata.
fn runtime_snapshot_mcp_effects(effects: McpToolEffects) -> SnapshotMcpToolEffects {
    SnapshotMcpToolEffects {
        reads_filesystem: effects.reads_filesystem,
        mutates_filesystem: effects.mutates_filesystem,
        executes_processes: effects.executes_processes,
        accesses_credentials: effects.accesses_credentials,
        uses_network: effects.uses_network,
        has_side_effects: effects.has_side_effects,
    }
}

/// Returns the durable snapshot name for one MCP server transport kind.
fn runtime_snapshot_mcp_kind_name(kind: McpServerKind) -> &'static str {
    match kind {
        McpServerKind::Stdio => "stdio",
        McpServerKind::Http => "streamable_http",
    }
}

/// Returns the durable snapshot name for one MCP server status.
fn runtime_snapshot_mcp_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Returns the durable snapshot name for one MCP tool approval setting.
fn runtime_snapshot_mcp_approval_name(approval: McpApprovalSetting) -> &'static str {
    match approval {
        McpApprovalSetting::Inherit => "inherit",
        McpApprovalSetting::Prompt => "prompt",
        McpApprovalSetting::Allow => "allow",
        McpApprovalSetting::Deny => "deny",
    }
}

/// Returns the durable snapshot name for one approval grant scope.
fn runtime_snapshot_approval_scope_name(scope: ApprovalScope) -> &'static str {
    match scope {
        ApprovalScope::Session => "session",
        ApprovalScope::Global => "global",
    }
}

/// Returns the durable snapshot name for one approval decision.
fn runtime_snapshot_approval_decision_name(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Approve => "approve",
        ApprovalDecision::Disapprove => "disapprove",
        ApprovalDecision::Redirect => "redirect",
    }
}

/// Returns the durable snapshot name for one blocked approval request state.
fn runtime_snapshot_blocked_approval_state_name(state: BlockedApprovalState) -> &'static str {
    match state {
        BlockedApprovalState::Pending => "pending",
        BlockedApprovalState::Approved => "approved",
        BlockedApprovalState::Disapproved => "disapproved",
        BlockedApprovalState::Redirected => "redirected",
    }
}

/// Returns the durable snapshot name for one agent shell visibility state.
pub(super) fn runtime_snapshot_agent_visibility_name(
    visibility: AgentShellVisibility,
) -> &'static str {
    match visibility {
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}
