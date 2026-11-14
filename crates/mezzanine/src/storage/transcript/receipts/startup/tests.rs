//! Physical append/receipt boundaries for historical duplicated occurrences.

use super::*;
use mez_agent::transcript::{TranscriptContextEvent, TranscriptContextRecord};
use mez_agent::{ContextBlock, ContextBlockRecord, ContextExecutionGroupId, TranscriptRole};

fn original_row(conversation: &str) -> TranscriptEntry {
    let block = ContextBlock::reference_event(
        mez_agent::ContextSourceKind::TranscriptAssistant,
        "assistant response",
        "original evidence",
    );
    let event = TranscriptContextEvent::execution_block_with_metadata(
        block.source,
        &block.label,
        &block.content,
        ContextExecutionGroupId::new("original-group").unwrap(),
        1,
        None,
    )
    .unwrap();
    TranscriptEntry {
        conversation_id: conversation.into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "original-turn".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: TranscriptContextRecord::new(
            event,
            ContextBlockRecord::allocate(&block).unwrap(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap(),
    }
}

fn store() -> AgentTranscriptStore {
    AgentTranscriptStore::new(std::env::temp_dir().join(format!(
        "mez-startup-receipts-{}",
        String::from(mez_agent::ContextBlockOccurrenceId::allocate())
    )))
}

/// New producers cannot leave either a receipt or raw bytes for an occurrence
/// already committed elsewhere. Exact original retries still settle normally.
#[test]
fn conflicting_occurrence_refuses_before_journal_and_raw_append() {
    let store = store();
    let row = original_row("source");
    let batch = TranscriptAppendBatch::owned_rows(vec![row.clone()]).unwrap();
    store.commit_accounted_transcript_batch(&batch, 1).unwrap();
    assert_eq!(
        store.commit_accounted_transcript_batch(&batch, 1).unwrap(),
        0
    );
    let mut duplicate = row.clone();
    duplicate.sequence = 2;
    duplicate.turn_id = "continued-turn".into();
    let duplicate = TranscriptAppendBatch::owned_rows(vec![duplicate]).unwrap();
    assert!(
        store
            .commit_accounted_transcript_batch(&duplicate, 2)
            .is_err()
    );
    assert_eq!(store.inspect("source").unwrap(), vec![row]);
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(store.root()).unwrap();
}

/// Recreates the old bug after its raw write and before its SQLite commit,
/// including later valid work. Startup preserves the complete poisoned source
/// while settling an unrelated accepted receipt; strict recovery still refuses.
#[test]
fn startup_preserves_conflicting_receipt_and_later_rows_while_recovering_healthy_source() {
    let store = store();
    let original = original_row("broken");
    store
        .commit_accounted_transcript_batch(
            &TranscriptAppendBatch::owned_rows(vec![original.clone()]).unwrap(),
            1,
        )
        .unwrap();
    let mut duplicate = original;
    duplicate.sequence = 2;
    duplicate.turn_id = "continued-turn".into();
    let batch = TranscriptAppendBatch::owned_rows(vec![duplicate.clone()]).unwrap();
    let receipt = AppendReceipt::from_batch(&batch, 2).unwrap();
    let lease = store.acquire_conversation_lock("broken").unwrap();
    // Historical fixture: today's admission must reject this original bug.
    let path = store.transcript_path("broken").unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "{}", encode_transcript_entry(&duplicate).unwrap()).unwrap();
    file.sync_all().unwrap();
    drop(lease);
    let mut later = original_row("broken");
    later.sequence = 3;
    store
        .commit_accounted_transcript_batch(
            &TranscriptAppendBatch::owned_rows(vec![later]).unwrap(),
            3,
        )
        .unwrap();
    // Preserve the original encoder/checksum, bypassing only today's admission
    // guard to seed the old daemon's already-accepted poisoned receipt.
    let directory = store.append_receipt_directory();
    fs::create_dir_all(&directory).unwrap();
    let receipt_path = directory.join(receipt.filename());
    let mut receipt_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&receipt_path)
        .unwrap();
    receipt_file
        .write_all(&serde_json::to_vec(&receipt).unwrap())
        .unwrap();
    receipt_file.sync_all().unwrap();
    fs::File::open(&directory).unwrap().sync_all().unwrap();
    let healthy = original_row("healthy");
    let healthy_batch = TranscriptAppendBatch::owned_rows(vec![healthy.clone()]).unwrap();
    store
        .accept_transcript_append_receipt(&healthy_batch, 4)
        .unwrap();
    let raw = fs::read(&path).unwrap();
    let receipt_bytes = fs::read(&receipt_path).unwrap();
    assert!(store.recover_append_receipts().is_err());
    for _ in 0..2 {
        let failures = store.recover_append_receipts_for_startup().unwrap();
        assert_eq!(
            failures.keys().map(String::as_str).collect::<Vec<_>>(),
            ["broken"]
        );
        assert_eq!(fs::read(&path).unwrap(), raw);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
        assert_eq!(store.inspect("healthy").unwrap(), vec![healthy.clone()]);
    }
    fs::remove_dir_all(store.root()).unwrap();
}

