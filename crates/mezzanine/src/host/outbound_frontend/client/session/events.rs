//! Consumed client event polling on one pinned initialized session.
//!
//! No unsolicited frames interleave with mutation replies. One bounded request
//! returns only a closed redraw action and optional event identity. Failure or
//! cancellation consumes stream ownership, never replaying setup or events.

use super::*;
use crate::host::terminal::wire_events::AttachRenderAction;

/// Closed event reply; unknown metadata cannot carry event content or authority.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventReply {
    handle: FrontendHandle,
    session: SessionSummary,
    action: String,
    event_id: Option<u64>,
}

impl OutboundSessionClient {
    /// Consumes one finite event poll, returning the unchanged owner only after
    /// exact reply validation. Idle replies retain reader state inside the broker.
    pub(crate) async fn poll_events(
        mut self,
        wait_ms: u64,
        budget: Duration,
    ) -> Result<(Self, AttachRenderAction, Option<u64>)> {
        validate_budget(1, 1, budget)?;
        if !(1..=250).contains(&wait_ms) || Duration::from_millis(wait_ms) >= budget {
            return Err(MezError::invalid_args("outbound event wait unavailable"));
        }
        let body =
            serde_json::json!({"operation":"events","handle":self.client.handle,"wait_ms":wait_ms})
                .to_string();
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            self.client
                .stream
                .send(ProtocolFrame::new(CONTENT_TYPE, body))
                .await?;
            let frame = self
                .client
                .stream
                .next()
                .await
                .transpose()?
                .ok_or_else(|| MezError::invalid_state("outbound event reply unavailable"))?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound event reply type unsupported",
                ));
            }
            let reply: EventReply = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_state("outbound event reply invalid"))?;
            let action = validate_reply(&reply, &self.client.handle, &self.summary)?;
            self.client.discovery.validate()?;
            if action == AttachRenderAction::Disconnect {
                return Err(MezError::invalid_state(
                    "outbound event stream disconnected; reattach required",
                ));
            }
            Ok((self, action, reply.event_id))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound event poll timed out"))?
    }
}

/// Requires the exact local handle and full pinned settlement before accepting
/// the closed redraw vocabulary; display facts never become input authority.
fn validate_reply(
    reply: &EventReply,
    handle: &FrontendHandle,
    summary: &SessionSummary,
) -> Result<AttachRenderAction> {
    if reply.handle != *handle || reply.session != *summary {
        return Err(MezError::conflict("outbound event reply owner changed"));
    }
    AttachRenderAction::from_str(&reply.action)
}

#[cfg(test)]
mod tests;
