//! Consumed administrative detach on one exactly initialized primary session.
//!
//! Only the explicit client target and original mutation key are forwarded. The
//! runtime still enforces attached-primary administrative authority; this API
//! cannot retarget a session, grant authority or replay an uncertain mutation.
//! Success and failure both dispose the administrative stream. Self-detach has
//! its separate target-free operation and is not broadened by this adapter.

use super::*;

/// Closed settlement binds the administrative owner as well as the target.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    handle: FrontendHandle,
    session: SessionSummary,
    idempotency_key: String,
    detached: bool,
    client_id: String,
}

impl OutboundSessionClient {
    /// Sends exactly one primary-authorized target detach, returning only after
    /// exact settlement validation. Cancellation or lost reply consumes ownership
    /// and never authorizes reconnect, retry or detaching a different client.
    pub(crate) async fn detach_target(
        mut self,
        target: &str,
        key: &str,
        budget: Duration,
    ) -> Result<()> {
        validate_budget(1, 1, budget)?;
        if self.summary.granted_role != "primary" {
            return Err(MezError::forbidden(
                "outbound target detach requires primary",
            ));
        }
        ClientId::parse('c', target.to_string())
            .ok_or_else(|| MezError::invalid_args("outbound target detach client invalid"))?;
        if target.len() > 128
            || key.is_empty()
            || key.len() > 128
            || key.chars().any(char::is_control)
        {
            return Err(MezError::invalid_args(
                "outbound target detach identity invalid",
            ));
        }
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            let body = serde_json::json!({"operation":"detach-target","handle":self.client.handle,
                "client_id":target,"idempotency_key":key})
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
                    MezError::invalid_state(
                        "outbound target detach reply unavailable; outcome unknown",
                    )
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound target detach reply type invalid; outcome unknown",
                ));
            }
            let reply: Reply = serde_json::from_str(&frame.body).map_err(|_| {
                MezError::invalid_state("outbound target detach reply invalid; outcome unknown")
            })?;
            validate_reply(&reply, &self.client.handle, &self.summary, target, key)?;
            self.client.discovery.validate()?;
            Ok(())
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound target detach timed out; outcome unknown"))?
    }
}

/// Does not expose a target settlement from a foreign administrative owner.
fn validate_reply(
    reply: &Reply,
    handle: &FrontendHandle,
    session: &SessionSummary,
    target: &str,
    key: &str,
) -> Result<()> {
    if reply.handle != *handle
        || reply.session != *session
        || reply.idempotency_key != key
        || !reply.detached
        || reply.client_id != target
    {
        return Err(MezError::invalid_state(
            "outbound target detach settlement changed; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
