//! Pure budget boundaries and actual worker-uncertainty classification.
//!
//! These tests do not replace native/actor admission acceptance: they prove the
//! finite timing contract and that a failed blocking task cannot mint retry
//! authority. Explicit time values exercise policy without rewriting a process
//! clock or manually delivering a native timer event.

use super::*;

/// Initial work has exactly two additional slots; each requires its full delay
/// after the preceding failure and a strict remaining horizon. Neither an
/// exhausted slot nor a delayed queue catches up or resets the deadline.
#[test]
fn external_usage_retry_budget_is_finite_spaced_and_strict() {
    let start = tokio::time::Instant::now();
    let deadline = start + Duration::from_secs(2);
    assert_eq!(
        retry_delay(0, start, deadline),
        Some(Duration::from_millis(100))
    );
    assert_eq!(
        retry_delay(1, start + Duration::from_millis(400), deadline),
        Some(Duration::from_millis(500))
    );
    assert_eq!(retry_delay(2, start, deadline), None);
    assert_eq!(retry_delay(usize::MAX, start, deadline), None);
    assert_eq!(
        retry_delay(0, start + Duration::from_millis(1900), deadline),
        None
    );
    assert_eq!(
        retry_delay(1, start + Duration::from_millis(1500), deadline),
        None
    );
    assert_eq!(
        retry_delay(0, deadline + Duration::from_secs(1), deadline),
        None
    );
}

/// An actual blocking-worker panic is terminal/unknown. Even though a worker
/// might have committed before panicking, absence of its result is never a
/// precommit witness or reason to replay the operation automatically.
#[tokio::test(flavor = "current_thread")]
async fn external_usage_retry_worker_panic_is_not_precommit_authority() {
    let result = tokio::task::spawn_blocking(|| -> ExternalUsageAttempt {
        panic!("generated worker uncertainty fixture")
    })
    .await;
    assert!(matches!(
        worker_outcome(result),
        ExternalUsageAttempt::Completed(Err(_))
    ));
}

/// Broad I/O diagnostics and successful worker results remain ordinary outcomes;
/// the scheduling adapter cannot promote them to a storage-produced witness.
#[test]
fn external_usage_retry_adapter_preserves_terminal_io_without_promotion() {
    let diagnostic = MezError::from(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        None,
    ));
    assert!(matches!(
        worker_outcome(Ok(ExternalUsageAttempt::Completed(Err(diagnostic)))),
        ExternalUsageAttempt::Completed(Err(_))
    ));
}
