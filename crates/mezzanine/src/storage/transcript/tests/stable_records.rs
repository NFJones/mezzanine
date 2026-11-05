//! Original stable carrier scope and refusal across real archive reads.
//!
//! These controls exercise existing raw validation and receipts without model
//! dispatch. Metadata never borrows a future carrier, bypasses placement checks,
//! or revives a claim retired by a later empty snapshot.

use super::*;
use crate::storage::transcript::{ConversationTranscriptRead, TranscriptAppendBatch};
use mez_agent::{
    AgentContext, ContextBlock, ContextPlacement, ContextSourceKind, StableContextBlockRecord,
    StableContextSlotId,
};

/// Captures a real typed user boundary with one current producer snapshot.
fn snapshot_row(
    cid: &str,
    sequence: u64,
    empty: bool,
) -> (TranscriptEntry, Vec<StableContextBlockRecord>) {
    let mut context = AgentContext::import_durable_blocks_with_stable_sources(
        vec![ContextBlock {
            source: ContextSourceKind::System,
            placement: ContextPlacement::StablePrefix,
            label: "configured agent system prompt".into(),
            content: "same trusted 雪".into(),
        }],
        &[],
        &[],
        &[(
            0,
            StableContextSlotId::new("runtime/configured-system-prompt").unwrap(),
        )],
    )
    .unwrap();
    context
        .append_user_event("user prompt", "original user source")
        .unwrap();
    let event = context.chronology().last().unwrap();
    let slots = if empty {
        Vec::new()
    } else {
        context.stable_block_records()
    };
    let semantic = mez_agent::TranscriptContextEvent::user_event(
        event.sequence().get(),
        "user prompt",
        "original user source",
    )
    .unwrap();
    let mut row = entry(cid, sequence, TranscriptRole::System);
    row.content = mez_agent::transcript::TranscriptContextRecord::new(
        semantic,
        event.record(),
        Some(slots.clone()),
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    (row, slots)
}

/// A prefix-hidden carrier remains available, while a real explicit frontier
/// refuses later metadata even when an after-read contains future physical rows.
/// Through-reads and latest windows share the same original-source observer.
#[test]
fn stable_snapshot_reads_preserve_hidden_carriers_and_original_frontiers() {
    let root = temp_root("stable-source-frontier");
    let store = AgentTranscriptStore::new(&root);
    let (first, old) = snapshot_row("stable-scope", 1, false);
    let (future, later) = snapshot_row("stable-scope", 3, false);
    assert_ne!(old, later);
    let rows = vec![
        first,
        entry("stable-scope", 2, TranscriptRole::Assistant),
        future,
    ];
    store
        .commit_accounted_transcript_batch(&TranscriptAppendBatch::owned_rows(rows).unwrap(), 1)
        .unwrap();
    for read in [
        ConversationTranscriptRead::After(3),
        ConversationTranscriptRead::Latest(1),
    ] {
        let snapshot = store
            .conversation_replay_snapshot("stable-scope", &[], |_| Some((read, true)))
            .unwrap();
        let stable = snapshot.stable_records.unwrap();
        assert_eq!(stable.sequence(), 3);
        assert_eq!(stable.records().unwrap(), later);
    }
    let (snapshot, ()) = store
        .conversation_replay_projection_with_frontier(
            "stable-scope",
            &[],
            Some(2),
            |_| Some((ConversationTranscriptRead::After(1), true)),
            |_| Ok(((), Vec::new())),
        )
        .unwrap();
    assert_eq!(snapshot.view.logical.last().unwrap().sequence, 2);
    assert_eq!(snapshot.view.committed.last().unwrap().sequence, 2);
    let stable = snapshot.stable_records.unwrap();
    assert_eq!(stable.sequence(), 1);
    assert_eq!(stable.records().unwrap(), old);
    let snapshot = store
        .conversation_replay_snapshot("stable-scope", &[], |_| {
            Some((ConversationTranscriptRead::Through(2), true))
        })
        .unwrap();
    assert_eq!(snapshot.stable_records.unwrap().records().unwrap(), old);
    fs::remove_dir_all(root).unwrap();
}

/// A foreign published row origin must reject even a carrier omitted from
/// model rows. Its evidence stays intact, and unrelated archive I/O continues.
#[test]
fn stable_snapshot_rejects_foreign_covered_carrier_placement() {
    let root = temp_root("stable-source-placement");
    let store = AgentTranscriptStore::new(&root);
    let (row, _) = snapshot_row("stable-placement", 1, false);
    store
        .commit_accounted_transcript_batch(
            &TranscriptAppendBatch::owned_rows(vec![row]).unwrap(),
            1,
        )
        .unwrap();
    let database = root.join("stable-placement/row-origins.sqlite3");
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE origins SET origin=?1 WHERE sequence='1'",
            [String::from(mez_agent::ContextBlockOccurrenceId::allocate())],
        )
        .unwrap();
    let before = fs::read(&database).unwrap();
    let path = store.transcript_path("stable-placement").unwrap();
    let raw = fs::read(&path).unwrap();
    let error = store
        .conversation_replay_snapshot("stable-placement", &[], |_| {
            Some((ConversationTranscriptRead::After(1), true))
        })
        .err()
        .unwrap();
    assert_eq!(error.kind(), crate::MezErrorKind::Conflict);
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read(path).unwrap(), raw);
    let independent = entry("stable-independent", 1, TranscriptRole::User);
    store.append(&independent).unwrap();
    assert_eq!(
        store.inspect(&independent.conversation_id).unwrap(),
        vec![independent]
    );
    fs::remove_dir_all(root).unwrap();
}

/// Unknown reserved provenance cannot silently borrow an older snapshot. A
/// newer complete empty snapshot retires it; an older bounded read still uses
/// its own original claim. No bad payload text leaks through the rejection.
#[test]
fn stable_snapshot_unknown_carrier_is_bounded_and_empty_snapshot_retires_claims() {
    let root = temp_root("stable-source-invalid");
    let store = AgentTranscriptStore::new(&root);
    let (first, old) = snapshot_row("stable-invalid", 1, false);
    store
        .commit_accounted_transcript_batch(
            &TranscriptAppendBatch::owned_rows(vec![first]).unwrap(),
            1,
        )
        .unwrap();
    let mut bad = entry("stable-invalid", 2, TranscriptRole::System);
    bad.content = "[mez-transcript-context-record/v99]\nprivate diagnostic source".into();
    store.append(&bad).unwrap();
    let snapshot = store
        .conversation_replay_snapshot("stable-invalid", &[], |_| {
            Some((ConversationTranscriptRead::After(2), true))
        })
        .unwrap();
    let error = snapshot.stable_records.unwrap().records().unwrap_err();
    assert!(!error.message().contains("private diagnostic source"));
    let snapshot = store
        .conversation_replay_snapshot("stable-invalid", &[], |_| {
            Some((ConversationTranscriptRead::Through(1), true))
        })
        .unwrap();
    assert_eq!(snapshot.stable_records.unwrap().records().unwrap(), old);
    let (retired, _) = snapshot_row("stable-invalid", 3, true);
    store
        .commit_accounted_transcript_batch(
            &TranscriptAppendBatch::owned_rows(vec![retired]).unwrap(),
            2,
        )
        .unwrap();
    let snapshot = store
        .conversation_replay_snapshot("stable-invalid", &[], |_| {
            Some((ConversationTranscriptRead::After(3), true))
        })
        .unwrap();
    let stable = snapshot.stable_records.unwrap();
    assert_eq!(stable.sequence(), 3);
    assert!(stable.records().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}
