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
use mez_mux::session::ClientTerminalDescriptor;

// Control dispatch entry points and dispatch internals.

/// Runs the dispatch control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
) -> String {
    dispatch_control_request_internal(body, session, primary_client_id, None)
}

/// Runs the client terminal descriptor from control operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_terminal_descriptor_from_control(
    terminal: Option<&TerminalDescriptor>,
) -> Option<ClientTerminalDescriptor> {
    terminal.map(|terminal| ClientTerminalDescriptor {
        columns: terminal.columns,
        rows: terminal.rows,
        term: terminal.term.clone(),
        features: terminal.features.clone(),
    })
}

/// Runs the dispatch control request with mcp operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_with_mcp(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    mcp_registry: &McpRegistry,
) -> String {
    dispatch_control_request_internal(body, session, primary_client_id, Some(mcp_registry))
}

/// Runs the dispatch control request with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_with_snapshots(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    snapshots: &SnapshotRepository,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };

    let result = if request.method.starts_with("snapshot/") {
        dispatch_snapshot_request(&request, session, snapshots)
    } else {
        dispatch_parsed_request(&request, session, primary_client_id, None)
    };
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch control request for client with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_snapshots(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    snapshots: &SnapshotRepository,
) -> String {
    dispatch_control_request_for_client_with_snapshot_captures(
        body,
        session,
        caller_client_id,
        snapshots,
        &[],
    )
}

/// Runs the dispatch control request for client with snapshot captures operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_snapshot_captures(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    snapshots: &SnapshotRepository,
    pane_captures: &[crate::snapshot::SnapshotPaneCapture],
) -> String {
    dispatch_control_request_for_client_with_snapshot_captures_and_config_layers(
        body,
        session,
        caller_client_id,
        snapshots,
        pane_captures,
        &[],
    )
}

/// Runs the dispatch control request for client with snapshot captures and config layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_snapshot_captures_and_config_layers(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    snapshots: &SnapshotRepository,
    pane_captures: &[crate::snapshot::SnapshotPaneCapture],
    active_config_layers: &[crate::snapshot::SnapshotConfigLayerMetadata],
) -> String {
    dispatch_control_request_for_client_with_snapshot_captures_config_layers_and_frame_state(
        body,
        session,
        caller_client_id,
        snapshots,
        pane_captures,
        active_config_layers,
        &crate::snapshot::SnapshotFrameState::default(),
    )
}

/// Runs the dispatch control request for client with snapshot captures config layers and frame state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_snapshot_captures_config_layers_and_frame_state(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    snapshots: &SnapshotRepository,
    pane_captures: &[crate::snapshot::SnapshotPaneCapture],
    active_config_layers: &[crate::snapshot::SnapshotConfigLayerMetadata],
    frame_state: &crate::snapshot::SnapshotFrameState,
) -> String {
    dispatch_control_request_for_client_with_snapshot_context(
        body,
        session,
        caller_client_id,
        snapshots,
        crate::snapshot::SnapshotCreationContext::new(
            pane_captures,
            active_config_layers,
            frame_state,
            &[],
        ),
    )
}

/// Runs the dispatch control request for client with snapshot context operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_snapshot_context(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    snapshots: &SnapshotRepository,
    snapshot_context: crate::snapshot::SnapshotCreationContext<'_>,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if let Err(error) = authorize_control_request(session, caller_client_id, &request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }

    let result = if request.method.starts_with("snapshot/") {
        dispatch_snapshot_request_with_context(&request, session, snapshots, snapshot_context)
    } else {
        dispatch_parsed_request(&request, session, caller_client_id, None)
    };
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch control request for client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    mcp_registry: Option<&McpRegistry>,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return invalid_control_request_response(&error);
        }
    };
    if let Err(error) = authorize_control_request(session, caller_client_id, &request) {
        return control_error_response(&request, &error);
    }
    dispatch_parsed_to_response(&request, session, caller_client_id, mcp_registry)
}

/// Runs the dispatch control request for client with agent state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_agent_state(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    mcp_registry: Option<&McpRegistry>,
    agent_store: &mut AgentShellStore,
    turn_ledger: &AgentTurnLedger,
) -> String {
    dispatch_control_request_for_client_with_agent_state_and_model_profiles(
        body,
        session,
        caller_client_id,
        mcp_registry,
        agent_store,
        turn_ledger,
        None,
    )
}

