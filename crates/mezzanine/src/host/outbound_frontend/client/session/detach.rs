//! Consumed self-detach for one exact initialized primary frontend.
//!
//! No client/session target is caller-controlled. The original mutation key is
//! preserved on the retained stream, and successful settlement retires ownership.
//! Errors/cancellation may be ambiguous and never replay or reconnect. Explicit
//! terminal restoration remains the foreground caller's responsibility.

use super::*;

/// Closed self-detach reply excluding arbitrary remote state or credentials.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachReply {
    handle: FrontendHandle,
    session: SessionSummary,
    idempotency_key: String,
    detached: bool,
    client_id: String,
}

impl OutboundSessionClient {
    /// Sends one primary self-detach mutation, then disposes the client on either
    /// outcome. No target parameter or reusable detached owner is returned.
    pub(crate) async fn detach_self(mut self, key: &str, budget: Duration) -> Result<()> {
        validate_budget(1, 1, budget)?;
        if self.summary.granted_role != "primary" {
            return Err(MezError::forbidden("outbound self-detach requires primary"));
        }
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(MezError::invalid_args("outbound detach key invalid"));
        }
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            let body = serde_json::json!({"operation":"detach","handle":self.client.handle,
                "idempotency_key":key})
            .to_string();
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
                    MezError::invalid_state("outbound detach reply unavailable; outcome unknown")
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound detach reply type invalid; outcome unknown",
                ));
            }
            let reply: DetachReply = serde_json::from_str(&frame.body).map_err(|_| {
                MezError::invalid_state("outbound detach reply invalid; outcome unknown")
            })?;
            validate_reply(&reply, &self.client.handle, &self.summary, key)?;
            self.client.discovery.validate()?;
            Ok(())
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound detach timed out; outcome unknown"))?
    }
}

/// Requires matching stream, whole pinned session, original key and exact client.
fn validate_reply(
    reply: &DetachReply,
    handle: &FrontendHandle,
    session: &SessionSummary,
    key: &str,
) -> Result<()> {
    if reply.handle != *handle
        || reply.session != *session
        || reply.idempotency_key != key
        || !reply.detached
        || reply.client_id != session.client_id
    {
        return Err(MezError::invalid_state(
            "outbound detach settlement changed; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
