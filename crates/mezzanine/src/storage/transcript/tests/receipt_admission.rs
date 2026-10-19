//! Deterministic exact-conversation receipt admission/retirement regressions.
//!
//! Fixtures use temporary stores and per-handle channel barriers, never physical
//! shell processes or providers. Every competing settlement finishes after the
//! reader enumerates paths and before it can open those paths.

use super::*;
use std::sync::mpsc;
use std::time::Duration;

/// A's lock cannot protect B's receipt pathname. Retire B after A enumerates
/// it, then require A's unrelated valid admission to succeed without a retry.
/// This forces the historical stat/open NotFound rather than hoping for a race.
#[test]
fn transcript_receipt_admission_survives_other_conversation_retirement() {
    let root = temp_root("receipt-enumeration-retirement");
    let store = AgentTranscriptStore::new(root.clone());
    let a = entry("conversation-a", 1, TranscriptRole::User);
    let b = entry("conversation-b", 1, TranscriptRole::User);
    store
        .accept_append_receipt(std::slice::from_ref(&b), 1)
        .unwrap();
    store.append(&b).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, resume_rx);
    let admission = thread::spawn({
        let store = store.clone();
        let a = a.clone();
        move || store.accept_append_receipt(&[a], 1)
    });
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // This separate handle has B's exact lock, not the paused A lock.
    AgentTranscriptStore::new(root.clone())
        .settle_append_receipt(std::slice::from_ref(&b), 1)
        .unwrap();
    resume_tx.send(()).unwrap();
    admission.join().unwrap().unwrap();
    assert_eq!(
        store.pending_append_receipts().unwrap(),
        vec![vec![a.clone()]]
    );
    store.recover_append_receipts().unwrap();
    assert_eq!(store.inspect(&a.conversation_id).unwrap(), vec![a]);
    assert_eq!(store.inspect(&b.conversation_id).unwrap(), vec![b]);
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}
