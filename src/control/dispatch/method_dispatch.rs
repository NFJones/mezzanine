//! Parsed control-method execution and JSON-RPC response projection.

use super::connection::{ControlConnectionState, initialize_control_connection};
use super::schema_validation::{
    control_current_unix_seconds, control_pane_size_spec, dispatch_session_attach_parsed,
    reject_runtime_required_creation_fields, validate_control_method_params_schema,
};
use super::{
    BlockedApprovalQueue, ClientId, ControlDispatchKind, EventLog, JsonRpcRequest,
    MAX_EVENT_REPLAY_RETENTION, McpRegistry, MezError, Result, Session, SplitDirection,
    agent_shell_command_response_json, agent_state_json, agents_json_for_params,
    approval_decide_scope_persistence, approval_json, approvals_json_for_params,
    clients_json_for_params, control_method_spec, destination_target_checked_resolved,
    dispatch_config_parsed_request, dispatch_event_list_request, dispatch_pane_capture_request,
    error_code, frame_read_json, initialize_result_json, json_bool_field, json_escape,
    json_object_field, json_raw_field, json_rpc_error, json_rpc_success, json_string_field,
    layout_state_json, mcp_servers_json, mcp_tools_json, mezzanine_error_code,
    nullable_state_request_session_target_matches, observer_json, observers_json_for_params,
    pane_by_id, pane_state_json, pane_target_checked_resolved, panes_json_for_params,
    parse_approval_decision, parse_join_position, parse_json_rpc_request, parse_split_direction,
    parse_trust_decision, project_trust_state_filter_from_params, require_idempotency_key,
    require_session_target_matches, session_state_json_for_params, session_summary_json,
    source_pane_target_checked_resolved, target_or_active_pane, validate_agent_task_list_params,
    window_by_id, window_id_for_target, window_state_json, window_target_checked_resolved,
    windows_json_for_params,
};
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
pub(super) fn control_result_response(request: &JsonRpcRequest, result: Result<String>) -> String {
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => control_error_response(request, &error),
    }
}

/// Converts one parsed control error into a JSON-RPC error envelope.
pub(super) fn control_error_response(request: &JsonRpcRequest, error: &MezError) -> String {
    json_rpc_error(
        &request.id,
        error_code(error.kind()),
        error.message(),
        mezzanine_error_code(error.kind()),
    )
}

/// Converts a malformed control envelope into the standard invalid request response.
pub(super) fn invalid_control_request_response(error: &MezError) -> String {
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
