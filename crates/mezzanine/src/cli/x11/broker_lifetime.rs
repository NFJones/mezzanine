//! Attaching-client coordination of foreground, channel disposal and credentials.
//!
//! One directly owned foreground receives persistent watch-based cancellation.
//! X11 completion/error or external cancellation asks that foreground to retire
//! and restore its terminal under a finite cleanup budget. Channel work is dropped
//! before waiting for foreground cleanup, and all scoped futures are disposed
//! before explicit credential cleanup. Whole-future abandonment still relies on
//! the caller's terminal guard and the prepared credential lease's Drop cleanup.
//! No detached tasks, retries, endpoint ownership or ordinary routing are added.

use super::PreparedX11Client;
use crate::error::{MezError, Result};
use crate::host::outbound_frontend::client::X11ChannelOpener;
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;

impl PreparedX11Client {
    /// Owns this prepared credential through foreground/channel retirement.
    /// The foreground factory must honor its cancellation receiver and preserve
    /// its terminal restoration path; a concrete guard remains caller-owned.
    /// Opener and supervisor limits must agree. Errors never retry channel work;
    /// causal operation errors take precedence over later cleanup failures.
    #[allow(
        clippy::too_many_arguments,
        reason = "packet setup and foreground retirement have independent finite budgets"
    )]
    pub(crate) async fn run_broker_attachment<F, W, C>(
        self,
        opener: X11ChannelOpener,
        limit: usize,
        budget: Duration,
        retirement_budget: Duration,
        foreground: F,
        cancellation: C,
    ) -> Result<()>
    where
        F: FnOnce(watch::Receiver<bool>) -> W,
        W: Future<Output = Result<()>>,
        C: Future<Output = ()>,
    {
        let result = if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget)
            || !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&retirement_budget)
            || !(1..=1024).contains(&limit)
        {
            Err(MezError::invalid_args(
                "broker X11 attachment budget invalid",
            ))
        } else {
            let forwarder = self.forwarder();
            let (stop, stopped) = watch::channel(false);
            coordinate(
                foreground(stopped),
                forwarder.supervise_broker_channels(&opener, limit, budget, std::future::pending()),
                cancellation,
                stop,
                retirement_budget,
            )
            .await
        };
        // coordinate has returned: its foreground and X11 futures no longer own
        // streams. Dispose discovery before invoking credential cleanup as well.
        drop(opener);
        let cleanup = self.close().await;
        match result {
            Ok(()) => cleanup,
            Err(error) => {
                let _ = cleanup;
                Err(error)
            }
        }
    }
}

/// Waits for a persistent foreground stop request or sender disposal.
/// Already-published cancellation is observed before waiting for another change.
pub(crate) async fn broker_attachment_cancelled(mut stopped: watch::Receiver<bool>) {
    while !*stopped.borrow_and_update() {
        if stopped.changed().await.is_err() {
            break;
        }
    }
}

/// Retains foreground cleanup after requesting stop, but disposes X11 immediately.
/// A completed foreground already owns its restoration result and is not rerun.
async fn coordinate<W, X, C>(
    foreground: W,
    channels: X,
    cancellation: C,
    stop: watch::Sender<bool>,
    budget: Duration,
) -> Result<()>
where
    W: Future<Output = Result<()>>,
    X: Future<Output = Result<()>>,
    C: Future<Output = ()>,
{
    let mut foreground = Box::pin(foreground);
    let mut channels = Box::pin(channels);
    tokio::pin!(cancellation);
    let channel_result = tokio::select! {
        biased;
        () = &mut cancellation => Ok(()),
        result = &mut foreground => return result,
        result = &mut channels => result,
    };
    drop(channels);
    let _ = stop.send(true);
    let foreground_result = tokio::time::timeout(budget, foreground)
        .await
        .map_err(|_| MezError::invalid_state("broker X11 foreground retirement timed out"))
        .and_then(|result| result);
    match channel_result {
        Ok(()) => foreground_result,
        Err(error) => {
            let _ = foreground_result;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests;