/// Dispatches an agent-state control request with optional runtime model
/// profile overrides for `agent/list` serialization.
///
/// Generic/offline callers pass `None` to retain the default placeholder
/// behavior, while the live runtime passes a pane-keyed map derived from
/// authoritative turn and model override state.
pub fn dispatch_control_request_for_client_with_agent_state_and_model_profiles(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    mcp_registry: Option<&McpRegistry>,
    agent_store: &mut AgentShellStore,
    turn_ledger: &AgentTurnLedger,
    model_profiles_by_pane: Option<&std::collections::BTreeMap<String, String>>,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if let Err(error) = authorize_control_request(session, caller_client_id, &request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    if let Err(error) = validate_control_method_params_schema(&request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    let result = match request.method.as_str() {
        "agent/list" => dispatch_agent_list_with_store_and_model_profiles(
            &request,
            session,
            agent_store,
            model_profiles_by_pane,
        ),
        "agent/task/list" => dispatch_agent_task_list_with_ledger(&request, session, turn_ledger),
        "agent/shell/show" | "agent/shell/hide" => {
            dispatch_agent_shell_visibility_with_store(&request, session, agent_store)
        }
        "agent/shell/command" => {
            dispatch_agent_shell_command_with_store(&request, session, agent_store)
        }
        _ => dispatch_parsed_request(&request, session, caller_client_id, mcp_registry),
    };
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch control request for client with config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_config(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    layers: &[ConfigLayer],
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if let Err(error) = authorize_control_request(session, caller_client_id, &request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    if is_config_control_method(&request.method) {
        let cached = config_request_cache_key(&request, caller_client_id)
            .is_some_and(|key| idempotency.completed.contains_key(&key));
        let response = dispatch_config_parsed_to_response_cached(
            &request,
            caller_client_id,
            layers,
            idempotency,
        );
        if !cached && config_response_advances_generation(&request.method, &response) {
            session.advance_config_generation();
        }
        return response;
    }
    dispatch_parsed_to_response(&request, session, caller_client_id, None)
}

/// Runs the dispatch control request for client with config and audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_config_and_audit(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    layers: &[ConfigLayer],
    idempotency: &mut ControlIdempotencyCache,
    audit_log: &mut AuditLog,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if let Err(error) = authorize_control_request(session, caller_client_id, &request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    if is_config_control_method(&request.method) {
        let audit_plan = config_audit_plan(session, caller_client_id, &request);
        let cached = config_request_cache_key(&request, caller_client_id)
            .is_some_and(|key| idempotency.completed.contains_key(&key));
        if let Some(mut record) = audit_plan.clone() {
            record.outcome = "started".to_string();
            if let Err(error) = audit_log.append(record.sanitized()) {
                return json_rpc_error(
                    &request.id,
                    error_code(error.kind()),
                    error.message(),
                    mezzanine_error_code(error.kind()),
                );
            }
        }
        let response = dispatch_config_parsed_to_response_cached(
            &request,
            caller_client_id,
            layers,
            idempotency,
        );
        if let Some(record) = audit_plan.map(|mut record| {
            record.outcome = config_audit_outcome(&response).to_string();
            record.sanitized()
        }) && let Err(error) = audit_log.append(record)
        {
            return json_rpc_error(
                &request.id,
                error_code(error.kind()),
                error.message(),
                mezzanine_error_code(error.kind()),
            );
        }
        if !cached && config_response_advances_generation(&request.method, &response) {
            session.advance_config_generation();
        }
        return response;
    }
    dispatch_parsed_to_response(&request, session, caller_client_id, None)
}

/// Runs the dispatch control request for client with events operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_client_with_events(
    body: &str,
    session: &mut Session,
    caller_client_id: &ClientId,
    mcp_registry: Option<&McpRegistry>,
    event_log: &EventLog,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if let Err(error) = authorize_control_request(session, caller_client_id, &request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    if request.method == "event/list" {
        return match dispatch_event_list_request(&request, session, caller_client_id, event_log) {
            Ok(events) => json_rpc_success(&request.id, &events),
            Err(error) => json_rpc_error(
                &request.id,
                error_code(error.kind()),
                error.message(),
                mezzanine_error_code(error.kind()),
            ),
        };
    }
    dispatch_parsed_to_response(&request, session, caller_client_id, mcp_registry)
}

/// Runs the dispatch control request with captures operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_with_captures(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    captures: &[PaneCaptureSource],
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    let result = if request.method == "pane/capture" {
        dispatch_pane_capture_request(&request, session, captures)
    } else {
        dispatch_parsed_request(&request, session, primary_client_id, None)
    };
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch control request with approvals operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_with_approvals(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    approval_queue: &mut BlockedApprovalQueue,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if let Err(error) = authorize_control_request(session, primary_client_id, &request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    if let Err(error) = validate_control_method_params_schema(&request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    let result = match request.method.as_str() {
        "approval/list" => {
            approvals_json_for_params(session, approval_queue, request.params.as_deref())
                .map(|approvals| format!(r#"{{"approvals":{approvals}}}"#))
        }
        "approval/decide" => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("approval/decide requires a params object"));
            params.and_then(|params| {
                require_idempotency_key(params)?;
                let approval_id = json_string_field(params, "approval_id").ok_or_else(|| {
                    MezError::invalid_args("approval/decide requires approval_id")
                })?;
                let decision = json_string_field(params, "decision")
                    .as_deref()
                    .map(parse_approval_decision)
                    .transpose()?
                    .ok_or_else(|| MezError::invalid_args("approval/decide requires decision"))?;
                approval_decide_scope_persistence(params)?;
                let instruction = json_string_field(params, "instruction");
                let approval = approval_queue.decide_with_client_at(
                    &approval_id,
                    decision,
                    instruction,
                    Some(primary_client_id.to_string()),
                    control_current_unix_seconds(),
                )?;
                Ok(format!(r#"{{"approval":{}}}"#, approval_json(approval)))
            })
        }
        _ => dispatch_parsed_request(&request, session, primary_client_id, None),
    };
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch control request with approvals and audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_with_approvals_and_audit(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    approval_queue: &mut BlockedApprovalQueue,
    audit_log: &mut AuditLog,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if let Err(error) = authorize_control_request(session, primary_client_id, &request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    if let Err(error) = validate_control_method_params_schema(&request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    let result = match request.method.as_str() {
        "approval/list" => {
            approvals_json_for_params(session, approval_queue, request.params.as_deref())
                .map(|approvals| format!(r#"{{"approvals":{approvals}}}"#))
        }
        "approval/decide" => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("approval/decide requires a params object"));
            params.and_then(|params| {
                require_idempotency_key(params)?;
                let approval_id = json_string_field(params, "approval_id").ok_or_else(|| {
                    MezError::invalid_args("approval/decide requires approval_id")
                })?;
                let decision = json_string_field(params, "decision")
                    .as_deref()
                    .map(parse_approval_decision)
                    .transpose()?
                    .ok_or_else(|| MezError::invalid_args("approval/decide requires decision"))?;
                approval_decide_scope_persistence(params)?;
                let instruction = json_string_field(params, "instruction");
                let pending = approval_queue.get(&approval_id).ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "approval request not found",
                    )
                })?;
                audit_log.append(approval_audit_record(
                    session,
                    primary_client_id,
                    pending,
                    "started",
                ))?;
                let approval = approval_queue.decide_with_client_at(
                    &approval_id,
                    decision,
                    instruction,
                    Some(primary_client_id.to_string()),
                    control_current_unix_seconds(),
                )?;
                audit_log.append(approval_audit_record(
                    session,
                    primary_client_id,
                    approval,
                    "applied",
                ))?;
                Ok(format!(r#"{{"approval":{}}}"#, approval_json(approval)))
            })
        }
        _ => dispatch_parsed_request(&request, session, primary_client_id, None),
    };
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}
/// Carries Control Connection State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlConnectionState {
    /// Stores the initialized value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) initialized: bool,
    /// Stores the outer authenticated value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) outer_authenticated: bool,
    /// Stores the trusted interactive assertion value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) trusted_interactive_assertion: bool,
    /// Stores the caller client id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) caller_client_id: Option<ClientId>,
    /// Stores whether EOF on this connection should detach the primary client.
    ///
    /// This is opt-in for foreground attach sockets so request-scoped control
    /// clients can close without mutating primary ownership.
    pub(super) detach_primary_on_disconnect: bool,
}

impl ControlConnectionState {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(outer_authenticated: bool, trusted_interactive_assertion: bool) -> Self {
        Self {
            initialized: false,
            outer_authenticated,
            trusted_interactive_assertion,
            caller_client_id: None,
            detach_primary_on_disconnect: false,
        }
    }

    /// Runs the trusted existing client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn trusted_existing_client(caller_client_id: ClientId) -> Self {
        Self {
            initialized: true,
            outer_authenticated: true,
            trusted_interactive_assertion: true,
            caller_client_id: Some(caller_client_id),
            detach_primary_on_disconnect: false,
        }
    }

    /// Runs the caller client id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn caller_client_id(&self) -> Option<&ClientId> {
        self.caller_client_id.as_ref()
    }

    /// Runs the rebind caller client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rebind_caller_client(&mut self, caller_client_id: ClientId) {
        self.initialized = true;
        self.caller_client_id = Some(caller_client_id);
    }

    /// Runs the initialized operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    /// Returns whether EOF on this connection should detach the primary client.
    pub fn detach_primary_on_disconnect(&self) -> bool {
        self.detach_primary_on_disconnect
    }
}

/// Runs the handle control frames for connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn handle_control_frames_for_connection(
    input: &[u8],
    max_content_length: usize,
    session: &mut Session,
    connection: &mut ControlConnectionState,
    idempotency: &mut ControlIdempotencyCache,
) -> Result<(Vec<u8>, usize)> {
    let mut offset = 0usize;
    let mut output = Vec::new();
    while offset < input.len() {
        let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
        let response =
            dispatch_control_request_for_connection(&body, session, connection, idempotency);
        output.extend_from_slice(&encode_control_body(&response));
        offset += consumed;
    }
    Ok((output, offset))
}

/// Runs the dispatch control request for connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_connection(
    body: &str,
    session: &mut Session,
    connection: &mut ControlConnectionState,
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };

    if !connection.initialized {
        if request.method != "control/initialize" {
            return json_rpc_error(
                &request.id,
                error_code(crate::error::MezErrorKind::Forbidden),
                "first control request must be control/initialize",
                mezzanine_error_code(crate::error::MezErrorKind::Forbidden),
            );
        }
        return match initialize_control_connection(&request, session, connection) {
            Ok(result) => json_rpc_success(&request.id, &initialize_result_json(&result)),
            Err(error) => json_rpc_error(
                &request.id,
                error_code(error.kind()),
                error.message(),
                mezzanine_error_code(error.kind()),
            ),
        };
    }

    if request.method == "control/initialize" {
        return json_rpc_error(
            &request.id,
            error_code(crate::error::MezErrorKind::InvalidState),
            "control connection is already initialized",
            mezzanine_error_code(crate::error::MezErrorKind::InvalidState),
        );
    }

    let caller_client_id = match connection.caller_client_id.clone() {
        Some(client_id) => client_id,
        None => {
            return json_rpc_error(
                &request.id,
                error_code(crate::error::MezErrorKind::Forbidden),
                "control connection has no authenticated session client",
                mezzanine_error_code(crate::error::MezErrorKind::Forbidden),
            );
        }
    };
    dispatch_control_request_cached_for_client(&request, session, &caller_client_id, idempotency)
}

/// Runs the dispatch control request cached for client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_control_request_cached_for_client(
    request: &JsonRpcRequest,
    session: &mut Session,
    caller_client_id: &ClientId,
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    if let Err(error) = authorize_control_request(session, caller_client_id, request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    let cache_key = request
        .params
        .as_deref()
        .and_then(|params| json_string_field(params, "idempotency_key"))
        .map(|key| format!("{caller_client_id}:{key}"));
    if let Some(cache_key) = &cache_key {
        match idempotency.cached_response(cache_key, &request.method, &request.params) {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(error) => {
                return json_rpc_error(
                    &request.id,
                    error_code(error.kind()),
                    error.message(),
                    mezzanine_error_code(error.kind()),
                );
            }
        }
    }
    let response = dispatch_parsed_to_response(request, session, caller_client_id, None);
    if let Some(cache_key) = cache_key {
        idempotency.remember_response(
            cache_key,
            request.method.clone(),
            request.params.clone(),
            response.clone(),
        );
    }
    response
}

/// Runs the initialize control connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn initialize_control_connection(
    request: &JsonRpcRequest,
    session: &mut Session,
    connection: &mut ControlConnectionState,
) -> Result<InitializeResult> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("control/initialize requires a params object"))?;
    let init = initialize_params_from_json(params)?;
    let authentication = init
        .authentication
        .as_ref()
        .unwrap_or(&AuthenticationMaterial {
            mechanism: AuthenticationMechanism::None,
            token: None,
        });
    let authenticated = connection.outer_authenticated || authentication.is_payload_authenticated();

    if !authenticated {
        connection.initialized = true;
        return initialize(
            init,
            InitializeContext {
                outer_authenticated: false,
                trusted_interactive_assertion: connection.trusted_interactive_assertion,
            },
        );
    }

    let selected_version = negotiate_protocol_version(init.requested_version)?;
    if let Some(session_target) = init.session_target_json.as_deref() {
        let session_target =
            serde_json::from_str::<serde_json::Value>(session_target).map_err(|error| {
                MezError::invalid_args(format!("session_target is invalid: {error}"))
            })?;
        require_session_target_matches_value(session, &session_target)?;
    }
    match init.requested_role {
        RequestedRole::Primary => {
            let client = init.client.as_ref().ok_or_else(|| {
                MezError::invalid_args("primary initialization requires a client descriptor")
            })?;
            if !client.identifies_interactive_terminal(connection.trusted_interactive_assertion) {
                return Err(MezError::forbidden(
                    "primary initialization requires a verified interactive terminal",
                ));
            }
            let client_id = match session.primary_client_id().cloned() {
                Some(existing_primary) => {
                    let existing = session
                        .clients()
                        .iter()
                        .find(|client| client.id == existing_primary)
                        .ok_or_else(|| {
                            MezError::invalid_state("primary client is missing from client list")
                        })?;
                    if existing.name != init.client_name {
                        return Err(MezError::conflict(
                            "session already has an attached primary client",
                        ));
                    }
                    existing_primary
                }
                None => session.attach_primary_with_terminal(
                    init.client_name.clone(),
                    client.interactive,
                    client_terminal_descriptor_from_control(client.terminal.as_ref()),
                )?,
            };
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            connection.detach_primary_on_disconnect = init.detach_primary_on_disconnect;
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                granted_role: GrantedRole::Primary,
                capabilities: Capabilities::primary(),
                approval_pending: false,
                observer_request: None,
            })
        }
        RequestedRole::Observer => {
            let terminal = init.client.as_ref().and_then(|client| {
                client_terminal_descriptor_from_control(client.terminal.as_ref())
            });
            let (client_id, observer_id) =
                session.request_observer_with_terminal(init.client_name, terminal);
            let observer_state = observer_json(session, observer_id.as_str())?;
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            connection.detach_primary_on_disconnect = false;
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: None,
                granted_role: GrantedRole::PendingObserver,
                capabilities: Capabilities::pending_observer(),
                approval_pending: true,
                observer_request: Some(ObserverRequestSummary {
                    request_id: observer_id.to_string(),
                    state: "pending",
                    state_json: Some(observer_state),
                }),
            })
        }
        RequestedRole::Agent => {
            let client_id = session.attach_control_client(
                init.client_name,
                ClientRole::Agent,
                init.client
                    .as_ref()
                    .is_some_and(|client| client.interactive),
            )?;
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                granted_role: GrantedRole::Agent,
                capabilities: Capabilities::agent(),
                approval_pending: false,
                observer_request: None,
            })
        }
        RequestedRole::Automation => {
            let client_id = session.attach_control_client(
                init.client_name,
                ClientRole::Automation,
                init.client
                    .as_ref()
                    .is_some_and(|client| client.interactive),
            )?;
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                granted_role: GrantedRole::Automation,
                capabilities: Capabilities::automation(),
                approval_pending: false,
                observer_request: None,
            })
        }
    }
}