/// A name that cannot identify one conversation cannot be isolated safely.
#[test]
fn startup_rejects_unroutable_receipt_inventory() {
    let store = store();
    fs::create_dir_all(store.append_receipt_directory()).unwrap();
    fs::write(store.append_receipt_directory().join("unknown.json"), b"{}").unwrap();
    assert!(store.recover_append_receipts_for_startup().is_err());
    fs::remove_dir_all(store.root()).unwrap();
}

/// Shared namespace changes after discovery must escape conversation-local
/// refusal handling and prevent replay of a healthy accepted owner.
#[test]
fn startup_rejects_unknown_name_appearing_after_discovery() {
    let store = store();
    let row = original_row("healthy");
    let batch = TranscriptAppendBatch::owned_rows(vec![row]).unwrap();
    store.accept_transcript_append_receipt(&batch, 1).unwrap();
    let (observed, observation) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed, resume);
    let worker = store.clone();
    let recovery = std::thread::spawn(move || worker.recover_append_receipts_for_startup());
    observation
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    fs::write(store.append_receipt_directory().join("unknown.json"), b"{}").unwrap();
    release.send(()).unwrap();
    assert!(recovery.join().unwrap().is_err());
    assert!(!store.transcript_path("healthy").unwrap().exists());
    fs::remove_dir_all(store.root()).unwrap();
}

/// A withheld child can have fully valid history and no pending append. Its
/// durable hold alone must protect it after runtime bindings disappear.
#[test]
fn startup_refusal_hold_protects_independent_child_history_from_retention() {
    let store = store();
    store.append(&original_row("child")).unwrap();
    store.retain_startup_refusal("child").unwrap();
    let reopened = AgentTranscriptStore::new(store.root());
    let report = reopened
        .enforce_saved_session_retention(i64::MAX as u64, &Default::default())
        .unwrap();
    assert!(report.deleted_conversation_ids.is_empty());
    assert!(report.failures.is_empty());
    assert_eq!(reopened.inspect("child").unwrap().len(), 1);
    assert!(
        reopened.delete("child").unwrap(),
        "explicit deletion remains separate from automatic retention"
    );
    fs::remove_dir_all(store.root()).unwrap();
}

/// A worker that passed the early veto must repeat it under the same mutation
/// lease used by refusal publication, before it can return an adoptable snapshot.
#[test]
fn startup_refusal_published_before_resume_lock_prevents_adoption() {
    let store = store();
    store.append(&original_row("source")).unwrap();
    let (observed, observation) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    store.gate_next_presentation_lock_for_tests(observed, resume);
    let worker = store.clone();
    let snapshot = std::thread::spawn(move || {
        worker
            .lock_resume_replay_snapshot("source", true)
            .map(|_| ())
    });
    observation
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    store.retain_startup_refusal("source").unwrap();
    release.send(()).unwrap();
    assert!(snapshot.join().unwrap().is_err());
    fs::remove_dir_all(store.root()).unwrap();
}
