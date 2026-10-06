//! Consumed host invitation pairing through authenticated broker readiness.
//!
//! Only a protected file path and optional local alias cross IPC. The broker
//! reads proof, redeems once and privately publishes the issued credential.
//! A closed reply binds the exact handle and expected profile alias; failure or
//! cancellation consumes this stream without endpoint fallback or redemption
//! replay. Publication may have completed despite a lost reply: inspect the
//! protected profile before attempting recovery. No session is allocated here.

use super::*;

/// Closed publication evidence, never remote credentials or authority fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairReply {
    handle: FrontendHandle,
    paired: bool,
    profile: String,
}

impl OutboundFrontendClient {
    /// Sends one protected invitation reference and validates exact publication
    /// evidence. The expected alias comes from caller-validated invitation data,
    /// not a returned credential. Uncertain outcomes never return reusable state.
    pub(crate) async fn pair_invitation(
        mut self,
        path: &Path,
        save_as: Option<&str>,
        expected_alias: &str,
        budget: Duration,
    ) -> Result<()> {
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget)
            || !path.is_absolute()
            || path.as_os_str().len() > 2048
            || expected_alias.is_empty()
            || expected_alias.len() > 128
            || expected_alias.chars().any(char::is_control)
            || save_as.is_some_and(|alias| alias != expected_alias)
        {
            return Err(MezError::invalid_args(
                "outbound pairing budget, path or alias invalid",
            ));
        }
        let body = serde_json::json!({"operation":"pair","handle":self.handle,
            "path":path.to_str().ok_or_else(|| {
                MezError::invalid_args("outbound pairing path cannot be represented by the wire protocol")
            })?,"save_as":save_as})
        .to_string();
        if body.len() > HELLO_LIMIT {
            return Err(MezError::invalid_args(
                "outbound pairing request exceeds limit",
            ));
        }
        tokio::time::timeout(budget, async move {
            self.discovery.validate()?;
            self.stream
                .send(ProtocolFrame::new(CONTENT_TYPE, body))
                .await?;
            let frame = self.stream.next().await.transpose()?.ok_or_else(|| {
                MezError::invalid_state(
                    "outbound pairing reply unavailable; inspect profile before retrying",
                )
            })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound pairing reply type invalid; outcome unknown",
                ));
            }
            let reply: PairReply = serde_json::from_str(&frame.body).map_err(|_| {
                MezError::invalid_state("outbound pairing reply invalid; outcome unknown")
            })?;
            validate_reply(&reply, &self.handle, expected_alias)?;
            self.discovery.validate()?;
            Ok(())
        })
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound pairing timed out; inspect profile before retrying")
        })?
    }
}

/// Validates only the exact closed publication result, excluding raw peer proof.
fn validate_reply(reply: &PairReply, handle: &FrontendHandle, alias: &str) -> Result<()> {
    if reply.handle != *handle || !reply.paired || reply.profile != alias {
        return Err(MezError::invalid_state(
            "outbound pairing settlement changed; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
