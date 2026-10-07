//! Inherited private-stream Pi callback ingress, never daemon capability ingress.
//!
//! The launcher supplies the connected Unix stream, immutable context session and
//! observer epoch. Payloads contain only allowlisted event facts: no routing,
//! credentials, paths, titles or vendor content. Each newline-delimited JSON frame
//! is bounded at 1024 bytes with one non-resetting partial-frame deadline. Idle
//! silence is allowed because lease renewal is independently worker-owned. EOF,
//! invalid frames, pressure or cancellation release this bridge's ingress handle;
//! accepted reports are not replayed and no vendor decision is changed.

use std::os::fd::AsRawFd;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use tokio::time::Instant;

use super::{pi, pi_renewal, pi_session::Ingress};
use crate::error::{MezError, Result};

/// Maximum complete event JSON bytes, excluding the newline delimiter.
const MAX_FRAME: usize = 1024;
/// Total time from first partial byte to newline; further bytes do not reset it.
const FRAME_DEADLINE: Duration = Duration::from_millis(250);

/// Only the released observer's lifecycle facts may cross this stream.
#[derive(Deserialize, serde::Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Event {
    #[serde(rename = "session_start")]
    Start { reason: StartReason },
    #[serde(rename = "agent_start")]
    Running {},
    #[serde(rename = "ui_prompt_start")]
    InputStart {
        reason: PromptReason,
        kind: PromptKind,
    },
    #[serde(rename = "ui_prompt_end")]
    InputEnd {
        reason: PromptReason,
        kind: PromptKind,
    },
    #[serde(rename = "agent_before_settle")]
    Candidate { outcome: Outcome },
    #[serde(rename = "agent_settled")]
    Settled {},
    #[serde(rename = "session_shutdown")]
    Shutdown { reason: StopReason },
}

/// Known released session activation reasons, not authority to rebind a session.
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StartReason {
    Startup,
    Reload,
    New,
    Resume,
    Fork,
}
/// Known teardown reasons; none proves process death.
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StopReason {
    Quit,
    Reload,
    New,
    Resume,
    Fork,
}
/// Extension UI wait, not automatic permission approval.
#[derive(Deserialize, serde::Serialize)]
pub(crate) enum PromptReason {
    #[serde(rename = "ui_prompt")]
    Ui,
}
/// Released UI prompt forms; no prompt text or answer is accepted.
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PromptKind {
    Select,
    Confirm,
    Input,
    Editor,
    Custom,
}
/// Candidate outcome remains provisional until a separate final notification.
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Outcome {
    Completed,
    Aborted,
    Error,
}

/// Generic errors omit arbitrary callback and private peer content.
fn unavailable() -> MezError {
    MezError::invalid_state("Pi observer stream unavailable")
}

/// Validates the wire allowlist before reusing the released Rust projector.
pub(crate) fn observation(session: &str, bytes: &[u8]) -> Result<pi::Observation> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(unavailable());
    }
    // Typed deserialization rejects duplicate/unknown fields and enums; consume
    // these inert fields only for validation, never serialize untrusted content.
    let event: Event = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    match event {
        Event::Start { reason } => {
            let _ = reason;
        }
        Event::InputStart { reason, kind } | Event::InputEnd { reason, kind } => {
            let _ = (reason, kind);
        }
        Event::Candidate { outcome } => {
            let _ = outcome;
        }
        Event::Shutdown { reason } => {
            let _ = reason;
        }
        Event::Running {} | Event::Settled {} => {}
    }
    pi::normalize(pi::RELEASE, session, session, bytes)
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)
}

/// Bounds a partial-frame read independently of Tokio's ready-I/O precedence.
/// A deadline is checked before polling and after completion; receiving bytes
/// does not prove they arrived before expiry when this future was not polled.
async fn read_before_deadline(
    deadline: Option<Instant>,
    read: impl std::future::Future<Output = std::io::Result<usize>>,
) -> Result<usize> {
    if deadline.is_some_and(|end| Instant::now() >= end) {
        return Err(unavailable());
    }
    let count = if let Some(end) = deadline {
        tokio::time::timeout_at(end, read)
            .await
            .map_err(|_| unavailable())?
    } else {
        read.await
    }
    .map_err(|_| unavailable())?;
    if deadline.is_some_and(|end| Instant::now() >= end) {
        return Err(unavailable());
    }
    Ok(count)
}

