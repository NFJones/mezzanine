//! Scheduling regressions for real commit guards and cancellation observations.
//!
//! These prove exclusion and fence ownership, not permission, file mutation or
//! whole-dispatch integration. Barriers establish actual concurrent custody
//! without inferring a stopped worker from an elapsed sleep.

use super::*;

/// Conflicting admitted actions share one writer slot. Releasing a completed
/// guard admits the next action, while a single action cannot acquire a second
/// guard even if accidentally supplied a different coordinator.
#[test]
fn guards_exclude_conflicting_writers_and_duplicate_action_admission() {
    let coordinator = NativePatchWriteCoordinator::default();
    let other_coordinator = NativePatchWriteCoordinator::default();
    let first = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
    let second = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
    let guard = first.try_begin(&coordinator).unwrap();
    assert!(second.try_begin(&coordinator).is_err());
    assert!(first.try_begin(&other_coordinator).is_err());
    let other = second.try_begin(&other_coordinator).unwrap();
    drop(other);
    drop(guard);
    assert!(second.try_begin(&coordinator).is_ok());
}

/// A cancellation acknowledged before admission prevents all later guards.
/// The shared cancellation identity affects its own action only, leaving an
/// independent replacement dispatch able to use the same writer coordinator.
#[test]
fn acknowledged_cancellation_fences_future_commits_without_revoking_replacement() {
    let coordinator = NativePatchWriteCoordinator::default();
    let cancellation = NativeActionCancellation::new();
    let fence = NativePatchCommitFence::new(cancellation.clone(), None).unwrap();
    assert_eq!(fence.cancel(), NativePatchFenceStatus::Fenced);
    assert!(fence.try_begin(&coordinator).is_err());
    assert!(cancellation.flag().load(Ordering::SeqCst));
    let replacement = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
    assert!(replacement.try_begin(&coordinator).is_ok());
}

/// A worker already owns a real guard before actor cancellation. Cancellation
/// must return promptly with InFlight, and cannot acknowledge a stopped commit
/// until the worker positively releases its guard. Later admission stays closed.
#[test]
fn cancellation_retains_in_flight_commit_until_actual_guard_release() {
    let coordinator = NativePatchWriteCoordinator::default();
    let fence = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
    let worker_fence = fence.clone();
    let admitted = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let worker_admitted = Arc::clone(&admitted);
    let worker_release = Arc::clone(&release);
    let worker = std::thread::spawn(move || {
        let guard = worker_fence.try_begin(&coordinator).unwrap();
        worker_admitted.wait();
        worker_release.wait();
        drop(guard);
        assert!(worker_fence.try_begin(&coordinator).is_err());
    });
    admitted.wait();
    assert_eq!(fence.cancel(), NativePatchFenceStatus::InFlight);
    assert_eq!(fence.cancel(), NativePatchFenceStatus::InFlight);
    release.wait();
    worker.join().unwrap();
    assert_eq!(fence.cancel(), NativePatchFenceStatus::Fenced);
}

/// A failed or unwinding commit releases scheduling custody without implying
/// rollback or successful mutation. The next independent action can acquire
/// the writer, while no fabricated filesystem outcome is produced by the fence.
#[test]
fn worker_unwind_releases_owned_guard_and_writer_slot() {
    let coordinator = NativePatchWriteCoordinator::default();
    let fence = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
    let worker_coordinator = coordinator.clone();
    let worker = std::thread::spawn(move || {
        let _guard = fence.try_begin(&worker_coordinator).unwrap();
        panic!("synthetic commit worker failure");
    });
    assert!(worker.join().is_err());
    let next = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
    assert!(next.try_begin(&coordinator).is_ok());
}

/// An earlier turn deadline is never extended to the patch budget. A distant
/// turn deadline cannot extend the thirty-second patch ceiling, and elapsed
/// deadlines reject before taking the shared writer slot.
#[test]
fn patch_deadline_is_finite_and_capped_by_the_owning_turn() {
    let coordinator = NativePatchWriteCoordinator::default();
    let expired = Instant::now() - Duration::from_secs(1);
    let fence =
        NativePatchCommitFence::new(NativeActionCancellation::new(), Some(expired)).unwrap();
    assert_eq!(fence.deadline, expired);
    assert!(fence.check().is_err());
    assert!(fence.try_begin(&coordinator).is_err());
    let before = Instant::now();
    let fence = NativePatchCommitFence::new(
        NativeActionCancellation::new(),
        Some(before + Duration::from_secs(600)),
    )
    .unwrap();
    assert!(fence.deadline <= Instant::now() + Duration::from_secs(30));
    assert!(fence.deadline >= before + Duration::from_secs(30));
    assert!(fence.try_begin(&coordinator).is_ok());
}
