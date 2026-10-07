//! Strict content-free private OpenCode observation framing.
//! Root identity is a proposal from a launcher-owned authenticated descriptor,
//! never permission to mint authority or read transcripts. Typed nested fields
//! reject content/paths, duplicate keys, unsafe counters and unrelated sessions.

use super::{opencode, pi_binding};
use crate::error::{MezError, Result};
use serde::{Deserialize, Serialize};

/// One allowlisted child observation. No raw vendor event survives decoding.
#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum Observation {
    #[serde(rename = "start")]
    Start { session: String },
    #[serde(rename = "status")]
    Status { session: String, state: String },
    #[serde(rename = "usage")]
    Usage { session: String, message: Message },
    #[serde(rename = "unavailable")]
    Unavailable { session: String, reason: LossReason },
}

/// Only known producer resource loss crosses the private observation wire.
#[derive(Deserialize)]
pub(crate) enum LossReason {
    #[serde(rename = "wait-capacity")]
    WaitCapacity,
}

/// Strict nested normalized upstream message, not arbitrary JSON or content.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Message {
    role: String,
    #[serde(rename = "sessionID")]
    session: String,
    id: String,
    #[serde(rename = "providerID")]
    provider: String,
    #[serde(rename = "modelID")]
    model: String,
    time: Time,
    tokens: Tokens,
}
/// Only completion time is exported by the producer.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Time {
    completed: u64,
}
/// Evidenced disjoint upstream categories, all checked by the shared normalizer.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Tokens {
    input: u64,
    output: u64,
    reasoning: u64,
    cache: Cache,
}
/// Cache categories are input subsets, never separate expenses.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    read: u64,
    write: u64,
}

/// Finite observations share the established inherited-frame deadline boundary.
pub(crate) struct Reader(tokio::net::UnixStream);
impl Reader {
    /// Authenticates the inherited endpoint before accepting any proposal.
    pub(crate) fn new(stream: tokio::net::UnixStream) -> Result<Self> {
        use std::os::fd::AsRawFd;
        crate::runtime::authenticated_unix_peer_uid(
            stream.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        )
        .map_err(|_| unavailable())?;
        Ok(Self(stream))
    }
    /// Reads one bounded strict observation; idle silence differs from EOF.
    pub(crate) async fn next(&mut self) -> Result<Option<Observation>> {
        let Some(bytes) = pi_binding::read_frame(&mut self.0, 8192).await? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| unavailable())
    }
}
/// Payload-free failures cannot expose vendor data or session paths.
fn unavailable() -> MezError {
    MezError::invalid_state("OpenCode observation unavailable")
}
impl Observation {
    /// Requires a bounded inert vendor session, independently of actor authority.
    pub(crate) fn session(&self) -> Result<&str> {
        let value = match self {
            Self::Start { session }
            | Self::Status { session, .. }
            | Self::Usage { session, .. }
            | Self::Unavailable { session, .. } => session,
        };
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
        {
            return Err(unavailable());
        }
        Ok(value)
    }
}
impl Message {
    /// Produces immutable checked expense evidence only after the launch cutoff.
    /// Historical completed snapshots cannot recharge under a fresh owner.
    pub(crate) fn report(
        &self,
        session: &str,
        cutoff_ms: u64,
    ) -> Result<Option<crate::storage::token_usage::ExternalUsageReport>> {
        if self.session != session || self.time.completed < cutoff_ms {
            return Ok(None);
        }
        let bytes = serde_json::to_vec(self).map_err(|_| unavailable())?;
        opencode::completed_report("best-effort", session, "parent-observation", None, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Strict observation envelopes omit duplicate/extra fields and preserve
    /// launch-time historical exclusion plus stable replay accounting identity.
    #[test]
    fn opencode_stream_rejects_content_and_excludes_historical_replay() {
        let valid=br#"{"session":"bound","kind":"usage","message":{"role":"assistant","sessionID":"bound","id":"message","providerID":"provider","modelID":"model","time":{"completed":1000},"tokens":{"input":10,"output":4,"reasoning":2,"cache":{"read":3,"write":5}}}}"#;
        let Observation::Usage { message, .. } = serde_json::from_slice(valid).unwrap() else {
            panic!("usage");
        };
        assert!(message.report("bound", 1001).unwrap().is_none());
        assert!(message.report("foreign", 0).unwrap().is_none());
        let report = message.report("bound", 1000).unwrap().unwrap();
        assert_eq!(report.counters.input_tokens, 18);
        assert_eq!(report.counters.output_tokens, 6);
        for bytes in [
            br#"{"session":"bound","kind":"start","prompt":"PRIVATE"}"#.as_slice(),
            br#"{"session":"bound","session":"other","kind":"start"}"#,
            br#"{"session":"bound","kind":"unavailable","reason":"PRIVATE"}"#,
        ] {
            assert!(serde_json::from_slice::<Observation>(bytes).is_err());
        }
    }
}
