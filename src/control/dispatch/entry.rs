//! Control-dispatch entry points and compatibility context assembly.

use super::method_dispatch::{
    control_error_response, dispatch_control_request_internal, dispatch_parsed_request,
    dispatch_parsed_to_response, invalid_control_request_response,
};
use super::schema_validation::control_current_unix_seconds;
use super::{
    AgentShellStore, AgentTurnLedger, AuditLog, BlockedApprovalQueue, ClientId, ConfigLayer,
    ControlIdempotencyCache, EventLog, McpRegistry, MezError, PaneCaptureSource, Session,
    SnapshotRepository, TerminalDescriptor, approval_audit_record,
    approval_decide_scope_persistence, approval_json, approvals_json_for_params,
    authorize_control_request, config_audit_outcome, config_audit_plan, config_request_cache_key,
    config_response_advances_generation, dispatch_agent_list_with_store_and_model_profiles,
    dispatch_agent_shell_command_with_store, dispatch_agent_shell_visibility_with_store,
    dispatch_agent_task_list_with_ledger, dispatch_config_parsed_to_response_cached,
    dispatch_event_list_request, dispatch_pane_capture_request, dispatch_snapshot_request,
    dispatch_snapshot_request_with_context, error_code, is_config_control_method, json_rpc_error,
    json_rpc_success, json_string_field, mezzanine_error_code, parse_approval_decision,
    parse_json_rpc_request, require_idempotency_key, validate_control_method_params_schema,
};
use mez_mux::session::ClientTerminalDescriptor;
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
pub(super) fn client_terminal_descriptor_from_control(
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
