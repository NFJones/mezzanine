//! Coherent epoch/row snapshots preserve archive integrity and frozen ownership.
//!
//! Native nonblocking mutation attempts establish exclusion without scheduler
//! timing. Pending rows remain replay-only until their actual archive append.

use super::*;
use crate::storage::transcript::ConversationTranscriptRead;
use rustix::fs::{FlockOperation, flock};

/// An epoch-selected suffix and its pending rows must come from one locked
/// history. An independent writer cannot enter between epoch selection and row
/// decoding. After the snapshot returns, a real append and epoch publication
/// succeed but cannot alter the captured epoch, committed prefix, or logical
/// suffix. A pending row already durable is deduplicated rather than charged as
/// a second chronological occurrence.
#[test]
fn replay_snapshot_keeps_epoch_and_pending_suffix_in_one_mutation_lane() {
    let root = temp_root("coherent-replay");
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=3 {
        store
            .append(&entry("source", sequence, TranscriptRole::Assistant))
            .unwrap();
    }
    store
        .save_compaction_epoch("source", 1, "first summary")
        .unwrap();
    let pending = vec![
        entry("source", 3, TranscriptRole::Assistant),
        entry("source", 4, TranscriptRole::Assistant),
    ];
    let competing = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".conversation-locks/source.lock"))
        .unwrap();
    let snapshot = store
        .conversation_replay_snapshot("source", &pending, |epoch| {
            let epoch = epoch.unwrap();
            assert_eq!(epoch.summary, "first summary");
            assert_eq!(
                flock(&competing, FlockOperation::NonBlockingLockExclusive),
                Err(rustix::io::Errno::WOULDBLOCK)
            );
            Some((
                ConversationTranscriptRead::After(epoch.through_sequence),
                true,
            ))
        })
        .unwrap();
    assert_eq!(
        snapshot
            .view
            .committed
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(
        snapshot
            .view
            .logical
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        [2, 3, 4]
    );
    flock(&competing, FlockOperation::NonBlockingLockExclusive).unwrap();
    drop(competing);
    store.append(&pending[1]).unwrap();
    store
        .save_compaction_epoch("source", 2, "later summary")
        .unwrap();
    assert_eq!(snapshot.epoch.unwrap().summary, "first summary");
    let later = store
        .conversation_replay_snapshot("source", &[], |epoch| {
            Some((
                ConversationTranscriptRead::After(epoch.unwrap().through_sequence),
                true,
            ))
        })
        .unwrap();
    assert_eq!(later.epoch.unwrap().summary, "later summary");
    assert_eq!(
        later
            .view
            .logical
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    fs::remove_dir_all(root).unwrap();
}

/// Coherent reads preserve the checked view's failure rules. A captured pending
/// row cannot replace different durable content at the same sequence, and a
/// missing required archive cannot become a successful receipt-only history.
/// Neither rejection mutates the original row or publishes an epoch.
#[test]
fn replay_snapshot_rejects_conflicting_pending_and_missing_required_history() {
    let root = temp_root("coherent-replay-invalid");
    let store = AgentTranscriptStore::new(root.clone());
    let original = entry("source", 1, TranscriptRole::Assistant);
    store.append(&original).unwrap();
    let mut conflicting = original.clone();
    conflicting.content.push_str(" conflicting pending content");
    assert!(
        store
            .conversation_replay_snapshot("source", &[conflicting], |_| {
                Some((ConversationTranscriptRead::All, true))
            })
            .is_err()
    );
    assert!(
        store
            .conversation_replay_snapshot(
                "missing",
                &[entry("missing", 1, TranscriptRole::User)],
                |_| { Some((ConversationTranscriptRead::All, true)) }
            )
            .is_err()
    );
    assert_eq!(store.inspect("source").unwrap(), [original]);
    assert!(store.compaction_epoch("source").unwrap().is_none());
    fs::remove_dir_all(root).unwrap();
}
