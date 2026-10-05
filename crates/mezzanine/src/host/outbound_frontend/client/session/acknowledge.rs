//! Explicit client receipt acknowledgement after a renderer commits its output.
//!
//! This API never infers commitment from receiving or decoding a snapshot.
//! Callers supply a mutation key only after successful terminal output. The
//! consumed owner validates exact handle/session/key/IDs and never retries an
//! uncertain exchange. No receipts means no mutation or local wire traffic.

use super::*;

/// Closed acknowledgement reply; arbitrary metadata cannot acquire authority.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgementReply {
    handle: FrontendHandle,
    session: SessionSummary,
    idempotency_key: String,
    presentation_ids: Vec<u64>,
    acknowledged: bool,
}

impl OutboundSessionClient {
    /// Acknowledges only the last snapshot's receipts after caller-proven output
    /// commitment. Receiving a snapshot never invokes this method automatically.
    /// Failure consumes the owner; false preserves receipts without inventing ACK.
    pub(crate) async fn acknowledge_presented(
        mut self,
        key: &str,
        deadline: Duration,
    ) -> Result<(Self, bool)> {
        validate_budget(1, 1, deadline)?;
        if self.receipts.is_empty() {
            return Ok((self, true));
        }
        crate::host::terminal::wire_receipts::validate_receipts(&self.receipts)?;
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(MezError::invalid_args("outbound presentation key invalid"));
        }
        let body = serde_json::json!({"operation":"acknowledge","handle":self.client.handle,
            "idempotency_key":key,"presentation_ids":self.receipts})
        .to_string();
        tokio::time::timeout(deadline, async move {
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
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "outbound presentation reply unavailable; outcome unknown",
                    )
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound presentation reply type invalid; outcome unknown",
                ));
            }
            let reply: AcknowledgementReply = serde_json::from_str(&frame.body).map_err(|_| {
                MezError::invalid_state("outbound presentation reply invalid; outcome unknown")
            })?;
            validate_reply(
                &reply,
                &self.client.handle,
                &self.summary,
                key,
                &self.receipts,
            )?;
            self.client.discovery.validate()?;
            if reply.acknowledged {
                self.receipts.clear();
            }
            Ok((self, reply.acknowledged))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound presentation timed out; outcome unknown"))?
    }
}

/// Requires every immutable owner field to match before exposing settlement.
fn validate_reply(
    reply: &AcknowledgementReply,
    handle: &FrontendHandle,
    summary: &SessionSummary,
    key: &str,
    ids: &[u64],
) -> Result<()> {
    if reply.handle != *handle
        || reply.session != *summary
        || reply.idempotency_key != key
        || reply.presentation_ids != ids
    {
        return Err(MezError::invalid_state(
            "outbound presentation reply owner changed; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
