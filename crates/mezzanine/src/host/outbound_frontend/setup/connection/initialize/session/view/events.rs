//! Bounded requested event delivery, not unsolicited control-stream multiplexing.
//!
//! Exact frontend ownership and version-one negotiation precede any event read.
//! Idle timeout retains the incremental reader; EOF/errors consume the session.
//! At most 64 classified events coalesce into one content-free reply. No tasks,
//! reconnects, input, clipboard payloads or presentation acknowledgements occur.

use super::*;
use crate::host::terminal::wire_events::AttachRenderAction;
use futures_util::FutureExt;

/// Closed request for a finite idle wait on the retained event reader.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventRequest {
    operation: String,
    handle: FrontendHandle,
    wait_ms: u64,
}

/// Delivers one finite classified burst under the enclosing exchange deadline.
/// Failed or cancelled reply delivery consumes the exact session without replay.
pub(super) async fn deliver_events(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: EventRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound event request invalid"))?;
    validate_request(&request, &session.connected.prepared.frontend.handle)?;
    if session.events.is_none() {
        return Err(MezError::invalid_state(
            "outbound session did not negotiate events",
        ));
    }
    let mut action = AttachRenderAction::None;
    let mut event_id = None;
    if let Ok(event) =
        tokio::time::timeout(Duration::from_millis(request.wait_ms), session.next_event()).await
    {
        let event = event?.ok_or_else(|| {
            MezError::invalid_state("outbound event stream ended; reattach required")
        })?;
        action = event.0;
        event_id = event.1;
        for _ in 1..64 {
            let Some(event) = session.next_event().now_or_never() else {
                break;
            };
            let event = event?.ok_or_else(|| {
                MezError::invalid_state("outbound event stream ended; reattach required")
            })?;
            combine(&mut action, &mut event_id, event);
        }
    }
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let reply = serde_json::json!({"handle":request.handle,"session":session.summary,
        "action":action.as_str(),"event_id":event_id})
    .to_string();
    session
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, reply)))
        .await
        .map_err(|_| MezError::invalid_state("outbound event reply unavailable"))?;
    Ok(session)
}

/// Rejects foreign handles and unsupported waits before consuming event bytes.
fn validate_request(request: &EventRequest, handle: &FrontendHandle) -> Result<()> {
    if request.operation != "events" || request.handle != *handle {
        return Err(MezError::conflict("outbound event request owner changed"));
    }
    if !(1..=250).contains(&request.wait_ms) {
        return Err(MezError::invalid_args("outbound event wait unavailable"));
    }
    Ok(())
}

/// Preserves strongest redraw and optional maximum identity. An unidentified
/// event makes the burst cutoff unknown, matching existing attach coalescing.
fn combine(
    action: &mut AttachRenderAction,
    id: &mut Option<u64>,
    event: (AttachRenderAction, Option<u64>),
) {
    *action = action.combine(event.0);
    *id = match (*id, event.1) {
        (Some(left), Some(right)) => Some(left.max(right)),
        _ => None,
    };
}

#[cfg(test)]
mod tests;