/// Runs the dispatch control request cached operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_cached(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    let cache_key = request
        .params
        .as_deref()
        .and_then(|params| json_string_field(params, "idempotency_key"))
        .map(|key| format!("{primary_client_id}:{key}"));
    if let Some(cache_key) = &cache_key {
        match idempotency.cached_response(cache_key, &request.method, &request.params) {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(error) => {
                return json_rpc_error(
                    &request.id,
                    error_code(error.kind()),
                    error.message(),
                    mezzanine_error_code(error.kind()),
                );
            }
        }
    }

    let response = dispatch_parsed_to_response(&request, session, primary_client_id, None);
    if let Some(cache_key) = cache_key {
        idempotency.remember_response(cache_key, request.method, request.params, response.clone());
    }
    response
}

/// Runs the dispatch control request internal operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_control_request_internal(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    mcp_registry: Option<&McpRegistry>,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };

    dispatch_parsed_to_response(&request, session, primary_client_id, mcp_registry)
}

/// Runs the dispatch parsed to response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_parsed_to_response(
    request: &JsonRpcRequest,
    session: &mut Session,
    primary_client_id: &ClientId,
    mcp_registry: Option<&McpRegistry>,
) -> String {
    control_result_response(
        request,
        dispatch_parsed_request(request, session, primary_client_id, mcp_registry),
    )
}

