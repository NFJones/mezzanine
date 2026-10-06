//! Attaching-client ownership of bounded dedicated broker X11 channels.
//!
//! One shared opener supplies finite admission and exact-session readiness. Each
//! channel future owns opening and local credential substitution through EOF.
//! Only completed application relays admit fresh demand; idle EOF retires without
//! reopening. Completed channels never replay their bytes. Any error
//! retires this supervisor instead of retrying an uncertain channel operation.
//! Cancellation or future abandonment drops every owned future and stream, with
//! no detached tasks. The attachment separately owns the prepared credential
//! lease and must dispose supervision before explicit lease cleanup. This staged
//! component does not activate ordinary CLI routing or restore terminal modes.

use super::X11ClientForwarder;
use super::broker_relay::BrokerRelayOutcome;
use crate::error::{MezError, Result};
use crate::host::outbound_frontend::client::X11ChannelOpener;
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

impl X11ClientForwarder {
    /// Owns at most `limit` opening/active relays using one retained opener.
    /// The caller must keep the exact parent attachment alive and supply limits
    /// compatible with broker admission. Errors propagate without reconnect or
    /// channel retry; cancellation disposes streams but not the credential lease.
    pub(crate) async fn supervise_broker_channels<C>(
        &self,
        opener: &X11ChannelOpener,
        limit: usize,
        budget: Duration,
        cancellation: C,
    ) -> Result<()>
    where
        C: Future<Output = ()>,
    {
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget) {
            return Err(MezError::invalid_args(
                "broker X11 supervision deadline invalid",
            ));
        }
        supervise(
            limit,
            || async {
                let channel = opener.open(budget).await?;
                self.relay_broker_stream_outcome(channel, budget).await
            },
            cancellation,
        )
        .await
    }
}

/// Keeps every channel operation directly owned in a finite collection. Only
/// completed application relay permits fresh demand; idle EOF retires supervision.
/// Failures never resubmit the operation.
/// Cancellation is prioritized, including before the first operation is polled.
async fn supervise<F, W, C>(limit: usize, operation: F, cancellation: C) -> Result<()>
where
    F: Fn() -> W,
    W: Future<Output = Result<BrokerRelayOutcome>>,
    C: Future<Output = ()>,
{
    if !(1..=1024).contains(&limit) {
        return Err(MezError::invalid_args(
            "broker X11 supervision capacity invalid",
        ));
    }
    let mut channels: FuturesUnordered<Pin<Box<W>>> =
        (0..limit).map(|_| Box::pin(operation())).collect();
    tokio::pin!(cancellation);
    loop {
        tokio::select! {
            biased;
            () = &mut cancellation => return Ok(()),
            result = channels.next() => {
                match result.ok_or_else(|| MezError::invalid_state("broker X11 supervision unavailable"))?? {
                    BrokerRelayOutcome::IdleRetired => return Ok(()),
                    BrokerRelayOutcome::Completed => channels.push(Box::pin(operation())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
