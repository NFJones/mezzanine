//! Consumed force-kill exchange using protected owner-side host authentication.
//!
//! Retains the caller's exact target and mutation key. Failed or cancelled
//! exchanges retire this local stream without replay or another endpoint. Host
//! permission enforcement is not replaced by local profile/display evidence.

use super::*;
use crate::host::outbound_frontend::killing::{
    KillSettlement, validate_request, validate_settlement,
};

/// Closed settlement reply without credentials or arbitrary host metadata.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KillReply {
    handle: FrontendHandle,
    host: super::listing::HostSummary,
    target: String,
    idempotency_key: String,
    settlement: KillSettlement,
}

impl OutboundFrontendClient {
    /// Sends one credential-free host-only setup and explicit kill request.
    /// Success returns only validated revocation evidence and retires ownership.
    pub(crate) async fn kill_session(
        mut self,
        profile: &str,
        target: &str,
        key: &str,
        budget: Duration,
    ) -> Result<KillSettlement> {
        validate_request(target, key)?;
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget)
            || profile.is_empty()
            || profile.len() > 128
            || profile.chars().any(char::is_control)
        {
            return Err(MezError::invalid_args(
                "outbound kill alias or budget invalid",
            ));
        }
        let setup = serde_json::json!({"handle":self.handle,"profile":profile,"initialize":{
            "client_name":"outbound-host-kill","requested_version":3,"requested_role":"observer","session_intent":"host_only"}}).to_string();
        let request = serde_json::json!({"operation":"kill","handle":self.handle,"target":target,"idempotency_key":key}).to_string();
        tokio::time::timeout(budget, async move {
            self.discovery.validate()?;
            self.stream
                .send(ProtocolFrame::new(CONTENT_TYPE, setup))
                .await?;
            *self.stream.codec_mut() = ProtocolFrameCodec::new(1024 * 1024)?;
            self.stream
                .send(ProtocolFrame::new(CONTENT_TYPE, request))
                .await?;
            let frame = self.stream.next().await.transpose()?.ok_or_else(|| {
                MezError::invalid_state("outbound kill reply unavailable; outcome unknown")
            })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound kill reply type invalid; outcome unknown",
                ));
            }
            let reply: KillReply = serde_json::from_str(&frame.body).map_err(|_| {
                MezError::invalid_state("outbound kill reply invalid; outcome unknown")
            })?;
            validate_reply(&reply, &self.handle, target, key)?;
            self.discovery.validate()?;
            Ok(reply.settlement)
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound kill timed out; outcome unknown"))?
    }
}

/// Requires exact stream, target, key and host-only settlement before exposing
/// revocation evidence. This does not authorize another mutation or retry.
fn validate_reply(
    reply: &KillReply,
    handle: &FrontendHandle,
    target: &str,
    key: &str,
) -> Result<()> {
    if reply.handle != *handle || reply.target != target || reply.idempotency_key != key {
        return Err(MezError::invalid_state(
            "outbound kill reply owner changed; outcome unknown",
        ));
    }
    reply.host.validate()?;
    validate_settlement(&reply.settlement, target)
}

#[cfg(test)]
mod tests;
