//! Sequence observations share the caller's existing mutation ownership.
//!
//! Native nonblocking lock attempts prove exclusion without sleep-based timing.
//! The append worker uses the public reconciled batch path; a held lookup must
//! neither acquire a second descriptor nor reserve a future sequence.

use super::*;
use rustix::fs::{FlockOperation, flock};
use std::sync::mpsc;
use std::time::Duration;

/// A sequence read under an existing conversation lock must return promptly,
/// exclude a real append until ownership is released, and observe its complete
/// large row afterward. Repeated reads reserve nothing. The exact batch retry
/// remains idempotent, while a conflicting claimant fails without rewriting
/// history. Independent lock descriptors establish the exclusion condition;
/// channels bound worker completion and every owned thread is joined.
#[test]
fn transcript_sequence_held_lookup_and_competing_append_keep_one_mutation_lane() {
    let root = temp_root("sequence-coherent-append");
    let store = AgentTranscriptStore::new(root.clone());
    let original = entry("source", 1, TranscriptRole::User);
    store.append(&original).unwrap();
    let lease = store.acquire_conversation_lock("source").unwrap();
    let competitor = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".conversation-locks/source.lock"))
        .unwrap();
    assert_eq!(
        flock(&competitor, FlockOperation::NonBlockingLockExclusive),
        Err(rustix::io::Errno::WOULDBLOCK)
    );
    assert_eq!(store.next_sequence("source").unwrap(), 2);
    assert_eq!(store.next_sequence("source").unwrap(), 2);

    let mut next = entry("source", 2, TranscriptRole::Assistant);
    next.content = "complete large competing row\n".repeat(120_000);
    let writer_store = store.clone();
    let writer_row = next.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = writer_store.append_many(&[writer_row]);
        done_tx.send(result).unwrap();
    });
    let started = started_rx.recv_timeout(Duration::from_secs(10));
    let before_release = done_rx.try_recv();
    let held_sequence = store.next_sequence("source");
    drop(lease);
    let completed = done_rx.recv_timeout(Duration::from_secs(10));
    worker.join().unwrap();
    started.unwrap();
    assert!(matches!(before_release, Err(mpsc::TryRecvError::Empty)));
    assert_eq!(held_sequence.unwrap(), 2);
    completed.unwrap().unwrap();
    assert_eq!(store.next_sequence("source").unwrap(), 3);
    assert_eq!(store.append_many(std::slice::from_ref(&next)).unwrap(), 0);
    let mut conflicting = next.clone();
    conflicting.content = "different claimant at an observed sequence".into();
    assert!(store.append_many(&[conflicting]).is_err());
    assert_eq!(store.inspect("source").unwrap(), [original, next]);
    flock(&competitor, FlockOperation::NonBlockingLockExclusive).unwrap();
    drop(competitor);
    fs::remove_dir_all(root).unwrap();
}
