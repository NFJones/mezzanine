//! Deterministic acknowledgment-policy tests with no actor or native process.
//! The injected seam exercises the production retry and feedback owner; it does
//! not prove committed-effect custody, actual durable writes, or native cleanup.

use super::*;
use crate::error::MezError;
use std::future::{pending, ready};
use std::sync::atomic::AtomicUsize;

/// Builds the real classifier's content-free queue-full marker with a private
/// canary to ensure no diagnostic reflects the actor's arbitrary error text.
fn capacity_error(suffix: &str) -> MezError {
    MezError::invalid_state(format!(
        "async runtime side-effect queue is full PRIVATE_ERROR_CANARY {suffix}"
    ))
}

/// Successful progress, including a safe transient refusal followed by success,
/// stays quiet. Retry retains the caller's same original source rather than
/// asking an executor to regenerate or repeat its action.
#[tokio::test(start_paused = true)]
async fn executor_progress_success_and_safe_recovery_are_quiet() {
    let feedback = ExecutorProgressFeedback::new(ExecutorProgressFamily::NativeShell);
    let mut calls = 0;
    submit_executor_progress(&feedback, || {
        calls += 1;
        ready(if calls == 1 {
            Err(capacity_error("applied=0"))
        } else {
            Ok(())
        })
    })
    .await;
    assert_eq!(calls, 2);
    assert!(feedback.diagnostic().is_none());
    submit_executor_progress(&feedback, || ready(Ok(()))).await;
    assert!(feedback.diagnostic().is_none());
}

/// Exhaustion counts one refused logical batch, not all 33 attempts. No extra
/// source, error payload, or retry owner survives the finite retry horizon.
#[tokio::test(start_paused = true)]
async fn executor_progress_exhaustion_reports_one_unapplied_refusal() {
    let feedback = ExecutorProgressFeedback::new(ExecutorProgressFamily::NativeShell);
    let mut calls = 0;
    submit_executor_progress(&feedback, || {
        calls += 1;
        ready::<Result<()>>(Err(capacity_error("applied=0")))
    })
    .await;
    assert_eq!(calls, AUTHORITATIVE_EVENT_CAPACITY_RETRY_ATTEMPTS + 1);
    assert_eq!(
        feedback.diagnostic().as_deref(),
        Some(
            "mezzanine executor progress: family=native_shell unapplied_capacity_refusals=1 consumed_or_unknown_failures=0 unacknowledged_submissions=0"
        )
    );
}

/// Both consumed marker orders and partially applied failures stop immediately.
/// Actor loss is unknown, not permission to replay and not proof of consumption.
/// All diagnostics are fixed reason/count projections with no error canary.
#[tokio::test]
async fn executor_progress_consumed_or_unknown_errors_never_retry() {
    let feedback = ExecutorProgressFeedback::new(ExecutorProgressFamily::ExternalAction);
    for error in [
        capacity_error("applied=0 consumed=1"),
        capacity_error("consumed=1 applied=0"),
        capacity_error("applied=1"),
        MezError::invalid_state("actor unavailable PRIVATE_ERROR_CANARY"),
    ] {
        let mut calls = 0;
        let mut error = Some(error);
        submit_executor_progress(&feedback, || {
            calls += 1;
            ready::<Result<()>>(Err(error.take().expect("consumed work must never retry")))
        })
        .await;
        assert_eq!(calls, 1);
    }
    assert_eq!(
        feedback.diagnostic().as_deref(),
        Some(
            "mezzanine executor progress: family=external_action unapplied_capacity_refusals=0 consumed_or_unknown_failures=4 unacknowledged_submissions=0"
        )
    );
}