/// Converts a parsed control dispatch result into its JSON-RPC envelope.
///
/// All control dispatch entry points use the same success and error envelope
/// shape. Keeping that mapping in one helper prevents wrapper drift while
/// preserving each entry point's authorization and subsystem-specific dispatch
/// policy.
fn control_result_response(request: &JsonRpcRequest, result: Result<String>) -> String {
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => control_error_response(request, &error),
    }
}

/// Converts one parsed control error into a JSON-RPC error envelope.
fn control_error_response(request: &JsonRpcRequest, error: &MezError) -> String {
    json_rpc_error(
        &request.id,
        error_code(error.kind()),
        error.message(),
        mezzanine_error_code(error.kind()),
    )
}

/// Converts a malformed control envelope into the standard invalid request response.
fn invalid_control_request_response(error: &MezError) -> String {
    json_rpc_error("null", -32600, error.message(), "invalid_request")
}

/// Runs the dispatch parsed request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_parsed_request(
    request: &JsonRpcRequest,
    session: &mut Session,
    primary_client_id: &ClientId,
    mcp_registry: Option<&McpRegistry>,
) -> Result<String> {
    validate_control_method_params_schema(request)?;
    let Some(method_spec) = control_method_spec(&request.method) else {
        return Err(MezError::not_implemented(format!(
            "unknown control method `{}`",
            request.method
        )));
    };
    match method_spec.dispatch {
        ControlDispatchKind::ControlInitialize => {
            let mut connection = ControlConnectionState::new(true, true);
            let init = initialize_control_connection(request, session, &mut connection)?;
            Ok(initialize_result_json(&init))
        }
        ControlDispatchKind::ControlShutdown => Ok(r#"{"closed":true}"#.to_string()),
        ControlDispatchKind::ControlCancel => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("control/cancel requires a params object"))?;
            let _request_id = json_string_field(params, "request_id")
                .ok_or_else(|| MezError::invalid_args("control/cancel requires request_id"))?;
            Ok(r#"{"cancel_requested":false}"#.to_string())
        }
        ControlDispatchKind::SessionAttach => dispatch_session_attach_parsed(request, session),
        ControlDispatchKind::SessionList => Ok(format!(
            r#"{{"sessions":[{}]}}"#,
            session_summary_json(session)
        )),
        ControlDispatchKind::SessionGet => Ok(format!(
            r#"{{"session":{}}}"#,
            session_state_json_for_params(session, request.params.as_deref())?
        )),
        ControlDispatchKind::WindowList => Ok(format!(
            r#"{{"windows":{}}}"#,
            windows_json_for_params(session, request.params.as_deref())?
        )),
        ControlDispatchKind::PaneList => Ok(format!(
            r#"{{"panes":{}}}"#,
            panes_json_for_params(session, request.params.as_deref())?
        )),
        ControlDispatchKind::FrameRead => frame_read_json(session, request.params.as_deref()),
        ControlDispatchKind::ClientList => Ok(format!(
            r#"{{"clients":{}}}"#,
            clients_json_for_params(session, request.params.as_deref())?
        )),
        ControlDispatchKind::ObserverList => Ok(format!(
            r#"{{"observers":{}}}"#,
            observers_json_for_params(session, request.params.as_deref())?
        )),
        ControlDispatchKind::ObserverInspect => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("observer/inspect requires a params object")
            })?;
            let observer_id =
                json_string_field(params, "observer_request_id").ok_or_else(|| {
                    MezError::invalid_args("observer/inspect requires observer_request_id")
                })?;
            Ok(format!(
                r#"{{"observer":{}}}"#,
                observer_json(session, &observer_id)?
            ))
        }
        ControlDispatchKind::WindowCreate => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("window/create requires a params object"))?;
            require_idempotency_key(params)?;
            reject_runtime_required_creation_fields(
                "window/create",
                params,
                &["shell_command", "start_directory"],
            )?;
            let name = json_string_field(params, "name").unwrap_or_else(|| "shell".to_string());
            let select = json_bool_field(params, "select").unwrap_or(true);
            let id = session.new_window(primary_client_id, name, select)?;
            let window = window_by_id(session, id.as_str())?;
            let pane = window.active_pane();
            Ok(format!(
                r#"{{"window":{},"pane":{}}}"#,
                window_state_json(session, window),
                pane_state_json(session, window, pane)
            ))
        }
        ControlDispatchKind::WindowRename => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("window/rename requires a params object"))?;
            require_idempotency_key(params)?;
            let name = json_string_field(params, "name")
                .ok_or_else(|| MezError::invalid_args("window/rename requires name"))?;
            let target = window_target_checked_resolved(session, params)?;
            let renamed_window_id = window_id_for_target(session, target.as_deref())?;
            session.rename_window(primary_client_id, target.as_deref(), name)?;
            let window = window_by_id(session, &renamed_window_id)?;
            Ok(format!(
                r#"{{"window":{}}}"#,
                window_state_json(session, window)
            ))
        }
        ControlDispatchKind::WindowSelect => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("window/select requires a params object"))?;
            require_idempotency_key(params)?;
            let target = window_target_checked_resolved(session, params)?
                .ok_or_else(|| MezError::invalid_args("window/select requires target"))?;
            session.select_window(primary_client_id, &target)?;
            let active = session
                .active_window()
                .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
            Ok(format!(
                r#"{{"active_window_id":"{}"}}"#,
                json_escape(&active.id.to_string())
            ))
        }
        ControlDispatchKind::WindowClose => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("window/close requires a params object"))?;
            require_idempotency_key(params)?;
            let force = json_bool_field(params, "force").unwrap_or(false);
            let target = window_target_checked_resolved(session, params)?;
            session.kill_window(primary_client_id, target.as_deref(), force)?;
            Ok(r#"{"closed":true}"#.to_string())
        }
        ControlDispatchKind::PaneCreate => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("pane/create requires a params object"))?;
            require_idempotency_key(params)?;
            reject_runtime_required_creation_fields(
                "pane/create",
                params,
                &["shell_command", "start_directory", "size"],
            )?;
            if let Some(target) = pane_target_checked_resolved(session, params)? {
                session.select_pane(primary_client_id, &target)?;
            }
            let split =
                json_string_field(params, "split").unwrap_or_else(|| "vertical".to_string());
            let select = json_bool_field(params, "select").unwrap_or(true);
            if split == "window" {
                let window_id = session.new_window(primary_client_id, "shell", select)?;
                let window = window_by_id(session, window_id.as_str())?;
                let pane = window.active_pane();
                return Ok(format!(
                    r#"{{"pane":{},"layout":{}}}"#,
                    pane_state_json(session, window, pane),
                    layout_state_json(window)
                ));
            }
            let direction = parse_split_direction(&split)?;
            let pane_id = session.split_active_pane_select(primary_client_id, direction, select)?;
            let (window, pane) = pane_by_id(session, pane_id.as_str())?;
            Ok(format!(
                r#"{{"pane":{},"layout":{}}}"#,
                pane_state_json(session, window, pane),
                layout_state_json(window)
            ))
        }
        ControlDispatchKind::PaneSelect => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("pane/select requires a params object"))?;
            require_idempotency_key(params)?;
            let target = pane_target_checked_resolved(session, params)?
                .ok_or_else(|| MezError::invalid_args("pane/select requires target"))?;
            session.select_pane(primary_client_id, &target)?;
            let pane = session
                .active_window()
                .ok_or_else(|| MezError::invalid_state("session has no active window"))?
                .active_pane();
            Ok(format!(
                r#"{{"active_pane_id":"{}"}}"#,
                json_escape(&pane.id.to_string())
            ))
        }
        ControlDispatchKind::PaneResize => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("pane/resize requires a params object"))?;
            require_idempotency_key(params)?;
            let target = pane_target_checked_resolved(session, params)?;
            let spec = control_pane_size_spec(params, "pane/resize size")?;
            let pane = session.resize_pane_with_spec(primary_client_id, target.as_deref(), spec)?;
            let (window, pane_state) = pane_by_id(session, pane.id.as_str())?;
            Ok(format!(
                r#"{{"pane":{},"layout":{}}}"#,
                pane_state_json(session, window, pane_state),
                session
                    .active_window()
                    .map(layout_state_json)
                    .unwrap_or_else(|| "null".to_string())
            ))
        }
        ControlDispatchKind::PaneSwap => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("pane/swap requires a params object"))?;
            require_idempotency_key(params)?;
            let source = source_pane_target_checked_resolved(session, params)?;
            let destination = destination_target_checked_resolved(session, params)?
                .ok_or_else(|| MezError::invalid_args("pane/swap requires destination"))?;
            session.swap_panes(primary_client_id, source.as_deref(), &destination)?;
            let window = session
                .active_window()
                .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
            Ok(format!(r#"{{"layout":{}}}"#, layout_state_json(window)))
        }
        ControlDispatchKind::PaneBreak => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("pane/break requires a params object"))?;
            require_idempotency_key(params)?;
            let target = pane_target_checked_resolved(session, params)?;
            let name = json_string_field(params, "name");
            let window_id = session.break_pane(primary_client_id, target.as_deref(), name, true)?;
            let window = window_by_id(session, window_id.as_str())?;
            let pane = window.active_pane();
            Ok(format!(
                r#"{{"window":{},"pane":{}}}"#,
                window_state_json(session, window),
                pane_state_json(session, window, pane)
            ))
        }
        ControlDispatchKind::PaneJoinMove => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args(format!("{} requires a params object", request.method))
            })?;
            require_idempotency_key(params)?;
            let source = source_pane_target_checked_resolved(session, params)?;
            let destination =
                destination_target_checked_resolved(session, params)?.ok_or_else(|| {
                    MezError::invalid_args(format!("{} requires destination", request.method))
                })?;
            let direction = json_string_field(params, "position")
                .as_deref()
                .map(parse_join_position)
                .transpose()?
                .unwrap_or(SplitDirection::Vertical);
            let pane_id = session.join_pane(
                primary_client_id,
                source.as_deref(),
                &destination,
                direction,
                true,
            )?;
            let (window, pane) = pane_by_id(session, pane_id.as_str())?;
            Ok(format!(
                r#"{{"pane":{},"layout":{}}}"#,
                pane_state_json(session, window, pane),
                layout_state_json(window)
            ))
        }
        ControlDispatchKind::PaneClose => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("pane/close requires a params object"))?;
            require_idempotency_key(params)?;
            let force = json_bool_field(params, "force").unwrap_or(false);
            let target = pane_target_checked_resolved(session, params)?;
            session.kill_pane(primary_client_id, target.as_deref(), force)?;
            Ok(r#"{"closed":true}"#.to_string())
        }
        ControlDispatchKind::PaneCapture => dispatch_pane_capture_request(request, session, &[]),
        ControlDispatchKind::TerminalView => {
            Err(MezError::invalid_state("terminal runtime is not attached"))
        }
        ControlDispatchKind::TerminalStep => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("terminal/step requires a params object"))?;
            require_idempotency_key(params)?;
            Err(MezError::invalid_state("terminal runtime is not attached"))
        }
        ControlDispatchKind::TerminalCommand => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("terminal/command requires a params object")
            })?;
            require_idempotency_key(params)?;
            let _input = json_string_field(params, "input")
                .ok_or_else(|| MezError::invalid_args("terminal/command requires input"))?;
            Err(MezError::invalid_state("terminal runtime is not attached"))
        }
        ControlDispatchKind::SessionRename => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("session/rename requires a params object"))?;
            require_idempotency_key(params)?;
            let name = json_string_field(params, "name")
                .ok_or_else(|| MezError::invalid_args("session/rename requires name"))?;
            session.rename_session(primary_client_id, name)?;
            Ok(r#"{"renamed":true}"#.to_string())
        }
        ControlDispatchKind::SessionKill => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("session/kill requires a params object"))?;
            require_idempotency_key(params)?;
            let force = json_bool_field(params, "force").unwrap_or(false);
            session.kill_session(primary_client_id, force)?;
            Ok(r#"{"killed":true}"#.to_string())
        }
        ControlDispatchKind::ClientDetach => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("client/detach requires a params object"))?;
            require_idempotency_key(params)?;
            let client_id = json_string_field(params, "client_id")
                .unwrap_or_else(|| primary_client_id.to_string());
            session.detach_client_target(primary_client_id, &client_id)?;
            Ok(r#"{"detached":true}"#.to_string())
        }
        ControlDispatchKind::ClientSelectPrimary => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("client/select_primary requires a params object")
            })?;
            require_idempotency_key(params)?;
            let client_id = json_string_field(params, "client_id").ok_or_else(|| {
                MezError::invalid_args("client/select_primary requires client_id")
            })?;
            let selected = session.select_primary_client(Some(primary_client_id), &client_id)?;
            Ok(format!(
                r#"{{"primary_client_id":"{}"}}"#,
                json_escape(&selected.to_string())
            ))
        }
        ControlDispatchKind::ObserverApprove => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("observer/approve requires a params object")
            })?;
            require_idempotency_key(params)?;
            let observer_id =
                json_string_field(params, "observer_request_id").ok_or_else(|| {
                    MezError::invalid_args("observer/approve requires observer_request_id")
                })?;
            session.approve_observer_target(primary_client_id, &observer_id)?;
            Ok(format!(
                r#"{{"observer":{}}}"#,
                observer_json(session, &observer_id)?
            ))
        }
        ControlDispatchKind::ObserverReject => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("observer/reject requires a params object")
            })?;
            require_idempotency_key(params)?;
            let observer_id =
                json_string_field(params, "observer_request_id").ok_or_else(|| {
                    MezError::invalid_args("observer/reject requires observer_request_id")
                })?;
            let reason = json_string_field(params, "reason");
            session.reject_observer_target_with_reason(primary_client_id, &observer_id, reason)?;
            Ok(format!(
                r#"{{"observer":{}}}"#,
                observer_json(session, &observer_id)?
            ))
        }
        ControlDispatchKind::ObserverRevoke => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("observer/revoke requires a params object")
            })?;
            require_idempotency_key(params)?;
            let client_id = json_string_field(params, "client_id")
                .ok_or_else(|| MezError::invalid_args("observer/revoke requires client_id"))?;
            let reason = json_string_field(params, "reason");
            session.revoke_observer_client_with_reason(primary_client_id, &client_id, reason)?;
            Ok(r#"{"revoked":true}"#.to_string())
        }
        ControlDispatchKind::AgentList => Ok(format!(
            r#"{{"agents":{}}}"#,
            agents_json_for_params(session, request.params.as_deref())?
        )),
        ControlDispatchKind::AgentTaskList => {
            validate_agent_task_list_params(session, request.params.as_deref())?;
            Ok(r#"{"tasks":[]}"#.to_string())
        }
        ControlDispatchKind::AgentSpawn => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("agent/spawn requires a params object"))?;
            require_idempotency_key(params)?;
            let _parent_agent = json_object_field(params, "parent_agent")
                .ok_or_else(|| MezError::invalid_args("agent/spawn requires parent_agent"))?;
            let _placement = json_object_field(params, "placement")
                .ok_or_else(|| MezError::invalid_args("agent/spawn requires placement"))?;
            let _role = json_string_field(params, "role")
                .ok_or_else(|| MezError::invalid_args("agent/spawn requires role"))?;
            if json_raw_field(params, "cooperation_mode").is_some()
                && json_string_field(params, "cooperation_mode").is_none()
            {
                return Err(MezError::invalid_args(
                    "agent/spawn cooperation_mode must be a string",
                ));
            }
            if let Some(read_scopes) = json_raw_field(params, "read_scopes")
                && !read_scopes.trim_start().starts_with('[')
            {
                return Err(MezError::invalid_args(
                    "agent/spawn read_scopes must be an array",
                ));
            }
            if let Some(write_scopes) = json_raw_field(params, "write_scopes")
                && !write_scopes.trim_start().starts_with('[')
            {
                return Err(MezError::invalid_args(
                    "agent/spawn write_scopes must be an array",
                ));
            }
            let _prompt = json_string_field(params, "prompt")
                .ok_or_else(|| MezError::invalid_args("agent/spawn requires prompt"))?;
            Err(MezError::invalid_state("agent runtime is not attached"))
        }
        ControlDispatchKind::AgentShellVisibility { visible } => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args(format!("{} requires a params object", request.method))
            })?;
            require_idempotency_key(params)?;
            let target = pane_target_checked_resolved(session, params)?;
            let (window, pane) = target_or_active_pane(session, target.as_deref())?;
            Ok(format!(
                r#"{{"agent":{},"visible":{}}}"#,
                agent_state_json(session.id.as_str(), window, pane, visible),
                visible
            ))
        }
        ControlDispatchKind::AgentShellCommand => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("agent/shell/command requires a params object")
            })?;
            require_idempotency_key(params)?;
            let input = json_string_field(params, "input")
                .ok_or_else(|| MezError::invalid_args("agent/shell/command requires input"))?;
            let (_window, pane) = target_or_active_pane(session, None)?;
            Ok(agent_shell_command_response_json(
                pane.id.as_str(),
                &input,
                None,
            ))
        }
        ControlDispatchKind::EventList => {
            let event_log = EventLog::new(MAX_EVENT_REPLAY_RETENTION, 1_048_576)?;
            dispatch_event_list_request(request, session, primary_client_id, &event_log)
        }
        ControlDispatchKind::ApprovalList => {
            let approval_queue = BlockedApprovalQueue::default();
            Ok(format!(
                r#"{{"approvals":{}}}"#,
                approvals_json_for_params(session, &approval_queue, request.params.as_deref())?
            ))
        }
        ControlDispatchKind::ApprovalDecide => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("approval/decide requires a params object")
            })?;
            require_idempotency_key(params)?;
            let approval_id = json_string_field(params, "approval_id")
                .ok_or_else(|| MezError::invalid_args("approval/decide requires approval_id"))?;
            let decision = json_string_field(params, "decision")
                .as_deref()
                .map(parse_approval_decision)
                .transpose()?
                .ok_or_else(|| MezError::invalid_args("approval/decide requires decision"))?;
            approval_decide_scope_persistence(params)?;
            let instruction = json_string_field(params, "instruction");
            let mut approval_queue = BlockedApprovalQueue::default();
            let approval = approval_queue.decide_with_client_at(
                &approval_id,
                decision,
                instruction,
                Some(primary_client_id.to_string()),
                control_current_unix_seconds(),
            )?;
            Ok(format!(r#"{{"approval":{}}}"#, approval_json(approval)))
        }
        ControlDispatchKind::SnapshotList => {
            nullable_state_request_session_target_matches(
                session,
                request.params.as_deref(),
                "snapshot/list params",
            )?;
            Ok(r#"{"snapshots":[]}"#.to_string())
        }
        ControlDispatchKind::LayoutSave => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("snapshot/create requires a params object")
            })?;
            require_idempotency_key(params)?;
            let target = json_object_field(params, "target")
                .ok_or_else(|| MezError::invalid_args("snapshot/create requires target"))?;
            require_session_target_matches(&target, session)?;
            Err(MezError::invalid_state(
                "snapshot repository is not configured",
            ))
        }
        ControlDispatchKind::LayoutLoad | ControlDispatchKind::SnapshotDelete => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args(format!("{} requires a params object", request.method))
            })?;
            require_idempotency_key(params)?;
            let _snapshot_id = json_string_field(params, "snapshot_id").ok_or_else(|| {
                MezError::invalid_args(format!("{} requires snapshot_id", request.method))
            })?;
            Err(MezError::invalid_state(
                "snapshot repository is not configured",
            ))
        }
        ControlDispatchKind::ProjectTrustList => {
            project_trust_state_filter_from_params(
                request.params.as_deref(),
                "project/trust/list params",
            )?;
            Ok(r#"{"projects":[]}"#.to_string())
        }
        ControlDispatchKind::ProjectTrustInspect => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("project/trust/inspect requires a params object")
            })?;
            let _project_root = json_string_field(params, "project_root").ok_or_else(|| {
                MezError::invalid_args("project/trust/inspect requires project_root")
            })?;
            Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "project not found",
            ))
        }
        ControlDispatchKind::ProjectTrustDecide => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("project/trust/decide requires a params object")
            })?;
            require_idempotency_key(params)?;
            let _project_root = json_string_field(params, "project_root").ok_or_else(|| {
                MezError::invalid_args("project/trust/decide requires project_root")
            })?;
            let _decision = json_string_field(params, "decision")
                .as_deref()
                .map(parse_trust_decision)
                .transpose()?
                .ok_or_else(|| MezError::invalid_args("project/trust/decide requires decision"))?;
            Err(MezError::invalid_state(
                "project trust store is not configured",
            ))
        }
        ControlDispatchKind::ProjectTrustRevoke => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("project/trust/revoke requires a params object")
            })?;
            require_idempotency_key(params)?;
            let _project_root = json_string_field(params, "project_root").ok_or_else(|| {
                MezError::invalid_args("project/trust/revoke requires project_root")
            })?;
            Err(MezError::invalid_state(
                "project trust store is not configured",
            ))
        }
        ControlDispatchKind::McpList => {
            nullable_state_request_session_target_matches(
                session,
                request.params.as_deref(),
                "mcp/list params",
            )?;
            let empty = McpRegistry::default();
            let registry = mcp_registry.unwrap_or(&empty);
            Ok(format!(
                r#"{{"servers":{},"tools":{}}}"#,
                mcp_servers_json(registry),
                mcp_tools_json(registry)
            ))
        }
        ControlDispatchKind::McpRetry => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("mcp/retry requires a params object"))?;
            require_idempotency_key(params)?;
            let _server_id = json_string_field(params, "server_id")
                .or_else(|| json_string_field(params, "id"))
                .ok_or_else(|| MezError::invalid_args("mcp/retry requires server_id"))?;
            Err(MezError::invalid_state("MCP runtime is not attached"))
        }
        ControlDispatchKind::Config => dispatch_config_parsed_request(request, &[]),
    }
}

