//! One bounded item exchange on an explicitly admitted clipboard session.
//!
//! Only the exact retained frontend can consume its next item. Clipboard effects
//! are encoded lazily as bounded begin/chunk/commit records; ordinary and partial
//! events return content-free redraw facts. The enclosing consumed exchange owns
//! the deadline and failure disposal. No remote request, host clipboard write,
//! reconnect, effect replay or unsolicited reply multiplexing is introduced.

use super::*;
use crate::host::outbound_frontend::clipboard_wire::ClipboardFrames;
use crate::host::outbound_frontend::events::OutboundEventItem;

/// Closed finite-wait request; callers cannot select content or another client.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemRequest {
    operation: String,
    handle: FrontendHandle,
    wait_ms: u64,
}

/// Consumes at most one item and returns the owner only after complete delivery.
/// Ambiguous delivery failure disposes the exact connection rather than replaying.
pub(super) async fn deliver_item(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: ItemRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound item request invalid"))?;
    validate_request(
        &request,
        &session.connected.prepared.frontend.handle,
        session.clipboard_enabled,
    )?;
    let item = match tokio::time::timeout(
        Duration::from_millis(request.wait_ms),
        session.next_event_item(),
    )
    .await
    {
        Ok(result) => result?.ok_or_else(|| {
            MezError::invalid_state("outbound item stream ended; reattach required")
        })?,
        Err(_) => OutboundEventItem::Redraw(
            crate::host::terminal::wire_events::AttachRenderAction::None,
            None,
        ),
    };
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    match item {
        OutboundEventItem::Redraw(action, id) => {
            let reply = serde_json::json!({"kind":"redraw","handle":request.handle,
                "session":session.summary,"action":action.as_str(),"event_id":id})
            .to_string();
            write_frame(&mut session, ProtocolFrame::new(CONTENT_TYPE, reply)).await?;
        }
        OutboundEventItem::Clipboard(content) => {
            session.clipboard_transfer =
                session.clipboard_transfer.checked_add(1).ok_or_else(|| {
                    MezError::invalid_state("outbound clipboard occurrence exhausted")
                })?;
            let summary: crate::host::outbound_frontend::client::SessionSummary =
                serde_json::from_value(session.summary.clone())
                    .map_err(|_| MezError::invalid_state("outbound retained session invalid"))?;
            for frame in ClipboardFrames::new(
                &request.handle,
                &summary,
                session.clipboard_transfer,
                &content,
            )? {
                session
                    .connected
                    .prepared
                    .frontend
                    ._endpoint
                    .frontend_config_root()?;
                write_frame(&mut session, frame?).await?;
            }
        }
    }
    Ok(session)
}

/// Preserves the shared body-size limit before a bounded local write.
async fn write_frame(session: &mut InitializedSessionFrontend, frame: ProtocolFrame) -> Result<()> {
    if frame.body.len() > BODY_LIMIT {
        return Err(MezError::invalid_state("outbound item reply exceeds limit"));
    }
    session
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&frame))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound item delivery unavailable; effect outcome unknown")
        })
}

/// Requires exact frontend, prior capability admission and a finite idle wait.
fn validate_request(request: &ItemRequest, handle: &FrontendHandle, enabled: bool) -> Result<()> {
    if request.operation != "items" || request.handle != *handle {
        return Err(MezError::conflict("outbound item request owner changed"));
    }
    if !enabled {
        return Err(MezError::forbidden(
            "outbound clipboard items were not negotiated",
        ));
    }
    if !(1..=250).contains(&request.wait_ms) {
        return Err(MezError::invalid_args("outbound item wait unavailable"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