/// Dropping an actually polled ingress records unknown acknowledgment once;
/// creating and dropping an unpolled helper never claims a request was issued.
/// This models terminal-wins cancellation without pretending the actor rolled
/// back work already queued before the reply was lost.
#[tokio::test]
async fn executor_progress_dropped_ack_is_unknown_not_unapplied() {
    let feedback = ExecutorProgressFeedback::new(ExecutorProgressFamily::ExternalAction);
    let calls = AtomicUsize::new(0);
    {
        let submission = submit_executor_progress(&feedback, || {
            calls.fetch_add(1, Ordering::Relaxed);
            pending::<Result<()>>()
        });
        tokio::pin!(submission);
        assert!(
            std::future::poll_fn(|context| {
                std::task::Poll::Ready(submission.as_mut().poll(context).is_pending())
            })
            .await
        );
    }
    drop(submit_executor_progress(&feedback, || {
        calls.fetch_add(1, Ordering::Relaxed);
        ready(Ok::<(), MezError>(()))
    }));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        feedback.diagnostic().as_deref(),
        Some(
            "mezzanine executor progress: family=external_action unapplied_capacity_refusals=0 consumed_or_unknown_failures=0 unacknowledged_submissions=1"
        )
    );
}

/// A known unapplied refusal awaiting its retry delay is not an unacknowledged
/// request. Cancelling there cannot invent consumed/unknown ingress evidence.
#[tokio::test(start_paused = true)]
async fn executor_progress_cancelled_retry_delay_has_no_unknown_ack() {
    let feedback = ExecutorProgressFeedback::new(ExecutorProgressFamily::NativeShell);
    let mut calls = 0;
    {
        let submission = submit_executor_progress(&feedback, || {
            calls += 1;
            ready::<Result<()>>(Err(capacity_error("applied=0")))
        });
        tokio::pin!(submission);
        assert!(
            std::future::poll_fn(|context| {
                std::task::Poll::Ready(submission.as_mut().poll(context).is_pending())
            })
            .await
        );
    }
    assert_eq!(calls, 1);
    assert!(feedback.diagnostic().is_none());
}

/// Fixed counters saturate even at exhaustion; repeated errors cannot wrap a
/// count to zero and silence the execution's single retirement diagnostic.
#[test]
fn executor_progress_feedback_counts_saturate() {
    let feedback = ExecutorProgressFeedback::new(ExecutorProgressFamily::NativeShell);
    feedback
        .consumed_or_unknown_failures
        .store(u64::MAX, Ordering::Relaxed);
    feedback.record_failure(&MezError::invalid_state("PRIVATE_ERROR_CANARY"));
    assert_eq!(
        feedback
            .consumed_or_unknown_failures
            .load(Ordering::Relaxed),
        u64::MAX
    );
    assert!(
        !feedback
            .diagnostic()
            .unwrap()
            .contains("PRIVATE_ERROR_CANARY")
    );
}

/// The real scoped network driver returns its canonical result while progress
/// still awaits ingress. Its dropped acknowledgment is recorded before the
/// execution's aggregate owner retires; no obsolete tail flush is required.
#[tokio::test]
async fn executor_progress_terminal_wins_records_pending_ack_once() {
    let feedback = ExecutorProgressFeedback::new(ExecutorProgressFamily::ExternalAction);
    let (sender, receiver) = tokio::sync::watch::channel(None);
    sender.send_replace(Some("PRIVATE_SOURCE_CANARY".to_string()));
    let (finish, completion) = tokio::sync::oneshot::channel::<u8>();
    let mut finish = Some(finish);
    let calls = AtomicUsize::new(0);
    tokio::pin!(completion);
    let result = super::super::drive_external_network_with_progress(
        completion.as_mut(),
        receiver,
        |_, _| {
            let finish = finish.take().expect("one original progress submission");
            let feedback = &feedback;
            let calls = &calls;
            async move {
                let mut finish = Some(finish);
                submit_executor_progress(feedback, || {
                    calls.fetch_add(1, Ordering::Relaxed);
                    finish.take().unwrap().send(41).unwrap();
                    pending::<Result<()>>()
                })
                .await;
            }
        },
    )
    .await;
    assert_eq!(result.unwrap(), 41);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        feedback.diagnostic().as_deref(),
        Some(
            "mezzanine executor progress: family=external_action unapplied_capacity_refusals=0 consumed_or_unknown_failures=0 unacknowledged_submissions=1"
        )
    );
}
