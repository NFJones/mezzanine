//! Consumed host-list client exchange over authenticated local readiness.
//!
//! Only a protected profile alias is supplied. The broker owns remote proof,
//! routing and the fixed read-only method. Errors consume the local stream;
//! no setup, mutation or terminal input is replayed. Returned summaries remain
//! inert and bounded, not authorization or raw host-response forwarding.

use super::*;
use crate::host::outbound_frontend::listing::{ListedSession, validate_sessions};

/// Closed host-only settlement facts, with no returned proof or remote payload.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostSummary {
    selected_version: u32,
    granted_role: String,
    host_only: bool,
}

impl HostSummary {
    /// Validates closed host-only facts without exposing remote credentials.
    pub(super) fn validate(&self) -> Result<()> {
        if self.selected_version != 3 || self.granted_role != "observer" || !self.host_only {
            return Err(MezError::forbidden("outbound host-only settlement changed"));
        }
        Ok(())
    }
}

/// Exact local listing reply tied to this authenticated frontend stream.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListReply {
    handle: FrontendHandle,
    host: HostSummary,
    sessions: Vec<ListedSession>,
}

impl OutboundFrontendClient {
    /// Consumes readiness once for credential-free host-only setup and listing.
    /// Success returns validated summaries and retires the management stream;
    /// siblings remain independently owned by the broker.
    pub(crate) async fn list_sessions(
        self,
        profile: &str,
        budget: Duration,
    ) -> Result<Vec<ListedSession>> {
        self.management_exchange(profile, budget, false).await
    }

    /// Authenticates the protected host profile without listing sessions or
    /// requiring list permission. Success retires only this management stream.
    pub(crate) async fn authenticate_profile(self, profile: &str, budget: Duration) -> Result<()> {
        let sessions = self.management_exchange(profile, budget, true).await?;
        if !sessions.is_empty() {
            return Err(MezError::invalid_state(
                "outbound health reply contains unexpected sessions",
            ));
        }
        Ok(())
    }

    /// Shares consumed host-only setup while keeping the operation vocabulary
    /// closed. Authentication-only mode performs no remote follow-up method.
    async fn management_exchange(
        mut self,
        profile: &str,
        budget: Duration,
        authentication_only: bool,
    ) -> Result<Vec<ListedSession>> {
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget)
            || profile.is_empty()
            || profile.len() > 128
            || profile.chars().any(char::is_control)
        {
            return Err(MezError::invalid_args(
                "outbound host-list budget or alias invalid",
            ));
        }
        let setup = serde_json::json!({"handle":self.handle,"profile":profile,"initialize":{
            "client_name":"outbound-host-list","requested_version":3,
            "requested_role":"observer","session_intent":"host_only"
        }})
        .to_string();
        tokio::time::timeout(budget, async move {
            self.discovery.validate()?;
            self.stream
                .send(ProtocolFrame::new(CONTENT_TYPE, setup))
                .await?;
            *self.stream.codec_mut() = ProtocolFrameCodec::new(1024 * 1024)?;
            self.stream
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({"handle":self.handle,"authentication_only":authentication_only}).to_string(),
                ))
                .await?;
            let frame =
                self.stream.next().await.transpose()?.ok_or_else(|| {
                    MezError::invalid_state("outbound host-list reply unavailable")
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound host-list reply type unsupported",
                ));
            }
            let reply: ListReply = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_state("outbound host-list reply invalid"))?;
            validate_reply(&reply, &self.handle)?;
            self.discovery.validate()?;
            Ok(reply.sessions)
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound host-list timed out"))?
    }
}

/// Requires exact stream ownership and observer host-only settlement before
/// exposing any bounded lease summaries.
fn validate_reply(reply: &ListReply, handle: &FrontendHandle) -> Result<()> {
    if reply.handle != *handle
        || reply.host.selected_version != 3
        || reply.host.granted_role != "observer"
        || !reply.host.host_only
    {
        return Err(MezError::forbidden(
            "outbound host-list owner or settlement changed",
        ));
    }
    validate_sessions(&reply.sessions)
}

#[cfg(test)]
mod tests;
