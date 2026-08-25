//! Control Authz implementation.
//!
//! This module owns the control authz boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_CONTROL_METHODS, AUTOMATION_CONTROL_METHODS, ClientId, ClientRole, ClientState,
    JsonRpcRequest, MezError, OBSERVER_CONTROL_METHODS, Result, Session, json_string_field,
};

// Control role and method authorization.

/// Runs the authorize control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn authorize_control_request(
    session: &Session,
    caller_client_id: &ClientId,
    request: &JsonRpcRequest,
) -> Result<()> {
    let client = session
        .clients()
        .iter()
        .find(|client| client.id == *caller_client_id)
        .ok_or_else(|| MezError::forbidden("unknown control client"))?;
    if client.state != ClientState::Attached {
        return Err(MezError::forbidden("control client is not attached"));
    }
    match client.role {
        ClientRole::Primary => Ok(()),
        ClientRole::Observer => authorize_observer_method(session, caller_client_id, request),
        ClientRole::Agent => authorize_agent_method(request),
        ClientRole::Automation => authorize_automation_method(request),
    }
}

/// Runs the authorize observer method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn authorize_observer_method(
    _session: &Session,
    caller_client_id: &ClientId,
    request: &JsonRpcRequest,
) -> Result<()> {
    if !OBSERVER_CONTROL_METHODS.contains(&request.method.as_str()) {
        return Err(MezError::forbidden(
            "observer clients are not authorized for this control method",
        ));
    }

    match request.method.as_str() {
        "control/initialize" | "control/shutdown" | "control/cancel" => Ok(()),
        "client/detach" => authorize_client_self_detach(caller_client_id, request),
        "terminal/view" => Ok(()),
        "event/list" => Ok(()),
        _ => Err(MezError::forbidden(
            "observer clients are not authorized for this control method",
        )),
    }
}

/// Allows an observer to detach only its own authenticated session client.
fn authorize_client_self_detach(
    caller_client_id: &ClientId,
    request: &JsonRpcRequest,
) -> Result<()> {
    let requested_client_id = request
        .params
        .as_deref()
        .and_then(|params| json_string_field(params, "client_id"));
    if requested_client_id
        .as_deref()
        .is_none_or(|id| id == caller_client_id.as_str())
    {
        Ok(())
    } else {
        Err(MezError::forbidden(
            "observer clients may detach only themselves",
        ))
    }
}

/// Runs the authorize agent method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn authorize_agent_method(request: &JsonRpcRequest) -> Result<()> {
    if AGENT_CONTROL_METHODS.contains(&request.method.as_str()) {
        Ok(())
    } else {
        Err(MezError::forbidden(
            "agent clients are not authorized for this control method",
        ))
    }
}

/// Runs the authorize automation method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn authorize_automation_method(request: &JsonRpcRequest) -> Result<()> {
    if AUTOMATION_CONTROL_METHODS.contains(&request.method.as_str()) {
        Ok(())
    } else {
        Err(MezError::forbidden(
            "automation clients are not authorized for this control method",
        ))
    }
}

/// Runs the require idempotency key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn require_idempotency_key(params: &str) -> Result<()> {
    if json_string_field(params, "idempotency_key").is_some() {
        Ok(())
    } else {
        Err(MezError::invalid_args(
            "mutating control method requires idempotency_key",
        ))
    }
}
