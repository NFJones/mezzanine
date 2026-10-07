//! Capability-only Pi lifecycle delivery over authenticated same-user Unix IPC.
//!
//! A launcher must supply its privately obtained exact-session capability. This
//! adapter cannot mint credentials, initialize a primary, choose a pane, rebind a
//! session or retry work. One total deadline covers connect/write/read; strict
//! framing and typed acknowledgments gate settlement. Errors never echo peer
//! payloads, tokens or paths. Installation and vendor conformance remain separate.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

use super::pi_owner::{Delivery, LifecycleOwner, Operation};
use crate::error::{MezError, Result};

/// Separate finite body/header budgets for one restricted exchange.
const MAX_BODY: usize = 64 * 1024;
/// One deadline, not a new budget for each transport phase.
const DEADLINE: Duration = Duration::from_millis(500);

/// Private immutable routing and launch authority, never Debug or serialized.
pub(crate) struct CapabilityTransport {
    socket: PathBuf,
    token: SecretString,
    generation: u64,
    session: String,
    owner: String,
}

/// A validated lease acknowledgment, not proof of vendor process liveness.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LeaseAck {
    pub(crate) agent_id: String,
    pub(crate) expires_at: u64,
}

/// Fixed RPC request; borrowed credential avoids an intermediate JSON token copy.
#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    id: &'static str,
    method: &'static str,
    params: Params<'a>,
}

/// Exact allowlisted fields; caller data cannot override authority or methods.
#[derive(Serialize)]
struct Params<'a> {
    launch_token: &'a str,
    generation: u64,
    external_session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'a str>,
}

/// Rejects ambiguous error/result envelopes and wrong reply ownership.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    jsonrpc: String,
    id: String,
    result: serde_json::Value,
}

/// Generic failure deliberately excludes arbitrary peer and credential content.
fn unavailable() -> MezError {
    MezError::invalid_state("Pi lifecycle delivery unavailable")
}