/// Returns the wall-clock timestamp supplied to lower approval state changes.
fn control_current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Runs the validate control method params schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn validate_control_method_params_schema(request: &JsonRpcRequest) -> Result<()> {
    let params = request.params.as_deref().unwrap_or("{}");
    let Some(method_spec) = control_method_spec(&request.method) else {
        return Ok(());
    };
    match method_spec.params_schema {
        ControlParamsSchema::Unchecked => Ok(()),
        ControlParamsSchema::Allowed(allowed_fields) => reject_unknown_json_fields(
            params,
            &format!("{} params", request.method),
            allowed_fields,
        ),
        ControlParamsSchema::Config => validate_config_control_params_schema(request),
    }
}

/// Runs the reject runtime required creation fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn reject_runtime_required_creation_fields(
    method: &str,
    params: &str,
    fields: &[&str],
) -> Result<()> {
    for field in fields {
        if json_raw_field(params, field).is_some() && !json_null_field(params, field) {
            return Err(MezError::invalid_state(format!(
                "{method} requires an attached terminal runtime for `{field}`"
            )));
        }
    }
    Ok(())
}

/// Runs the dispatch session attach parsed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_session_attach_parsed(
    request: &JsonRpcRequest,
    session: &mut Session,
) -> Result<String> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("session/attach requires a params object"))?;
    require_idempotency_key(params)?;
    let role = json_string_field(params, "role").unwrap_or_else(|| "primary".to_string());
    let client_descriptor = json_object_field(params, "client")
        .as_deref()
        .map(client_descriptor_from_json)
        .transpose()?
        .ok_or_else(|| MezError::invalid_args("session/attach requires client"))?;
    match role.as_str() {
        "primary" => {
            ensure_client_descriptor_role_matches(
                &client_descriptor,
                RequestedRole::Primary,
                "session/attach client descriptor",
            )?;
            let client_id = session.attach_primary_with_terminal(
                &client_descriptor.name,
                client_descriptor.interactive,
                client_terminal_descriptor_from_control(client_descriptor.terminal.as_ref()),
            )?;
            let client = session
                .clients()
                .iter()
                .find(|client| client.id == client_id)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
                })?;
            Ok(format!(
                r#"{{"client":{},"approval_pending":false}}"#,
                client_json(session, client)
            ))
        }
        "observer" => {
            ensure_client_descriptor_role_matches(
                &client_descriptor,
                RequestedRole::Observer,
                "session/attach client descriptor",
            )?;
            let (client_id, _observer_id) = session.request_observer_with_terminal(
                &client_descriptor.name,
                client_terminal_descriptor_from_control(client_descriptor.terminal.as_ref()),
            );
            let client = session
                .clients()
                .iter()
                .find(|client| client.id == client_id)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
                })?;
            Ok(format!(
                r#"{{"client":{},"approval_pending":true}}"#,
                client_json(session, client)
            ))
        }
        _ => Err(MezError::invalid_args(
            "session/attach role must be primary or observer",
        )),
    }
}

