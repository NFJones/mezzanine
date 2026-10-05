//! Consumed client input exchange on the pinned session, never automatic replay.
//!
//! Only an initialized primary may send bounded bytes with an explicit mutation
//! key. The returned acknowledgement means runtime acceptance, not physical input
//! delivery. Failure consumes stream ownership; the client cannot blindly retry
//! uncertain input on another connection or retarget its initialized session.

use super::*;

/// Closed runtime-acceptance evidence and truthful lifecycle flags.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputAcknowledgement {
    pub(crate) input_bytes: usize,
    pub(crate) client_detached: bool,
    pub(crate) session_terminated: bool,
}

/// Exact local reply envelope; arbitrary metadata and device proof reject.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputReply {
    handle: FrontendHandle,
    session: SessionSummary,
    idempotency_key: String,
    acknowledgement: InputAcknowledgement,
}

impl OutboundSessionClient {
    /// Sends one primary input mutation with the original key and geometry.
    /// Returns this same owner only after exact acknowledgement validation;
    /// cancellation or errors never replay input or yield a retryable stream.
    pub(crate) async fn step(
        mut self,
        columns: u16,
        rows: u16,
        input: &[u8],
        key: &str,
        deadline: Duration,
    ) -> Result<(Self, InputAcknowledgement)> {
        validate_budget(columns, rows, deadline)?;
        if self.summary.granted_role != "primary" {
            return Err(MezError::forbidden(
                "outbound input requires initialized primary",
            ));
        }
        if input.len() > 512
            || key.is_empty()
            || key.len() > 128
            || key.chars().any(char::is_control)
        {
            return Err(MezError::invalid_args("outbound input budget unavailable"));
        }
        let body = serde_json::json!({"operation":"step","handle":self.client.handle,
            "columns":columns,"rows":rows,"idempotency_key":key,"input_bytes":input})
        .to_string();
        if body.len() > HELLO_LIMIT {
            return Err(MezError::invalid_args(
                "outbound input request exceeds limit",
            ));
        }
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
                    MezError::invalid_state("outbound input reply unavailable; outcome unknown")
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound input reply content type unsupported; outcome unknown",
                ));
            }
            let reply: InputReply = serde_json::from_str(&frame.body).map_err(|_| {
                MezError::invalid_state("outbound input reply invalid; outcome unknown")
            })?;
            validate_reply(&reply, &self.client.handle, &self.summary, key, input.len())?;
            self.client.discovery.validate()?;
            Ok((self, reply.acknowledgement))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound input timed out; outcome unknown"))?
    }
}

/// Checks exact stream, settlement, mutation identity and accepted byte count.
fn validate_reply(
    reply: &InputReply,
    handle: &FrontendHandle,
    summary: &SessionSummary,
    key: &str,
    count: usize,
) -> Result<()> {
    if reply.handle != *handle
        || reply.session != *summary
        || reply.idempotency_key != key
        || reply.acknowledgement.input_bytes != count
    {
        return Err(MezError::invalid_state(
            "outbound input acknowledgement changed; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
