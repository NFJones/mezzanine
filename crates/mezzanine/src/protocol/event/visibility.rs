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
        (EventVisibility::AllPrimaries, EventAudience::AllPrimaries)
        | (EventVisibility::SessionView, EventAudience::AllPrimaries)
        | (EventVisibility::AllPrimaries, EventAudience::PrimaryClient(_))
        | (EventVisibility::SessionView, EventAudience::PrimaryClient(_))
        | (EventVisibility::SessionView, EventAudience::SessionView) => true,
        (
            EventVisibility::PrimaryClient(event_client_id),
            EventAudience::PrimaryClient(audience_client_id),
        ) => event_client_id == audience_client_id,
        (
            EventVisibility::SessionView,
            EventAudience::ApprovedObserver {
                visible_from_event_id,
            },
        ) => event.id >= *visible_from_event_id,
        (EventVisibility::Agent(event_agent), EventAudience::Agent { agent_id }) => {
            event_agent == agent_id
        }
        (EventVisibility::Automation, EventAudience::Automation) => true,
        _ => false,
    };

    if !include {
        return None;
    }

    Some(VisibleEvent {
        id: event.id,
        time: event.time.clone(),
        kind: event.kind,
        session_id: event.session_id.clone(),
        payload: event.payload.clone(),
    })
}
