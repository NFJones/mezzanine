//! Control Authz implementation.
//!
//! This module owns the control authz boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_CONTROL_METHODS, AUTOMATION_CONTROL_METHODS, ClientId, ClientRole, ClientState,
    JsonRpcRequest, MezError, OBSERVER_CONTROL_METHODS, PENDING_OBSERVER_CONTROL_METHODS, Result,
    Session, json_string_field,
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
    if !matches!(client.state, ClientState::Attached | ClientState::Pending) {
        return Err(MezError::forbidden("control client is not attached"));
    }
    match client.role {
        ClientRole::Primary => Ok(()),
        ClientRole::PendingObserver | ClientRole::Observer => {
            authorize_observer_method(session, caller_client_id, request)
        }
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
    session: &Session,
    caller_client_id: &ClientId,
    request: &JsonRpcRequest,
) -> Result<()> {
    let client = session
        .clients()
        .iter()
        .find(|client| client.id == *caller_client_id)
        .ok_or_else(|| MezError::forbidden("unknown observer client"))?;
    if client.role == ClientRole::PendingObserver {
        return authorize_pending_observer_method(session, caller_client_id, request);
    }
    if !OBSERVER_CONTROL_METHODS.contains(&request.method.as_str()) {
        return Err(MezError::forbidden(
            "observer clients are not authorized for this control method",
        ));
    }

    match request.method.as_str() {
        "control/initialize" | "control/shutdown" | "control/cancel" => Ok(()),
        "terminal/view" => Ok(()),
        "event/list" if client.role == ClientRole::Observer => Ok(()),
        "session/attach" => {
            let Some(params) = request.params.as_deref() else {
                return Err(MezError::invalid_args(
                    "session/attach requires a params object",
                ));
            };
            let role = json_string_field(params, "role").unwrap_or_else(|| "primary".to_string());
            if role == "observer" {
                Ok(())
            } else {
                Err(MezError::forbidden(
                    "observer clients may attach only as observers",
                ))
            }
        }
        "observer/inspect" => {
            let Some(params) = request.params.as_deref() else {
                return Ok(());
            };
            let Some(observer_id) = json_string_field(params, "observer_request_id") else {
                return Ok(());
            };
            let owns_request = session.observers().iter().any(|observer| {
                observer.id.as_str() == observer_id && observer.client_id == *caller_client_id
            });
            if owns_request {
                Ok(())
            } else {
                Err(MezError::forbidden(
                    "observer clients may inspect only their own observer request",
                ))
            }
        }
        _ => Err(MezError::forbidden(
            "observer clients are not authorized for this control method",
        )),
    }
}

/// Runs the authorize pending observer method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn authorize_pending_observer_method(
    session: &Session,
    caller_client_id: &ClientId,
    request: &JsonRpcRequest,
) -> Result<()> {
    if !PENDING_OBSERVER_CONTROL_METHODS.contains(&request.method.as_str()) {
        return Err(MezError::forbidden(
            "pending observer clients are not authorized for this control method",
        ));
    }

    match request.method.as_str() {
        "control/initialize" | "control/shutdown" | "control/cancel" => Ok(()),
        "session/attach" => {
            let Some(params) = request.params.as_deref() else {
                return Err(MezError::invalid_args(
                    "session/attach requires a params object",
                ));
            };
            let role = json_string_field(params, "role").unwrap_or_else(|| "primary".to_string());
            if role == "observer" {
                Ok(())
            } else {
                Err(MezError::forbidden(
                    "pending observer clients may attach only as observers",
                ))
            }
        }
        "observer/inspect" => {
            let Some(params) = request.params.as_deref() else {
                return Ok(());
            };
            let Some(observer_id) = json_string_field(params, "observer_request_id") else {
                return Ok(());
            };
            let owns_request = session.observers().iter().any(|observer| {
                observer.id.as_str() == observer_id && observer.client_id == *caller_client_id
            });
            if owns_request {
                Ok(())
            } else {
                Err(MezError::forbidden(
                    "pending observer clients may inspect only their own observer request",
                ))
            }
        }
        _ => Err(MezError::forbidden(
            "pending observer clients are not authorized for this control method",
        )),
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
