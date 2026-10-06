//! Consumed item polling on one pinned, explicitly requested clipboard session.
//!
//! Clipboard output is exposed only after bounded exact-owner UTF-8 completion.
//! The retained receiver preserves occurrence watermarks across polls. One total
//! deadline includes request, idle wait and every transfer frame; errors or
//! cancellation drop partial content and the stream rather than replaying effects.
//! This adapter neither applies clipboard policy nor writes a host clipboard.

use super::*;
use crate::host::terminal::wire_events::AttachRenderAction;

/// Validated item facts; sensitive content deliberately has no Debug formatter.
pub(crate) enum FrontendItem {
    /// Closed redraw facts, including a neutral idle or partial-effect reply.
    Redraw(AttachRenderAction, Option<u64>),
    /// Complete effect content, not independent permission to copy it locally.
    Clipboard(String),
}

/// Closed single-frame redraw response; transfer records have their own decoder.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RedrawReply {
    kind: String,
    handle: FrontendHandle,
    session: SessionSummary,
    action: String,
    event_id: Option<u64>,
}

impl OutboundSessionClient {
    /// Polls one admitted item, returning ownership only after full settlement.
    /// Redraw-only or observer clients cannot acquire clipboard access by polling.
    pub(crate) async fn poll_items(
        mut self,
        wait_ms: u64,
        budget: Duration,
    ) -> Result<(Self, FrontendItem)> {
        validate_budget(1, 1, budget)?;
        if self.clipboard_receiver.is_none() || self.summary.granted_role != "primary" {
            return Err(MezError::forbidden(
                "outbound clipboard items were not requested",
            ));
        }
        if !(1..=250).contains(&wait_ms) || Duration::from_millis(wait_ms) >= budget {
            return Err(MezError::invalid_args("outbound item wait unavailable"));
        }
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            self.client.stream.send(ProtocolFrame::new(CONTENT_TYPE,
                serde_json::json!({"operation":"items","handle":self.client.handle,"wait_ms":wait_ms}).to_string()
            )).await?;
            // At most begin + 32 chunks + commit. The decoder separately enforces
            // exact count, total bytes and occurrence identity before exposure.
            for index in 0..34 {
                let frame = self.client.stream.next().await.transpose()?
                    .ok_or_else(|| MezError::invalid_state("outbound item reply unavailable; effect outcome unknown"))?;
                if frame.content_type != CONTENT_TYPE {
                    return Err(MezError::invalid_state("outbound item reply type unsupported"));
                }
                let value: serde_json::Value = serde_json::from_str(&frame.body)
                    .map_err(|_| MezError::invalid_state("outbound item reply invalid"))?;
                if value.get("kind").and_then(serde_json::Value::as_str) == Some("redraw") {
                    if index != 0 {
                        return Err(MezError::invalid_state("outbound item transfer interrupted"));
                    }
                    let reply: RedrawReply = serde_json::from_value(value)
                        .map_err(|_| MezError::invalid_state("outbound item redraw invalid"))?;
                    let action = validate_redraw(&reply, &self.client.handle, &self.summary)?;
                    self.client.discovery.validate()?;
                    return Ok((self, FrontendItem::Redraw(action, reply.event_id)));
                }
                let receiver = self.clipboard_receiver.as_mut()
                    .ok_or_else(|| MezError::invalid_state("outbound clipboard receiver unavailable"))?;
                if let Some(content) = receiver.apply(&frame)? {
                    self.client.discovery.validate()?;
                    return Ok((self, FrontendItem::Clipboard(content)));
                }
            }
            Err(MezError::invalid_state("outbound item transfer exceeds frame count"))
        }).await.map_err(|_| MezError::invalid_state("outbound item exchange timed out; effect outcome unknown"))?
    }
}

/// Validates the exact pinned owner and closed vocabulary without payload errors.
fn validate_redraw(
    reply: &RedrawReply,
    handle: &FrontendHandle,
    session: &SessionSummary,
) -> Result<AttachRenderAction> {
    if reply.kind != "redraw" || reply.handle != *handle || reply.session != *session {
        return Err(MezError::conflict("outbound item reply owner changed"));
    }
    let action = AttachRenderAction::from_str(&reply.action)?;
    if action == AttachRenderAction::Disconnect {
        return Err(MezError::invalid_state(
            "outbound item stream disconnected; reattach required",
        ));
    }
    Ok(action)
}

#[cfg(test)]
mod tests;
