//! Control State Json implementation.
//!
//! This module owns the control state json boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentShellSession, AgentShellStore, AgentShellVisibility, AgentTurnLedger, AgentTurnState,
    ApprovalDecision, AuditActor, AuditRecord, BlockedApprovalQueue, BlockedApprovalRequest,
    BlockedApprovalState, ClientId, ClientRole, ClientState, EventAudience, EventKind, EventLog,
    FrameContext, FrameOverflow, GrantedRole, JsonRpcRequest, LayoutLoadPlan,
    MAX_EVENT_REPLAY_RETENTION, McpRegistry, McpServerKind, McpServerStatus, MezError,
    ObserverDecisionState, PaneCaptureSource, ProjectTrustRecord, Result, Session, SessionState,
    SnapshotKind, SnapshotState, TrustDecision, VisibleEvent, Window, json_escape, json_raw_field,
    json_string_field, pane_by_id, pane_target_checked_resolved, parse_json_object_value,
    reject_unknown_json_fields, render_frame_template, require_idempotency_key,
    require_session_target_matches_value, resolve_pane_target_value, resolve_window_target_value,
    target_or_active_pane, target_value_has_pane_shape, unix_seconds_to_rfc3339, window_by_id,
};
use crate::agent::slash::{AgentShellCommandOutcome, execute_agent_shell_command};
use crate::terminal::TerminalFrameContext;
use mez_agent::permissions::builtin_rules;
use mez_mux::layout::LayoutNode;
use mez_mux::process::PaneExitStatus;
use mez_mux::session::ClientTerminalDescriptor;
use mez_terminal::DEFAULT_HISTORY_LIMIT;
use mez_terminal::DEFAULT_PANE_TERM;
use std::collections::BTreeMap;

// Control state serialization helpers.

mod agents;
mod approvals;
mod clients;
mod events;
mod mcp;
mod session;
mod snapshots;
mod window_pane;

pub(in crate::control) use agents::{
    agent_shell_command_response_json, agent_state_json, agents_json_for_params,
    dispatch_agent_list_with_store_and_model_profiles, dispatch_agent_shell_command_with_store,
    dispatch_agent_shell_visibility_with_store, dispatch_agent_task_list_with_ledger,
    validate_agent_task_list_params,
};
pub(crate) use approvals::{
    ApprovalDecisionScopePersistence, approval_decide_scope_persistence,
    project_trust_state_filter_from_params,
};
pub(in crate::control) use approvals::{
    approval_audit_record, approval_json, approvals_json_for_params, control_audit_actor,
    parse_approval_decision, parse_trust_decision, project_trust_json,
};
pub(in crate::control) use clients::client_json;
pub(crate) use clients::observers_json;
pub(crate) use events::dispatch_event_list_request;
pub(crate) use mcp::observer_json;
pub(in crate::control) use mcp::{mcp_servers_json, mcp_tools_json};
pub(in crate::control) use session::{
    clients_json_for_params, granted_role_name, observers_json_for_params, panes_json_for_params,
    session_state_json_for_params, session_summary_json, windows_json_for_params,
};
pub(crate) use session::{
    nullable_state_request_session_target_matches, state_request_pane_list_window_ids,
    state_request_session_target_matches,
};
pub(crate) use snapshots::session_state_name;
pub(in crate::control) use snapshots::{
    json_optional_string, resume_plan_json, snapshot_state_json, snapshots_json, string_array_json,
};
pub(in crate::control) use window_pane::{
    frame_read_json, pane_state_json, pane_state_json_with_capture, window_state_json,
};
pub(crate) use window_pane::{frame_read_json_with_context, layout_state_json};
