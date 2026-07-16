//! Control Dispatch implementation.
//!
//! This module owns the control dispatch boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::registry::{ControlDispatchKind, ControlParamsSchema, control_method_spec};
use super::snapshot::require_session_target_matches;
use super::{
    AgentShellStore, AgentTurnLedger, AuditLog, AuthenticationMaterial, AuthenticationMechanism,
    BlockedApprovalQueue, Capabilities, ClientId, ClientRole, ConfigLayer, ControlIdempotencyCache,
    EventLog, GrantedRole, InitializeContext, InitializeResult, JsonRpcRequest,
    MAX_EVENT_REPLAY_RETENTION, McpRegistry, MezError, ObserverRequestSummary, PaneCaptureSource,
    PaneSizeSpec, RequestedRole, ResizeAxis, ResizeDirection, Result, ServerIdentity, Session,
    SnapshotRepository, SplitDirection, TerminalDescriptor, agent_shell_command_response_json,
    agent_state_json, agents_json_for_params, approval_audit_record,
    approval_decide_scope_persistence, approval_json, approvals_json_for_params,
    authorize_control_request, client_descriptor_from_json, client_json, clients_json_for_params,
    config_audit_outcome, config_audit_plan, config_request_cache_key,
    config_response_advances_generation, decode_control_frame, destination_target_checked_resolved,
    dispatch_agent_list_with_store_and_model_profiles, dispatch_agent_shell_command_with_store,
    dispatch_agent_shell_visibility_with_store, dispatch_agent_task_list_with_ledger,
    dispatch_config_parsed_request, dispatch_config_parsed_to_response_cached,
    dispatch_event_list_request, dispatch_pane_capture_request, dispatch_snapshot_request,
    dispatch_snapshot_request_with_context, encode_control_body,
    ensure_client_descriptor_role_matches, error_code, frame_read_json, initialize,
    initialize_params_from_json, initialize_result_json, is_config_control_method, json_bool_field,
    json_escape, json_null_field, json_object_field, json_raw_field, json_rpc_error,
    json_rpc_success, json_string_field, layout_state_json, mcp_servers_json, mcp_tools_json,
    mezzanine_error_code, negotiate_protocol_version,
    nullable_state_request_session_target_matches, observer_json, observers_json_for_params,
    pane_by_id, pane_state_json, pane_target_checked_resolved, panes_json_for_params,
    parse_approval_decision, parse_join_position, parse_json_object_value, parse_json_rpc_request,
    parse_split_direction, parse_trust_decision, project_trust_state_filter_from_params,
    reject_unknown_json_fields, require_idempotency_key, require_session_target_matches_value,
    session_state_json_for_params, session_summary_json, source_pane_target_checked_resolved,
    target_or_active_pane, validate_agent_task_list_params, validate_config_control_params_schema,
    window_by_id, window_id_for_target, window_state_json, window_target_checked_resolved,
    windows_json_for_params,
};
// Control dispatch entry points and dispatch internals.

mod connection;
mod entry;
mod method_dispatch;
mod schema_validation;

pub use connection::{
    ControlConnectionState, dispatch_control_request_cached,
    dispatch_control_request_for_connection, handle_control_frames_for_connection,
};
pub use entry::{
    dispatch_control_request, dispatch_control_request_for_client,
    dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_agent_state_and_model_profiles,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_events,
    dispatch_control_request_for_client_with_snapshot_captures,
    dispatch_control_request_for_client_with_snapshot_captures_and_config_layers,
    dispatch_control_request_for_client_with_snapshot_captures_config_layers_and_frame_state,
    dispatch_control_request_for_client_with_snapshot_context,
    dispatch_control_request_for_client_with_snapshots, dispatch_control_request_with_approvals,
    dispatch_control_request_with_approvals_and_audit, dispatch_control_request_with_captures,
    dispatch_control_request_with_mcp, dispatch_control_request_with_snapshots,
};
pub(crate) use schema_validation::validate_control_method_params_schema;