/// Inert metadata bounds match the server without exposing failed values.
fn inert(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 128
        && !value.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

impl CapabilityTransport {
    /// Accepts only explicit routing and already-issued authority. No discovery,
    /// filesystem reads, connection or registration occurs during construction.
    pub(crate) fn new(
        socket: &Path,
        token: SecretString,
        generation: u64,
        owner: &LifecycleOwner,
    ) -> Result<Self> {
        let (session, incarnation) = owner.transport_binding();
        let raw = token.expose_secret();
        if !socket.is_absolute()
            || raw.len() != 43
            || !raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || generation == 0
            || !inert(session)
        {
            return Err(unavailable());
        }
        Ok(Self {
            socket: socket.into(),
            token,
            generation,
            session: session.into(),
            owner: incarnation.into(),
        })
    }

    /// Registers fixed inert display metadata; immutable registration replay is
    /// server-owned. A lost reply returns failure, never invents a fresh identity.
    pub(crate) async fn register(&self, name: &str) -> Result<LeaseAck> {
        if !inert(name) {
            return Err(unavailable());
        }
        let result = self
            .exchange("agent/external/register", Some(name), None)
            .await?;
        if result
            .get("registered")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err(unavailable());
        }
        self.lease_ack(&result)
    }

    /// Renews only the exact bound session. Scheduling and expiry ownership stay
    /// with the launcher; this method neither starts timers nor retries failures.
    pub(crate) async fn renew(&self) -> Result<LeaseAck> {
        let result = self.exchange("agent/external/renew", None, None).await?;
        self.lease_ack(&result)
    }

    /// Returns true only for matching typed lease evidence, never peer claims
    /// about another generation or unbounded arbitrary metadata.
    fn lease_ack(&self, result: &serde_json::Value) -> Result<LeaseAck> {
        let agent_id = result
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| inert(id))
            .ok_or_else(unavailable)?;
        if result.get("generation").and_then(serde_json::Value::as_u64) != Some(self.generation) {
            return Err(unavailable());
        }
        let expires_at = result
            .get("expires_at_unix_seconds")
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
            .ok_or_else(unavailable)?;
        Ok(LeaseAck {
            agent_id: agent_id.into(),
            expires_at,
        })
    }

    /// Sends only this transport's originating owner's pending head. Failed or
    /// lost replies preserve original work; no retry or effect reconstruction.
    pub(crate) async fn deliver_next(&self, owner: &mut LifecycleOwner) -> Result<bool> {
        if !self.belongs_to(owner) {
            return Err(unavailable());
        }
        let Some(delivery) = owner.pending().cloned() else {
            return Ok(false);
        };
        self.deliver(&delivery).await?;
        if !owner.acknowledge(&delivery) {
            return Err(unavailable());
        }
        Ok(true)
    }

    /// Retires this immutable launch binding after independently observed child
    /// exit. A failed exchange is not retried; expiry remains the fallback.
    pub(crate) async fn retire(&self) -> Result<()> {
        let result = self
            .exchange("agent/external/deregister", None, None)
            .await?;
        if result.get("retired").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(unavailable());
        }
        Ok(())
    }

    /// Checks exact reducer incarnation before registration or worker startup.
    pub(super) fn belongs_to(&self, owner: &LifecycleOwner) -> bool {
        owner.transport_binding() == (self.session.as_str(), self.owner.as_str())
    }

    /// Sends one retained lifecycle delivery after immutable owner validation.
    /// A successful matching RPC result, not EOF, establishes acknowledgment.
    async fn deliver(&self, delivery: &Delivery) -> Result<()> {
        match delivery.operation {
            Operation::Present(state) => {
                if delivery.sequence == 0
                    || !matches!(
                        state,
                        "ready" | "running" | "input-wait" | "complete" | "interrupted" | "failed"
                    )
                {
                    return Err(unavailable());
                }
                let result = self
                    .exchange(
                        "agent/external/presentation",
                        None,
                        Some((delivery.sequence, state)),
                    )
                    .await?;
                if result.get("sequence").and_then(serde_json::Value::as_u64)
                    != Some(delivery.sequence)
                    || result
                        .get("changed")
                        .and_then(serde_json::Value::as_bool)
                        .is_none()
                {
                    return Err(unavailable());
                }
            }
            Operation::Retire => {
                let result = self
                    .exchange("agent/external/deregister", None, None)
                    .await?;
                if result.get("retired").and_then(serde_json::Value::as_bool) != Some(true)
                    || result
                        .get("changed")
                        .and_then(serde_json::Value::as_bool)
                        .is_none()
                {
                    return Err(unavailable());
                }
            }
        }
        Ok(())
    }

    /// Executes one same-user capability-only RPC with a strict total budget.
    /// No retries, primary initialization, subprocesses or arbitrary RPC surface.
    async fn exchange(
        &self,
        method: &'static str,
        name: Option<&str>,
        presentation: Option<(u64, &str)>,
    ) -> Result<serde_json::Value> {
        tokio::time::timeout(DEADLINE, async {
            let body = Zeroizing::new(
                serde_json::to_string(&Request {
                    jsonrpc: "2.0",
                    id: "pi-lifecycle",
                    method,
                    params: Params {
                        launch_token: self.token.expose_secret(),
                        generation: self.generation,
                        external_session_id: &self.session,
                        display_name: name,
                        sequence: presentation.map(|(sequence, _)| sequence),
                        state: presentation.map(|(_, state)| state),
                    },
                })
                .map_err(|_| unavailable())?,
            );
            let mut stream = tokio::net::UnixStream::connect(&self.socket)
                .await
                .map_err(|_| unavailable())?;
            crate::runtime::authenticated_unix_peer_uid(
                stream.as_raw_fd(),
                crate::runtime::current_effective_uid(),
            )
            .map_err(|_| unavailable())?;
            let frame = Zeroizing::new(crate::control::encode_control_body(&body));
            stream.write_all(&frame).await.map_err(|_| unavailable())?;
            let mut response = Zeroizing::new(Vec::new());
            let mut buffer = [0; 4096];
            loop {
                let count = stream.read(&mut buffer).await.map_err(|_| unavailable())?;
                if count == 0 || response.len().saturating_add(count) > MAX_BODY + 8192 {
                    return Err(unavailable());
                }
                response.extend_from_slice(&buffer[..count]);
                if let Some((frame, consumed)) =
                    crate::protocol::framing::decode_frame_incremental(&response, MAX_BODY)
                        .map_err(|_| unavailable())?
                {
                    if consumed != response.len()
                        || frame.content_type != crate::control::CONTROL_CONTENT_TYPE
                    {
                        return Err(unavailable());
                    }
                    let reply: Reply =
                        serde_json::from_str(&frame.body).map_err(|_| unavailable())?;
                    if reply.jsonrpc != "2.0"
                        || reply.id != "pi-lifecycle"
                        || !reply.result.is_object()
                    {
                        return Err(unavailable());
                    }
                    return Ok(reply.result);
                }
            }
        })
        .await
        .map_err(|_| unavailable())?
    }
}

#[cfg(test)]
mod tests;
