//! Payload-free feedback for executor progress ingress.
//!
//! Progress cannot replace authoritative completion or authorize action replay.
//! Only proven unapplied capacity refusals may retry. A fixed-size owner reports
//! terminal refusals separately from consumed/unknown failures and acknowledgments
//! abandoned at cancellation/completion, once per execution. It retains no event,
//! action identity, source, error text, or recovery queue. This diagnostic is not
//! evidence that refused committed effects were recovered.

use super::{
    AUTHORITATIVE_EVENT_CAPACITY_RETRY_ATTEMPTS, AUTHORITATIVE_EVENT_CAPACITY_RETRY_DELAY,
};
use crate::error::Result;
use std::future::Future;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

/// Fixed executor families; arbitrary labels never enter diagnostics.
pub(super) enum ExecutorProgressFamily {
    /// Native shell execution, including confirmed patch sections.
    NativeShell,
    /// Approved network action execution; MCP has no preview forwarder.
    ExternalAction,
}

/// One execution's bounded feedback owner, retained across pending submissions.
pub(super) struct ExecutorProgressFeedback {
    family: ExecutorProgressFamily,
    unapplied_capacity_refusals: AtomicU64,
    consumed_or_unknown_failures: AtomicU64,
    unacknowledged_submissions: AtomicU64,
}

impl ExecutorProgressFeedback {
    /// Starts a quiet owner; successful ingress produces no diagnostic.
    pub(super) fn new(family: ExecutorProgressFamily) -> Self {
        Self {
            family,
            unapplied_capacity_refusals: AtomicU64::new(0),
            consumed_or_unknown_failures: AtomicU64::new(0),
            unacknowledged_submissions: AtomicU64::new(0),
        }
    }

    /// Counts only the final known refusal, never intermediary retry attempts.
    fn record_failure(&self, error: &crate::error::MezError) {
        if crate::host::async_runtime::error_is_retryable_side_effect_capacity(error) {
            increment(&self.unapplied_capacity_refusals);
        } else {
            // No proven pre-application refusal: do not infer replay safety or
            // label actor loss/other errors as definitively consumed work.
            increment(&self.consumed_or_unknown_failures);
        }
    }

    /// Formats fixed reason counts only, without retaining potentially private
    /// error messages or implying successful recovery/authoritative completion.
    fn diagnostic(&self) -> Option<String> {
        let unapplied = self.unapplied_capacity_refusals.load(Ordering::Relaxed);
        let unknown = self.consumed_or_unknown_failures.load(Ordering::Relaxed);
        let unacknowledged = self.unacknowledged_submissions.load(Ordering::Relaxed);
        if unapplied == 0 && unknown == 0 && unacknowledged == 0 {
            return None;
        }
        let family = match self.family {
            ExecutorProgressFamily::NativeShell => "native_shell",
            ExecutorProgressFamily::ExternalAction => "external_action",
        };
        Some(format!(
            "mezzanine executor progress: family={family} unapplied_capacity_refusals={unapplied} consumed_or_unknown_failures={unknown} unacknowledged_submissions={unacknowledged}"
        ))
    }
}

impl Drop for ExecutorProgressFeedback {
    fn drop(&mut self) {
        if let Some(diagnostic) = self.diagnostic() {
            // Diagnostic I/O failure must not panic during cancellation or
            // replace the authoritative executor result with a logging error.
            let _ = writeln!(std::io::stderr().lock(), "{diagnostic}");
        }
    }
}

/// Saturates fixed-size diagnostic counters rather than wrapping them.
fn increment(counter: &AtomicU64) {
    let mut value = counter.load(Ordering::Relaxed);
    loop {
        match counter.compare_exchange_weak(
            value,
            value.saturating_add(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(observed) => value = observed,
        }
    }
}

/// Tracks an individual awaited acknowledgment, not a retry delay. Dropping an
/// in-flight request cannot establish whether the actor applied its batch.
struct ProgressAcknowledgment<'a> {
    feedback: &'a ExecutorProgressFeedback,
    received: bool,
}

impl Drop for ProgressAcknowledgment<'_> {
    fn drop(&mut self) {
        if !self.received {
            increment(&self.feedback.unacknowledged_submissions);
        }
    }
}

/// Submits one original progress batch through an injected acknowledgment seam.
/// Terminal errors are counted, not propagated into unrelated service teardown.
/// Only known unapplied capacity refusals retry; consumed/unknown errors stop
/// immediately. Cancellation records uncertainty only while awaiting a reply.
pub(super) async fn submit_executor_progress<T, Submit, Submission>(
    feedback: &ExecutorProgressFeedback,
    mut submit: Submit,
) where
    Submit: FnMut() -> Submission,
    Submission: Future<Output = Result<T>>,
{
    let mut attempts = 0usize;
    loop {
        let result = {
            let mut acknowledgment = ProgressAcknowledgment {
                feedback,
                received: false,
            };
            let result = submit().await;
            acknowledgment.received = true;
            result
        };
        match result {
            Ok(_) => return,
            Err(error)
                if attempts < AUTHORITATIVE_EVENT_CAPACITY_RETRY_ATTEMPTS
                    && crate::host::async_runtime::error_is_retryable_side_effect_capacity(
                        &error,
                    ) =>
            {
                attempts = attempts.saturating_add(1);
                tokio::time::sleep(AUTHORITATIVE_EVENT_CAPACITY_RETRY_DELAY).await;
            }
            Err(error) => {
                feedback.record_failure(&error);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests;
