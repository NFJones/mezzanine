//! Persistent Pi lease renewal independent of extension callback traffic.
//!
//! A launcher owns this future and its cancellation channel outside extension
//! reload lifetime. It consumes only an explicitly supplied capability transport;
//! registration and scheduled renewal never mint authority or rebind a session.
//! Published leases have conservative monotonic expiry, not just wall-clock
//! timestamps. Failure, cancellation or worker drop clears availability, never
//! retries uncertain work or mutates the vendor. No installer is enabled here.

use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch;
use tokio::time::Instant;

use super::pi_transport::{CapabilityTransport, LeaseAck};
use crate::error::{MezError, Result};

/// Server-issued registration identity and a conservative local expiry fence.
#[derive(Debug, Clone)]
pub(crate) struct ActiveLease {
    /// Exact returned identity, never replaced by a different renewal response.
    pub(crate) agent_id: String,
    /// Exact server expiry; repeated responses cannot extend local availability.
    server_expiry: u64,
    /// Monotonic deadline; readers must check this even while a worker stalls.
    expires: Instant,
}

impl ActiveLease {
    /// Reports telemetry availability only before its observed lease expires.
    /// Neither availability nor expiry proves vendor process liveness.
    pub(crate) fn is_current(&self) -> bool {
        Instant::now() < self.expires
    }

    /// Converts a bounded future UTC acknowledgment into conservative monotonic
    /// evidence. The request-start anchor prevents reply latency extending it.
    fn accept(
        ack: LeaseAck,
        wall_now: u64,
        started: Instant,
        previous: Option<&Self>,
    ) -> Result<Self> {
        let remaining = ack
            .expires_at
            .checked_sub(wall_now)
            .filter(|seconds| *seconds > 1 && *seconds <= 60)
            .ok_or_else(unavailable)?;
        // UTC seconds are rounded down. Reserve one full second so this
        // local fence never relies on the fractional remainder being zero.
        let expires = started + std::time::Duration::from_secs(remaining - 1);
        if Instant::now() >= expires
            || previous.is_some_and(|old| {
                !old.is_current()
                    || old.agent_id != ack.agent_id
                    || ack.expires_at <= old.server_expiry
                    || expires <= old.expires
            })
        {
            return Err(unavailable());
        }
        Ok(Self {
            agent_id: ack.agent_id,
            server_expiry: ack.expires_at,
            expires,
        })
    }
}

/// Clears publication when its worker ends or is dropped at any await point.
struct Publication(watch::Sender<Option<ActiveLease>>);

impl Drop for Publication {
    /// No async teardown or implicit deregistration: expiry remains server-owned.
    fn drop(&mut self) {
        self.0.send_replace(None);
    }
}

/// Payload-free error; expired or inconsistent acknowledgments cannot revive a lease.
fn unavailable() -> MezError {
    MezError::invalid_state("Pi renewal lease unavailable")
}

/// Waits for explicit cancellation or loss of launcher ownership. False updates
/// do not cancel, and closure is terminal rather than a busy ready branch.
async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() || stop.changed().await.is_err() {
            return;
        }
    }
}

/// Registers once and renews at half the remaining monotonic lease while idle.
/// The owning launcher must cancel/drop this future on shutdown or replacement.
/// No retry occurs after any exchange/lease failure, and publication is cleared.
pub(crate) async fn run(
    transport: &CapabilityTransport,
    name: &str,
    status: watch::Sender<Option<ActiveLease>>,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    run_with_clock(transport, name, status, stop, || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs())
    })
    .await
}

/// Shares production scheduling with a per-worker test clock, never global time
/// or credential mutation. One cancellation selection owns every exchange.
async fn run_with_clock(
    transport: &CapabilityTransport,
    name: &str,
    status: watch::Sender<Option<ActiveLease>>,
    mut stop: watch::Receiver<bool>,
    clock: impl Fn() -> Option<u64>,
) -> Result<()> {
    let publication = Publication(status);
    let started = Instant::now();
    let ack = tokio::select! {
        biased;
        () = cancelled(&mut stop) => return Ok(()),
        ack = transport.register(name) => ack?,
    };
    let mut lease = ActiveLease::accept(ack, clock().ok_or_else(unavailable)?, started, None)?;
    loop {
        publication.0.send_replace(Some(lease.clone()));
        let remaining = lease
            .expires
            .checked_duration_since(Instant::now())
            .ok_or_else(unavailable)?;
        tokio::select! {
            biased;
            () = cancelled(&mut stop) => return Ok(()),
            () = tokio::time::sleep(remaining / 2) => {}
        }
        if !lease.is_current() {
            return Err(unavailable());
        }
        let started = Instant::now();
        let ack = tokio::select! {
            biased;
            () = cancelled(&mut stop) => return Ok(()),
            ack = tokio::time::timeout_at(lease.expires, transport.renew()) =>
                ack.map_err(|_| unavailable())??,
        };
        lease = ActiveLease::accept(ack, clock().ok_or_else(unavailable)?, started, Some(&lease))?;
    }
}

#[cfg(test)]
mod tests;
