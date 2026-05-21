//! Audience-specific event projection.
//!
//! Visibility is kept separate from retention so replay policy can evolve
//! without changing append semantics or notification encoding.

use super::types::{EventAudience, EventVisibility, MezzanineEvent, VisibleEvent};

/// Runs the visible event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn visible_event(
    event: &MezzanineEvent,
    audience: &EventAudience,
) -> Option<VisibleEvent> {
    let include = match (&event.visibility, audience) {
        (_, EventAudience::Primary) => true,
        (
            EventVisibility::SessionView,
            EventAudience::ApprovedObserver {
                visible_from_event_id,
            },
        ) => event.id >= *visible_from_event_id,
        (
            EventVisibility::PendingObserverRequest(event_observer),
            EventAudience::PendingObserver {
                observer_request_id,
            },
        ) => event_observer == observer_request_id,
        (EventVisibility::Agent(event_agent), EventAudience::Agent { agent_id }) => {
            event_agent == agent_id
        }
        (EventVisibility::Automation, EventAudience::Automation) => true,
        _ => false,
    };

    if !include {
        return None;
    }

    let session_id = match audience {
        EventAudience::PendingObserver { .. } => None,
        _ => event.session_id.clone(),
    };

    Some(VisibleEvent {
        id: event.id,
        time: event.time.clone(),
        kind: event.kind,
        session_id,
        payload: event.payload.clone(),
    })
}
