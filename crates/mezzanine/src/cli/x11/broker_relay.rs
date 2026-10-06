//! Client-local credential substitution for a dedicated authenticated broker stream.
//!
//! The caller authenticates the stream's exact frontend/session/occurrence and
//! preserves readiness read-ahead before handing it here. This layer reads one
//! bounded setup packet, validates the fake cookie, substitutes the real cookie
//! locally and dials only the previously frozen X destination. No real credential,
//! display or authority path is sent to the broker. Idle demand waiting remains
//! caller-owned; the first byte starts one finite setup deadline. Established
//! relay uses caller cancellation and directional EOF.
//! Dropping the future closes its owned streams, with no retry or detached task.
//! PreparedX11Client's credential lease remains separately caller-owned.

use super::X11ClientForwarder;
use crate::error::{MezError, Result};
use crate::runtime::x11::{X11_MAX_SETUP_BYTES, X11SetupProgress, parse_x11_setup};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

/// Distinguishes an idle channel's retirement from completed application work.
/// Only completed setup and bidirectional relay permit fresh channel demand.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BrokerRelayOutcome {
    /// EOF arrived before setup; no application was admitted or relayed.
    IdleRetired,
    /// Setup was validated and both application directions completed normally.
    Completed,
}

impl X11ClientForwarder {
    /// Relays an independently authenticated dedicated broker byte stream to the
    /// frozen client-local X target. Buffered readiness successors must remain
    /// part of this stream; the caller may not discard them before invocation.
    /// Setup errors expose no payloads, credentials or local destination. The
    /// established application relay is not limited by the setup deadline.
    /// Before the first setup byte, idle waiting retains this stream until EOF
    /// or caller cancellation and performs no local dialing or automatic retry.
    pub(crate) async fn relay_broker_stream<S>(&self, broker: S, budget: Duration) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.relay_broker_stream_outcome(broker, budget)
            .await
            .map(|_| ())
    }

    /// Reports whether EOF retired an idle stream or followed completed setup
    /// and application relay. Supervisors must not replenish idle retirement.
    pub(super) async fn relay_broker_stream_outcome<S>(
        &self,
        mut broker: S,
        budget: Duration,
    ) -> Result<BrokerRelayOutcome>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget) {
            return Err(MezError::invalid_args(
                "broker X11 client setup deadline invalid",
            ));
        }
        let mut first = [0_u8; 1];
        let read = broker
            .read(&mut first)
            .await
            .map_err(|_| MezError::invalid_state("broker X11 client demand unavailable"))?;
        if read == 0 {
            return Ok(BrokerRelayOutcome::IdleRetired);
        }
        let deadline = tokio::time::Instant::now() + budget;
        let mut local = tokio::time::timeout_at(deadline, async {
            let mut setup = read_setup(&mut broker, first[0]).await?;
            self.rewrite_setup(&mut setup)
                .map_err(|_| MezError::forbidden("broker X11 client setup credential invalid"))?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let mut local = self.connect(remaining).await.map_err(|_| {
                MezError::invalid_state("broker X11 client local connection unavailable")
            })?;
            local.write_all(&setup).await.map_err(|_| {
                MezError::invalid_state("broker X11 client local setup write unavailable")
            })?;
            local.flush().await.map_err(|_| {
                MezError::invalid_state("broker X11 client local setup flush unavailable")
            })?;
            Ok::<_, MezError>(local)
        })
        .await
        .map_err(|_| MezError::invalid_state("broker X11 client setup timed out"))??;
        tokio::io::copy_bidirectional(&mut broker, &mut local)
            .await
            .map_err(|_| MezError::invalid_state("broker X11 client relay unavailable"))?;
        Ok(BrokerRelayOutcome::Completed)
    }
}

/// Reads exactly one bounded raw setup, leaving application bytes in the source.
/// Requested lengths come only from the established strict setup parser.
async fn read_setup<R: AsyncRead + Unpin>(source: &mut R, first: u8) -> Result<Zeroizing<Vec<u8>>> {
    let mut setup = Zeroizing::new(vec![first]);
    loop {
        match parse_x11_setup(&setup)
            .map_err(|_| MezError::forbidden("broker X11 client setup packet invalid"))?
        {
            X11SetupProgress::Complete(_) => return Ok(setup),
            X11SetupProgress::Incomplete { required_len } => {
                if required_len <= setup.len() || required_len > X11_MAX_SETUP_BYTES {
                    return Err(MezError::forbidden("broker X11 client setup size invalid"));
                }
                let start = setup.len();
                setup.resize(required_len, 0);
                source
                    .read_exact(&mut setup[start..])
                    .await
                    .map_err(|_| MezError::invalid_state("broker X11 client setup incomplete"))?;
            }
        }
    }
}

#[cfg(test)]
mod tests;
