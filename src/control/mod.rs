//! Control endpoint schemas and initialization logic.
//!
//! This module contains typed request/response state, role gating,
//! authentication-material handling, framing helpers, and JSON-RPC dispatch
//! used by the runtime control socket server.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::process::geteuid;

use crate::audit::{AuditActor, AuditLog, AuditRecord};
use crate::config::{
    ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigMutationPlan, ConfigMutationValue, ConfigScope, ConfigValidation,
    compose_effective_config, persist_config_mutation, validate_config_file,
};
use crate::error::{MezError, Result};
use crate::event::{EventAudience, EventKind, EventLog, VisibleEvent};
use crate::framing::{
    FrameContext, FrameOverflow, ProtocolFrame, decode_frame, encode_frame, render_frame_template,
};
use crate::project::{ProjectTrustRecord, ProjectTrustStore, TrustDecision};
use crate::snapshot::{LayoutLoadPlan, SnapshotKind, SnapshotRepository, SnapshotState};
use mez_agent::mcp::{McpRegistry, McpServerKind, McpServerStatus};
use mez_agent::permissions::{
    ApprovalDecision, BlockedApprovalQueue, BlockedApprovalRequest, BlockedApprovalState,
};
use mez_agent::{
    AgentShellSession, AgentShellStore, AgentShellVisibility, AgentTurnLedger, AgentTurnState,
};
use mez_core::ids::ClientId;
use mez_mux::layout::{PaneSizeSpec, ResizeAxis, ResizeDirection, SplitDirection, Window};
use mez_mux::session::{ClientRole, ClientState, ObserverDecisionState, Session, SessionState};

/// Exposes the authz module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod authz;
/// Exposes the capture module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod capture;
/// Exposes the config module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod config;
/// Exposes the dispatch module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod dispatch;
/// Exposes the framing module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod framing;
/// Exposes the idempotency module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod idempotency;
/// Exposes the initialize module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod initialize;
/// Exposes the json module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod json;
/// Exposes the registry module boundary.
///
/// The nested module keeps method metadata in one place so dispatch,
/// validation, and future capability views do not duplicate method strings.
mod registry;
/// Exposes the snapshot module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod snapshot;
/// Exposes the state json module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod state_json;
/// Exposes the targets module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod targets;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use authz::authorize_control_request;
pub use config::{
    dispatch_config_request, dispatch_config_request_cached, dispatch_project_trust_request,
    dispatch_session_attach_request,
};
pub(crate) use dispatch::validate_control_method_params_schema;
pub use dispatch::{
    ControlConnectionState, dispatch_control_request, dispatch_control_request_cached,
    dispatch_control_request_for_client, dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_agent_state_and_model_profiles,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_events,
    dispatch_control_request_for_client_with_snapshot_captures,
    dispatch_control_request_for_client_with_snapshot_captures_and_config_layers,
    dispatch_control_request_for_client_with_snapshot_captures_config_layers_and_frame_state,
    dispatch_control_request_for_client_with_snapshot_context,
    dispatch_control_request_for_client_with_snapshots, dispatch_control_request_for_connection,
    dispatch_control_request_with_approvals, dispatch_control_request_with_approvals_and_audit,
    dispatch_control_request_with_captures, dispatch_control_request_with_mcp,
    dispatch_control_request_with_snapshots, handle_control_frames_for_connection,
};
pub use framing::{
    decode_control_frame, encode_control_body, handle_control_frame, handle_control_frames,
};
pub use idempotency::{
    CachedControlResponse, ControlIdempotencyCache, JsonRpcRequest, parse_json_rpc_request,
};
pub use initialize::initialize;
pub(crate) use snapshot::dispatch_snapshot_request_with_context_async;
pub(crate) use targets::{
    destination_target_checked_resolved, pane_target_checked_resolved,
    require_session_target_matches_value, source_pane_target_checked_resolved,
    window_target_checked_resolved,
};
pub(crate) use types::{
    AGENT_CONTROL_METHODS, AUTOMATION_CONTROL_METHODS, OBSERVER_CONTROL_METHODS,
    PENDING_OBSERVER_CONTROL_METHODS,
};
pub use types::{
    AuthenticationMaterial, AuthenticationMechanism, CONTROL_CONTENT_TYPE, Capabilities,
    CapabilityFeatures, CapabilityLimits, ClientDescriptor, ClientStdioDescriptor, GrantedRole,
    InitializeContext, InitializeParams, InitializeResult, MAX_EVENT_REPLAY_RETENTION,
    ObserverRequestSummary, PaneCaptureSource, RequestedRole, ServerIdentity, TerminalDescriptor,
};

use authz::require_idempotency_key;
use capture::dispatch_pane_capture_request;
pub(crate) use config::{
    ControlPersistTarget, config_audit_outcome, config_audit_plan,
    config_mutation_plan_result_json, config_mutation_value_from_json, config_request_cache_key,
    config_response_advances_generation, dispatch_config_parsed_request,
    dispatch_config_parsed_to_response_cached, is_config_control_method, persist_target_from_json,
    validate_config_control_params_schema,
};
use initialize::{
    client_descriptor_from_json, ensure_client_descriptor_role_matches,
    initialize_params_from_json, initialize_result_json, negotiate_protocol_version,
};
pub(crate) use json::unix_seconds_to_rfc3339;
use json::{
    current_rfc3339_seconds, effective_uid, error_code, field_value, json_bool_field, json_escape,
    json_null_field, json_object_field, json_raw_field, json_rpc_error, json_rpc_success,
    json_string_array_field, json_string_field, mezzanine_error_code, reject_unknown_json_fields,
};
pub(crate) use snapshot::snapshot_id_for_idempotency_key;
use snapshot::{dispatch_snapshot_request, dispatch_snapshot_request_with_context};
pub(crate) use state_json::{ApprovalDecisionScopePersistence, approval_decide_scope_persistence};
use state_json::{
    agent_shell_command_response_json, agent_state_json, agents_json_for_params,
    approval_audit_record, approval_json, approvals_json_for_params, client_json,
    clients_json_for_params, control_audit_actor,
    dispatch_agent_list_with_store_and_model_profiles, dispatch_agent_shell_command_with_store,
    dispatch_agent_shell_visibility_with_store, dispatch_agent_task_list_with_ledger,
    frame_read_json, granted_role_name, json_optional_string, mcp_servers_json, mcp_tools_json,
    observers_json_for_params, pane_state_json, pane_state_json_with_capture,
    panes_json_for_params, parse_approval_decision, parse_trust_decision, project_trust_json,
    resume_plan_json, session_state_json_for_params, session_summary_json, snapshot_state_json,
    snapshots_json, string_array_json, validate_agent_task_list_params, window_state_json,
    windows_json_for_params,
};
pub(crate) use state_json::{
    dispatch_event_list_request, frame_read_json_with_context, layout_state_json,
    nullable_state_request_session_target_matches, observer_json, observers_json,
    project_trust_state_filter_from_params, session_state_name, state_request_pane_list_window_ids,
    state_request_session_target_matches,
};
use targets::{
    pane_by_id, parse_join_position, parse_json_object_value, parse_split_direction,
    resolve_pane_target_value, resolve_window_target_value, target_or_active_pane,
    target_value_has_pane_shape, window_by_id, window_id_for_target,
};
use types::{CaptureEndpoint, CaptureOrigin, CaptureRange};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
