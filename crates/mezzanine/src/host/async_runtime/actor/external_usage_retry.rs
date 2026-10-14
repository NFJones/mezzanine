//! Bounded retry of one retained, native-admitted socket accounting obligation.
//!
//! The original dispatch task retains the exact work, reply and actor reservation.
//! Only a storage-produced precommit refusal may schedule another invocation;
//! writes/COMMIT/worker failure never do. No source lookup, marker/readiness mint,
//! provider request, durable spool or independent expense owner exists here.
//! The2s monotonic horizon bounds dispatch, not blocking syscall completion. The
//! actor receives one final completion even when the original RPC waiter closes.

use crate::error::{MezError, Result};
use crate::runtime::ExternalUsageWork;
use crate::storage::token_usage::{ExternalUsageAttempt, ExternalUsageCommit};
use std::time::Duration;

#[cfg(test)]
mod tests;

/// Selects spacing after a completed refusal, never catch-up offsets. There are
/// exactly two additional attempts, both strictly before the original deadline.
fn retry_delay(
    retries: usize,
    now: tokio::time::Instant,
    deadline: tokio::time::Instant,
) -> Option<Duration> {
    let delay = *[Duration::from_millis(100), Duration::from_millis(500)].get(retries)?;
    (now + delay < deadline).then_some(delay)
}

/// Worker failure is uncertain, not proof of precommit refusal. No panic,
/// cancellation or missing reply can synthesize a retryable storage witness.
fn worker_outcome(
    result: std::result::Result<ExternalUsageAttempt, tokio::task::JoinError>,
) -> ExternalUsageAttempt {
    result.unwrap_or_else(|_| {
        ExternalUsageAttempt::Completed(Err(MezError::invalid_state(
            "external accounting worker failed",
        )))
    })
}

/// Runs initial work and at most two explicitly precommit-refused successors.
/// Each delay starts after failure; queued delay cannot catch up in a burst.
/// A fresh worker invocation still validates immutable origin/report/publication.
pub(super) async fn execute_retained_external_usage(
    work: &ExternalUsageWork,
    dispatch_started: tokio::time::Instant,
) -> Result<ExternalUsageCommit> {
    let deadline = dispatch_started + Duration::from_secs(2);
    let mut retries = 0;
    loop {
        if work.permits_precommit_retry() && tokio::time::Instant::now() >= deadline {
            return Err(MezError::invalid_state(
                "external accounting retry dispatch horizon elapsed",
            ));
        }
        #[cfg(test)]
        if let Some(probe) = &work.worker_probe {
            let _ = probe.retry_enabled.set(work.permits_precommit_retry());
            probe
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let worker = work.clone();
        let attempt =
            worker_outcome(tokio::task::spawn_blocking(move || worker.execute_attempt()).await);
        #[cfg(test)]
        {
            let error = match &attempt {
                ExternalUsageAttempt::Completed(Err(error))
                | ExternalUsageAttempt::PrecommitRefused { error, .. } => Some(error),
                _ => None,
            };
            if let Some(error) = error
                && let Some(probe) = &work.worker_probe
                && probe.failure.set(error.kind()).is_ok()
            {
                if work.permits_precommit_retry() {
                    work.assert_unpublished_native_attempt_for_tests();
                }
                probe.failed.notify_one();
                probe.release.notified().await;
            }
        }
        match attempt {
            ExternalUsageAttempt::PrecommitRefused { error, refusal }
                if refusal.is_explicit_precommit() =>
            {
                let Some(delay) = retry_delay(retries, tokio::time::Instant::now(), deadline)
                else {
                    return Err(error);
                };
                tokio::time::sleep(delay).await;
                // Check after the await, immediately before dispatch. This is
                // not a promise that a blocked pool worker starts/finishes here.
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
                retries += 1;
            }
            attempt => return attempt.into_result(),
        }
    }
}
