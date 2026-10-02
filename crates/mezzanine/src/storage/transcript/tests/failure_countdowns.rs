//! Failure-injection countdown regressions for transcript storage.
//!
//! These tests protect shared atomic consumption, exhaustion, and the objective
//! hook's second-read semantics without changing production persistence behavior.

use super::{AgentTranscriptStore, Arc, Barrier, TranscriptRole, entry, fs, temp_root, thread};

/// Configured append failures must be consumed before any row is committed.
/// After exhaustion, retries must succeed and an initially empty counter must
/// remain inert rather than wrap and inject additional failures.
#[test]
fn failure_countdown_append_exhaustion_preserves_retries() {
    let root = temp_root("append-failure-countdown");
    let store = AgentTranscriptStore::new(root.clone());
    let row = entry("countdown", 1, TranscriptRole::User);
    store.fail_transcript_append_attempts(2);
    for _ in 0..2 {
        let error = store.append_many(std::slice::from_ref(&row)).unwrap_err();
        assert!(
            error
                .message()
                .contains("injected consecutive transcript append failure")
        );
        assert!(!root.join("countdown").exists());
    }
    assert!(store.append_many(std::slice::from_ref(&row)).is_ok());
    assert!(store.append_many(&[]).is_ok());
    fs::remove_dir_all(root).unwrap();
}

/// Objective reads fail only on the second read following injection, including
/// reads through cloned stores. Later reads must recover with a zero countdown
/// so metadata failures cannot persist indefinitely after their intended use.
#[test]
fn failure_countdown_objective_fails_only_on_second_read() {
    let root = temp_root("objective-failure-countdown");
    let store = AgentTranscriptStore::new(root);
    assert_eq!(store.user_objective("countdown").unwrap(), None);
    store.fail_second_subsequent_user_objective_read();
    assert_eq!(store.clone().user_objective("countdown").unwrap(), None);
    assert_eq!(store.user_objective_read_failure_countdown(), 1);
    let error = store.user_objective("countdown").unwrap_err();
    assert!(
        error
            .message()
            .contains("injected second user objective metadata read failure")
    );
    assert_eq!(store.user_objective_read_failure_countdown(), 0);
    assert_eq!(store.user_objective("countdown").unwrap(), None);
    assert_eq!(store.user_objective_read_failure_countdown(), 0);
}

/// Cloned stores share one failure budget even when workers race to append.
/// Empty batches isolate the atomic hook from filesystem locking, proving that
/// exactly the configured number of calls fail and all others stay successful.
#[test]
fn failure_countdown_append_is_exact_under_contention() {
    let store = AgentTranscriptStore::new(temp_root("concurrent-failure-countdown"));
    store.fail_transcript_append_attempts(5);
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                (0..4).filter(|_| store.append_many(&[]).is_err()).count()
            })
        })
        .collect();
    let failed: usize = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .sum();
    assert_eq!(failed, 5);
    assert!(store.append_many(&[]).is_ok());
}
