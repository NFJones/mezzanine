//! Event audience filtering, listing, and payload serialization.

use super::mcp::event_kind_name;
use super::*;

/// Runs the control event audience operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn control_event_audience(
    session: &Session,
    caller_client_id: &ClientId,
) -> Result<EventAudience> {
    let client = session
        .clients()
        .iter()
        .find(|client| client.id == *caller_client_id)
        .ok_or_else(|| MezError::forbidden("unknown control client"))?;
    match client.role {
        ClientRole::Primary => Ok(EventAudience::Primary),
        ClientRole::Observer => {
            let observer = session
                .observers()
                .iter()
                .find(|observer| observer.client_id == *caller_client_id)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "observer request not found",
                    )
                })?;
            Ok(EventAudience::ApprovedObserver {
                visible_from_event_id: observer.visible_from_event_id.unwrap_or(u64::MAX),
            })
        }
        ClientRole::PendingObserver => {
            let observer = session
                .observers()
                .iter()
                .find(|observer| observer.client_id == *caller_client_id)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "observer request not found",
                    )
                })?;
            Ok(EventAudience::PendingObserver {
                observer_request_id: observer.id.to_string(),
            })
        }
        ClientRole::Agent => Ok(EventAudience::Agent {
            agent_id: caller_client_id.to_string(),
        }),
        ClientRole::Automation => Ok(EventAudience::Automation),
    }
}

/// Carries Event List Params state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::control) struct EventListParams {
    /// Stores the after event id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::control) after_event_id: Option<u64>,
    /// Stores the limit value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::control) limit: Option<usize>,
}

/// Runs the dispatch event list request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn dispatch_event_list_request(
    request: &JsonRpcRequest,
    session: &Session,
    caller_client_id: &ClientId,
    event_log: &EventLog,
) -> Result<String> {
    let audience = control_event_audience(session, caller_client_id)?;
    let params = parse_event_list_params(request.params.as_deref())?;
    let effective_limit = params.limit.unwrap_or(MAX_EVENT_REPLAY_RETENTION);
    let mut events = if params.after_event_id.is_some() || params.limit.is_some() {
        event_log.replay_after_for(
            &audience,
            params.after_event_id.unwrap_or(0),
            effective_limit.saturating_add(1),
        )
    } else {
        event_log.replay_for(&audience)
    };
    let truncated = params.limit.is_some_and(|limit| events.len() > limit);
    if truncated {
        events.truncate(effective_limit);
    }
    Ok(format!(
        r#"{{"events":{},"latest_event_id":{},"retained_from_event_id":{},"replay_retention":{},"truncated":{}}}"#,
        events_json(events),
        event_log.latest_event_id(),
        event_log
            .first_retained_event_id()
            .map(|event_id| event_id.to_string())
            .unwrap_or_else(|| "null".to_string()),
        event_log.retention_limit(),
        truncated
    ))
}

/// Runs the parse event list params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn parse_event_list_params(params: Option<&str>) -> Result<EventListParams> {
    let Some(params) = params else {
        return Ok(EventListParams {
            after_event_id: None,
            limit: None,
        });
    };
    reject_unknown_json_fields(params, "event/list params", &["after_event_id", "limit"])?;
    let value = serde_json::from_str::<serde_json::Value>(params)
        .map_err(|_| MezError::invalid_args("event/list params must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("event/list params must be a JSON object"))?;
    let after_event_id = match object.get("after_event_id") {
        Some(value) => Some(value.as_u64().ok_or_else(|| {
            MezError::invalid_args("event/list after_event_id must be a non-negative integer")
        })?),
        None => None,
    };
    let limit = match object.get("limit") {
        Some(value) => {
            let limit = value.as_u64().ok_or_else(|| {
                MezError::invalid_args("event/list limit must be a non-negative integer")
            })?;
            let limit = usize::try_from(limit)
                .map_err(|_| MezError::invalid_args("event/list limit is too large"))?;
            if limit > MAX_EVENT_REPLAY_RETENTION {
                return Err(MezError::invalid_args(format!(
                    "event/list limit must be at most {MAX_EVENT_REPLAY_RETENTION}"
                )));
            }
            Some(limit)
        }
        None => None,
    };
    Ok(EventListParams {
        after_event_id,
        limit,
    })
}

/// Runs the events json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn events_json(events: Vec<VisibleEvent>) -> String {
    let encoded = events
        .iter()
        .map(|event| {
            format!(
                r#"{{"event_id":{},"time":"{}","event_type":"{}","kind":"{}","session_id":{},"object":{},"payload":"{}"}}"#,
                event.id,
                json_escape(&event.time),
                event_kind_name(event.kind),
                event_kind_name(event.kind),
                json_optional_string(event.session_id.as_deref()),
                event_payload_object_json(&event.payload),
                json_escape(&event.payload)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", encoded.join(","))
}

/// Runs the event payload object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn event_payload_object_json(payload: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) if value.is_object() => value.to_string(),
        _ => format!(r#"{{"content":"{}"}}"#, json_escape(payload)),
    }
}
