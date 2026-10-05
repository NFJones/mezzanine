//! Consumed exact-session health exchange over the retained authenticated IPC.
//!
//! A closed reply contains coarse connection-local observations, not path data,
//! credentials, remote authority or a promise of future availability. Errors
//! consume the stream without replay. Sampling does not render or acknowledge.

use super::*;
use crate::host::terminal::TerminalIrohStatusQuality;

/// Closed coarse connection facts; unknown fields cannot carry private metrics.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthReply {
    handle: FrontendHandle,
    session: SessionSummary,
    connected: bool,
    quality: String,
}

impl OutboundSessionClient {
    /// Requests one sample of this session's retained connection and returns
    /// ownership only after exact reply validation. No terminal write is implied.
    pub(crate) async fn sample_transport_health(
        mut self,
        budget: Duration,
    ) -> Result<(Self, bool, TerminalIrohStatusQuality)> {
        validate_budget(1, 1, budget)?;
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            let body =
                serde_json::json!({"operation":"health","handle":self.client.handle}).to_string();
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
                .ok_or_else(|| MezError::invalid_state("outbound health reply unavailable"))?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound health reply type unsupported",
                ));
            }
            let reply: HealthReply = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_state("outbound health reply invalid"))?;
            let quality = validate_reply(&reply, &self.client.handle, &self.summary)?;
            self.client.discovery.validate()?;
            Ok((self, reply.connected, quality))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound health exchange timed out"))?
    }
}

/// Binds observations to the exact owner and rejects invented quality vocabulary
/// or an inconsistent disconnected sample. This grants no mutation authority.
fn validate_reply(
    reply: &HealthReply,
    handle: &FrontendHandle,
    summary: &SessionSummary,
) -> Result<TerminalIrohStatusQuality> {
    if reply.handle != *handle || reply.session != *summary {
        return Err(MezError::conflict("outbound health reply owner changed"));
    }
    let quality = match reply.quality.as_str() {
        "good" => TerminalIrohStatusQuality::Good,
        "degraded" => TerminalIrohStatusQuality::Degraded,
        "poor" => TerminalIrohStatusQuality::Poor,
        "unknown" => TerminalIrohStatusQuality::Unknown,
        _ => {
            return Err(MezError::invalid_state(
                "outbound health quality unsupported",
            ));
        }
    };
    if !reply.connected && quality != TerminalIrohStatusQuality::Unknown {
        return Err(MezError::invalid_state(
            "outbound disconnected health must be unknown",
        ));
    }
    Ok(quality)
}

#[cfg(test)]
mod tests;
