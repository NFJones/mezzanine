//! Summary occurrence identity survives persistence independently of text/order.
//!
//! These tests exercise real epoch files, mutation leases, publication faults
//! and append receipts. Legacy import is an explicit persisted boundary rather
//! than a wildcard equality rule or a new random identity on each read.

use super::*;
use crate::storage::transcript::{
    AgentCompactionEpoch, AgentCompactionRange, TranscriptAppendBatch,
};

/// Builds a genuine old prefix shape with no authored accounting claim.
fn legacy_prefix(conversation_id: &str) -> AgentCompactionEpoch {
    AgentCompactionEpoch {
        version: 1,
        conversation_id: conversation_id.to_string(),
        through_sequence: 0,
        summary: "Durable 雪 decision; keep `x += 1`.".to_string(),
        ranges: Vec::new(),
        summary_records: None,
    }
}

/// Writes an actual old sidecar beside an already valid archive.
fn install_legacy_epoch(root: &std::path::Path, epoch: &AgentCompactionEpoch) -> Vec<u8> {
    let bytes = serde_json::to_vec(epoch).unwrap();
    fs::write(
        root.join(&epoch.conversation_id)
            .join("compaction-epoch.json"),
        &bytes,
    )
    .unwrap();
    bytes
}

/// A pre-rename failure must expose no newly assigned origins. Successful retry
/// migrates once, preserves raw bytes, and every reopened/leased read reuses the
/// exact IDs/caches without recursively acquiring the conversation lock.
#[test]
fn legacy_summary_import_is_atomic_and_stable_through_reopen_and_snapshot() {
    let root = temp_root("legacy-summary-records");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("legacy", 1, TranscriptRole::Assistant))
        .unwrap();
    let archive = fs::read(store.transcript_path("legacy").unwrap()).unwrap();
    let legacy = legacy_prefix("legacy");
    let original = install_legacy_epoch(&root, &legacy);
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(root.join("legacy/compaction-epoch.json"))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    store.fail_next_compaction_epoch_before_rename();
    assert!(store.compaction_epoch("legacy").is_err());
    assert_eq!(
        fs::read(root.join("legacy/compaction-epoch.json")).unwrap(),
        original
    );
    assert!(
        !root
            .join("legacy/.compaction-epoch-accounting.json.tmp")
            .exists()
    );
    let imported = store.compaction_epoch("legacy").unwrap().unwrap();
    assert_eq!(
        fs::metadata(root.join("legacy/compaction-epoch.json"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert_eq!(imported.version, 3);
    assert!(imported.summary_records.as_ref().unwrap().prefix.is_some());
    assert!(imported.matches_expected(&legacy));
    let encoded = fs::read(root.join("legacy/compaction-epoch.json")).unwrap();
    let reopened = AgentTranscriptStore::new(root.clone());
    for _ in 0..3 {
        assert_eq!(
            reopened.compaction_epoch("legacy").unwrap(),
            Some(imported.clone())
        );
        let snapshot = reopened
            .conversation_replay_snapshot("legacy", &[], |_| {
                Some((
                    crate::storage::transcript::ConversationTranscriptRead::All,
                    true,
                ))
            })
            .unwrap();
        assert_eq!(snapshot.epoch, Some(imported.clone()));
    }
    assert_eq!(
        fs::read(root.join("legacy/compaction-epoch.json")).unwrap(),
        encoded
    );
    assert_eq!(
        fs::read(store.transcript_path("legacy").unwrap()).unwrap(),
        archive
    );
    fs::remove_dir_all(root).unwrap();
}

/// Stale method/digest/count fields are descriptive input. Repair only those
/// fields against the real Unicode/code rendering, preserving source, origin,
/// legacy qualification and the exact raw archive.
#[test]
fn summary_cache_import_repairs_authored_counts_without_replacing_occurrence() {
    let root = temp_root("summary-record-cache-repair");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("legacy", 1, TranscriptRole::Assistant))
        .unwrap();
    let legacy = legacy_prefix("legacy");
    install_legacy_epoch(&root, &legacy);
    let expected = store.compaction_epoch("legacy").unwrap().unwrap();
    let archive = fs::read(store.transcript_path("legacy").unwrap()).unwrap();
    let mut value = serde_json::to_value(&expected).unwrap();
    value["summary_records"]["prefix"]["counting_method"] = serde_json::json!(999);
    value["summary_records"]["prefix"]["framed_input_tokens"] = serde_json::json!(usize::MAX);
    value["summary_records"]["prefix"]["native_input_tokens"] = serde_json::json!(0);
    value["summary_records"]["prefix"]["rendered_sha256"] = serde_json::json!(vec![0; 32]);
    fs::write(
        root.join("legacy/compaction-epoch.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert_eq!(
        store.compaction_epoch("legacy").unwrap(),
        Some(expected.clone())
    );
    assert!(expected.matches_expected(&legacy));
    assert_eq!(
        fs::read(store.transcript_path("legacy").unwrap()).unwrap(),
        archive
    );
    let persisted: AgentCompactionEpoch =
        serde_json::from_slice(&fs::read(root.join("legacy/compaction-epoch.json")).unwrap())
            .unwrap();
    assert_eq!(persisted, expected);
    fs::remove_dir_all(root).unwrap();
}

/// Creates complete independent execution groups for two structural summaries.
fn indexed_epoch_fixture() -> (PathBuf, AgentTranscriptStore, AgentCompactionEpoch) {
    let root = temp_root("indexed-summary-records");
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=4 {
        let mut row = entry("indexed", sequence, TranscriptRole::System);
        row.turn_id = format!("group-{}", (sequence - 1) / 2);
        row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            if sequence % 2 == 1 {
                mez_agent::ContextSourceKind::TranscriptAssistant
            } else {
                mez_agent::ContextSourceKind::ActionResult
            },
            "source",
            format!("evidence {sequence}"),
            mez_agent::ContextExecutionGroupId::new(row.turn_id.clone()).unwrap(),
            (sequence - 1) % 2 + 1,
            None,
        )
        .unwrap()
        .to_transcript_content();
        store.append(&row).unwrap();
    }
    let mut epoch = legacy_prefix("indexed");
    epoch.version = 2;
    epoch.ranges = vec![
        AgentCompactionRange {
            first_sequence: 1,
            through_sequence: 2,
            summary: "equal summary".to_string(),
        },
        AgentCompactionRange {
            first_sequence: 3,
            through_sequence: 4,
            summary: "equal summary".to_string(),
        },
    ];
    epoch.allocate_summary_records().unwrap();
    store
        .save_compaction_projection(epoch.clone(), None, None)
        .unwrap();
    (root, store, epoch)
}

/// Equal selective summaries occupy different immutable slots. Missing records,
/// an unsupported format, or a repeated UUID cannot silently allocate new IDs
/// or partially repair a corrupt authority; the original corrupt file remains
/// available for diagnosis rather than being overwritten with guessed state.
#[test]
fn indexed_summary_records_distinguish_equal_text_and_reject_corrupt_slots() {
    let (root, store, epoch) = indexed_epoch_fixture();
    let records = epoch.summary_records.as_ref().unwrap();
    assert_ne!(
        records.ranges[0].occurrence_id(),
        records.ranges[1].occurrence_id()
    );
    assert_eq!(
        records.ranges[0].estimated_input_tokens(),
        records.ranges[1].estimated_input_tokens()
    );
    assert_eq!(
        store.compaction_epoch("indexed").unwrap(),
        Some(epoch.clone())
    );
    for corruption in [
        "missing",
        "duplicate",
        "slots",
        "record-version",
        "epoch-version",
    ] {
        let mut value = serde_json::to_value(&epoch).unwrap();
        match corruption {
            "missing" => {
                value.as_object_mut().unwrap().remove("summary_records");
            }
            "duplicate" => {
                value["summary_records"]["ranges"][1]["occurrence_id"] =
                    value["summary_records"]["ranges"][0]["occurrence_id"].clone()
            }
            "slots" => {
                value["summary_records"]["ranges"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            "record-version" => {
                value["summary_records"]["prefix"]["format_version"] = serde_json::json!(999)
            }
            "epoch-version" => value["version"] = serde_json::json!(999),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(root.join("indexed/compaction-epoch.json"), &bytes).unwrap();
        assert!(store.compaction_epoch("indexed").is_err(), "{corruption}");
        assert_eq!(
            fs::read(root.join("indexed/compaction-epoch.json")).unwrap(),
            bytes
        );
    }
    fs::remove_dir_all(root).unwrap();
}

/// Qualified equality exists only for the actual migrated legacy authority.
/// A later logical publication with identical body/boundaries has new origins
/// and cannot impersonate that old snapshot or the preceding typed epoch.
#[test]
fn new_equal_text_summary_cannot_satisfy_preimport_or_prior_typed_authority() {
    let root = temp_root("equal-summary-authority");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("legacy", 1, TranscriptRole::Assistant))
        .unwrap();
    let old = legacy_prefix("legacy");
    install_legacy_epoch(&root, &old);
    let imported = store.compaction_epoch("legacy").unwrap().unwrap();
    let mut fresh = old.clone();
    fresh.allocate_summary_records().unwrap();
    store
        .save_compaction_projection(fresh.clone(), None, None)
        .unwrap();
    let current = store.compaction_epoch("legacy").unwrap().unwrap();
    assert_eq!(current, fresh);
    assert_eq!(current.summary, imported.summary);
    assert_ne!(
        current.summary_records.as_ref().unwrap().prefix,
        imported.summary_records.as_ref().unwrap().prefix
    );
    assert!(!current.matches_expected(&old));
    assert!(!current.matches_expected(&imported));
    assert!(!imported.matches_expected(&current));
    fs::remove_dir_all(root).unwrap();
}

/// A new initial receipt commits the original typed records through a
/// sidecar-before-marker failure, restart and lost-ack retry. Equal text with
/// different proposal IDs is conflicting authority, not an idempotent retry.
/// Authored stale costs remain in the exact receipt payload, while committed
/// authority restores the correct cache under the same original occurrence.
#[test]
fn typed_initial_receipt_preserves_summary_records_through_restart_and_retry() {
    let root = temp_root("typed-summary-receipt");
    let store = AgentTranscriptStore::new(root.clone());
    let mut proposal = legacy_prefix("child");
    proposal.allocate_summary_records().unwrap();
    let rows = vec![entry("child", 1, TranscriptRole::Assistant)];
    let mut stale = serde_json::to_value(&proposal).unwrap();
    stale["summary_records"]["prefix"]["counting_method"] = serde_json::json!(999);
    stale["summary_records"]["prefix"]["framed_input_tokens"] = serde_json::json!(usize::MAX);
    let stale: AgentCompactionEpoch = serde_json::from_value(stale).unwrap();
    let batch = TranscriptAppendBatch::initial_projection(stale, rows.clone()).unwrap();
    store.fail_next_compaction_epoch_before_marker();
    assert!(store.accept_transcript_append_receipt(&batch, 9).is_err());
    let reopened = AgentTranscriptStore::new(root.clone());
    assert_eq!(
        reopened.compaction_epoch("child").unwrap(),
        Some(proposal.clone())
    );
    for _ in 0..2 {
        reopened
            .accept_transcript_append_receipt(&batch, 9)
            .unwrap();
    }
    assert_eq!(
        reopened.compaction_epoch("child").unwrap(),
        Some(proposal.clone())
    );
    let mut other = legacy_prefix("child");
    other.allocate_summary_records().unwrap();
    let conflict = TranscriptAppendBatch::initial_projection(other, rows.clone()).unwrap();
    assert!(
        reopened
            .accept_transcript_append_receipt(&conflict, 10)
            .is_err()
    );
    assert_eq!(reopened.compaction_epoch("child").unwrap(), Some(proposal));
    reopened
        .settle_transcript_append_receipt(&batch, 9)
        .unwrap();
    assert!(reopened.pending_append_receipts().unwrap().is_empty());
    assert_eq!(reopened.inspect("child").unwrap(), rows);
    fs::remove_dir_all(root).unwrap();
}

/// An actual unaccounted old receipt imports once and remains retryable against
/// exactly its recorded legacy projection. It cannot demand new origins on
/// each replay or accidentally turn new equal-text publication into its owner.
#[test]
fn legacy_initial_receipt_reuses_its_qualified_imported_summary_record() {
    let root = temp_root("legacy-summary-receipt");
    let store = AgentTranscriptStore::new(root.clone());
    let legacy = legacy_prefix("child");
    let batch = TranscriptAppendBatch::initial_projection(
        legacy.clone(),
        vec![entry("child", 1, TranscriptRole::Assistant)],
    )
    .unwrap();
    store.accept_transcript_append_receipt(&batch, 9).unwrap();
    let imported = store.compaction_epoch("child").unwrap().unwrap();
    assert!(imported.matches_expected(&legacy));
    for _ in 0..2 {
        store.accept_transcript_append_receipt(&batch, 9).unwrap();
    }
    assert_eq!(store.compaction_epoch("child").unwrap(), Some(imported));
    store.settle_transcript_append_receipt(&batch, 9).unwrap();
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Adding record metadata must not reject a valid legacy sidecar near its old
/// encoded bound. The v3 metadata allowance expands, while actual summary text
/// still respects the original one-MiB limit and is never truncated on import.
#[test]
fn summary_record_import_preserves_legacy_epoch_near_encoded_size_limit() {
    let root = temp_root("summary-record-import-size");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("legacy", 1, TranscriptRole::Assistant))
        .unwrap();
    let mut legacy = legacy_prefix("legacy");
    legacy.summary.clear();
    let framing = serde_json::to_vec(&legacy).unwrap().len();
    legacy.summary = "x"
        .repeat(crate::storage::transcript::store::COMPACTION_EPOCH_MAX_BYTES as usize - framing);
    let old = install_legacy_epoch(&root, &legacy);
    assert!(old.len() as u64 <= crate::storage::transcript::store::COMPACTION_EPOCH_MAX_BYTES);
    let imported = store.compaction_epoch("legacy").unwrap().unwrap();
    assert_eq!(imported.summary, legacy.summary);
    assert!(imported.matches_expected(&legacy));
    let size = fs::metadata(root.join("legacy/compaction-epoch.json"))
        .unwrap()
        .len();
    assert!(size > crate::storage::transcript::store::COMPACTION_EPOCH_MAX_BYTES);
    assert_eq!(store.compaction_epoch("legacy").unwrap(), Some(imported));
    fs::remove_dir_all(root).unwrap();
}

/// A legacy parent's summary IDs are assigned and durably captured before
/// copying, so the child shares those actual origins. Descriptive migration
/// preserves parent summary authority and exact raw archive while the target
/// receives its own correctly rendered prefix cache and independent identity.
#[test]
fn legacy_fork_capture_preserves_summary_origins_and_parent_source_authority() {
    let root = temp_root("legacy-summary-fork");
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=2 {
        store
            .append(&entry("legacy", sequence, TranscriptRole::Assistant))
            .unwrap();
    }
    let mut legacy = legacy_prefix("legacy");
    legacy.through_sequence = 1;
    install_legacy_epoch(&root, &legacy);
    let original = fs::read(store.transcript_path("legacy").unwrap()).unwrap();
    store.fork("legacy", "child", 9).unwrap();
    let parent = store.compaction_epoch("legacy").unwrap().unwrap();
    let child = store.compaction_epoch("child").unwrap().unwrap();
    assert!(parent.matches_expected(&legacy));
    assert_eq!(
        fs::read(store.transcript_path("legacy").unwrap()).unwrap(),
        original
    );
    let source = parent.summary_records.as_ref().unwrap().prefix.unwrap();
    let copied = child.summary_records.as_ref().unwrap().prefix.unwrap();
    assert_eq!(source.occurrence_id(), copied.occurrence_id());
    assert_eq!(parent.summary, child.summary);
    assert_eq!(parent.through_sequence, child.through_sequence);
    let reopened = AgentTranscriptStore::new(root.clone());
    assert_eq!(reopened.compaction_epoch("legacy").unwrap(), Some(parent));
    assert_eq!(reopened.compaction_epoch("child").unwrap(), Some(child));
    fs::remove_dir_all(root).unwrap();
}