/// Runs the control pane size spec operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn control_pane_size_spec(params: &str, context: &'static str) -> Result<PaneSizeSpec> {
    let value = parse_json_object_value(params, "pane/resize params")?;
    let size = value
        .get("size")
        .ok_or_else(|| MezError::invalid_args("pane/resize requires size"))?;
    control_pane_size_spec_value(size, context)
}

/// Runs the control pane size spec value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn control_pane_size_spec_value(
    value: &serde_json::Value,
    context: &'static str,
) -> Result<PaneSizeSpec> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{context} must be an object")))?;
    let mode = object
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("{context} requires mode")))?;
    match mode {
        "cells" => {
            let columns = optional_size_u16(object.get("columns"), "columns")?;
            let rows = optional_size_u16(object.get("rows"), "rows")?;
            if columns.is_none() && rows.is_none() {
                return Err(MezError::invalid_args(
                    "cells size requires columns or rows",
                ));
            }
            Ok(PaneSizeSpec::Cells { columns, rows })
        }
        "percent" => {
            let percent = required_size_u16(object.get("percent"), "percent")?;
            let axis = match object
                .get("axis")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("both")
            {
                "columns" | "horizontal" => ResizeAxis::Columns,
                "rows" | "vertical" => ResizeAxis::Rows,
                "both" => ResizeAxis::Both,
                _ => return Err(MezError::invalid_args("percent size axis is invalid")),
            };
            Ok(PaneSizeSpec::Percent { percent, axis })
        }
        "delta" => {
            let direction = object
                .get("direction")
                .and_then(serde_json::Value::as_str)
                .and_then(ResizeDirection::from_name)
                .ok_or_else(|| MezError::invalid_args("delta size direction is invalid"))?;
            let amount = required_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Delta { direction, amount })
        }
        "edge" => {
            let edge = object
                .get("edge")
                .and_then(serde_json::Value::as_str)
                .and_then(ResizeDirection::from_name)
                .ok_or_else(|| MezError::invalid_args("edge size edge is invalid"))?;
            let amount = required_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Edge { edge, amount })
        }
        _ => Err(MezError::invalid_args("size mode is invalid")),
    }
}

/// Runs the optional size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_size_u16(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> Result<Option<u16>> {
    value
        .map(|value| required_size_u16(Some(value), field))
        .transpose()
}

/// Runs the required size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_size_u16(value: Option<&serde_json::Value>, field: &'static str) -> Result<u16> {
    let value = value
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args(format!("size {field} must be a number")))?;
    u16::try_from(value)
        .map_err(|_| MezError::invalid_args(format!("size {field} is out of range")))
}