/// Waits for the initial matched session-start fact before public-launch
/// registration. One-byte reads preserve every subsequent buffered frame for
/// the existing coordinator. Idle silence is permitted, but partial input uses
/// the same finite deadline/allowlist as ordinary observer ingress. EOF, another
/// first fact or malformed input ends telemetry without daemon work.
pub(crate) async fn wait_for_start(
    stream: &mut tokio::net::UnixStream,
    session: &str,
) -> Result<pi::Observation> {
    crate::runtime::authenticated_unix_peer_uid(
        stream.as_raw_fd(),
        crate::runtime::current_effective_uid(),
    )
    .map_err(|_| unavailable())?;
    let mut bytes = Vec::with_capacity(MAX_FRAME);
    let mut deadline = None;
    loop {
        let mut byte = [0];
        if read_before_deadline(deadline, stream.read(&mut byte)).await? == 0 {
            return Err(unavailable());
        }
        if byte[0] == b'\n' {
            let fact = observation(session, &bytes)?;
            return if matches!(fact, pi::Observation::SessionStarted { reason: "startup" }) {
                Ok(fact)
            } else {
                Err(unavailable())
            };
        }
        if bytes.len() == MAX_FRAME {
            return Err(unavailable());
        }
        if bytes.is_empty() {
            deadline = Some(Instant::now() + FRAME_DEADLINE);
        }
        bytes.push(byte[0]);
    }
}

/// Serves only a launcher-supplied stream after kernel peer authentication.
pub(crate) async fn serve(
    mut stream: tokio::net::UnixStream,
    session: &str,
    mut epoch: u64,
    ingress: Ingress,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    observation(session, br#"{"type":"agent_start"}"#)?;
    if epoch == 0 {
        return Err(unavailable());
    }
    crate::runtime::authenticated_unix_peer_uid(
        stream.as_raw_fd(),
        crate::runtime::current_effective_uid(),
    )
    .map_err(|_| unavailable())?;
    let mut pending = Vec::with_capacity(MAX_FRAME);
    let mut deadline = None;
    let mut buffer = [0; 256];
    let mut awaiting_reload = false;
    loop {
        let read = stream.read(&mut buffer);
        let count = tokio::select! {
            biased;
            () = pi_renewal::cancelled(&mut stop) => return Ok(()),
            () = ingress.closed() => return Ok(()),
            result = read_before_deadline(deadline, read) => result?,
        };
        if count == 0 {
            return if pending.is_empty() {
                Ok(())
            } else {
                Err(unavailable())
            };
        }
        for &byte in &buffer[..count] {
            if byte == b'\n' {
                let fact = observation(session, &pending)?;
                if !awaiting_reload
                    && matches!(fact, pi::Observation::SessionStarted { reason: "reload" })
                {
                    return Err(unavailable());
                }
                if awaiting_reload {
                    if !matches!(fact, pi::Observation::SessionStarted { reason: "reload" }) {
                        pending.clear();
                        deadline = None;
                        continue;
                    }
                    // FIFO ingress applies the old shutdown before making this
                    // same-session proposal. Only explicit parent confirmation
                    // activates the reducer's new epoch; callbacks cannot do so.
                    let offer = ingress.attach_after_reload(session)?;
                    epoch = offer.await.map_err(|_| unavailable())??.confirm().await?;
                }
                let reloading =
                    matches!(fact, pi::Observation::SessionShutdown { reason: "reload" });
                ingress
                    .observe(epoch, session, fact)
                    .map_err(|_| unavailable())?;
                awaiting_reload = reloading;
                pending.clear();
                deadline = None;
            } else {
                if pending.len() == MAX_FRAME {
                    return Err(unavailable());
                }
                if pending.is_empty() {
                    deadline = Some(Instant::now() + FRAME_DEADLINE);
                }
                pending.push(byte);
            }
        }
    }
}

#[cfg(test)]
mod tests;
