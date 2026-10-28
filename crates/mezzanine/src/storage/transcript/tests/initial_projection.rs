//! An initial child projection is durable append ownership, not optional cache.
//!
//! Real storage APIs must retain rows and summary authority together through
//! admission, uncertainty and restart. Empty tails use no invented raw rows.

use super::*;
use crate::storage::transcript::{AgentCompactionEpoch, TranscriptAppendBatch};

/// Builds one independently owned initial prefix for an uncompacted child tail.
fn epoch(conversation_id: &str) -> AgentCompactionEpoch {
    AgentCompactionEpoch {
        version: 1,
        conversation_id: conversation_id.into(),
        through_sequence: 0,
        summary: "INHERITED_PREFIX".into(),
        ranges: vec![],
    }
}

/// A producer may return success only after its initial projection and rows are
/// readable together. Losing this sidecar silently resends or drops context.
#[test]
fn initial_projection_admission_publishes_complete_rows_and_summary() {
    let root = temp_root("initial-projection-admission");
    let store = AgentTranscriptStore::new(root.clone());
    let rows = vec![entry("child", 1, TranscriptRole::Assistant)];
    let expected = epoch("child");
    let batch = TranscriptAppendBatch::initial_projection(expected.clone(), rows.clone()).unwrap();
    store.accept_transcript_append_receipt(&batch, 1).unwrap();
    assert_eq!(store.compaction_epoch("child").unwrap(), Some(expected));
    assert_eq!(store.inspect("child").unwrap(), rows);
    fs::remove_dir_all(root).unwrap();
}

/// An inherited summary may be the entire source. Admission must publish its
/// legitimate empty archive and exact epoch without manufacturing a row merely
/// to obtain a receipt identity or advance a conversation frontier.
#[test]
fn initial_projection_admission_preserves_genuinely_empty_raw_tail() {
    let root = temp_root("initial-projection-empty");
    let store = AgentTranscriptStore::new(root.clone());
    let expected = epoch("empty-child");
    let batch = TranscriptAppendBatch::initial_projection(expected.clone(), vec![]).unwrap();
    assert_eq!(batch.first_sequence(), 0);
    store.accept_transcript_append_receipt(&batch, 2).unwrap();
    assert_eq!(
        store.compaction_epoch("empty-child").unwrap(),
        Some(expected)
    );
    assert!(store.inspect("empty-child").unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Builds complete typed source for a selective replacement at the beginning
/// of a new child archive; user/control rows are never invented or summarized.
fn selective_batch() -> TranscriptAppendBatch {
    let rows = (1..=2)
        .map(|sequence| {
            let mut row = entry("selective-child", sequence, TranscriptRole::System);
            row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                mez_agent::ContextSourceKind::TranscriptAssistant,
                "assistant",
                format!("original exact result {sequence}"),
                mez_agent::ContextExecutionGroupId::new("initial-owned-group").unwrap(),
                sequence,
                None,
            )
            .unwrap()
            .to_transcript_content();
            row
        })
        .collect();
    let mut expected = epoch("selective-child");
    expected.version = 2;
    expected.ranges = vec![crate::storage::transcript::AgentCompactionRange {
        first_sequence: 1,
        through_sequence: 2,
        summary: "SELECTIVE_AT_ORIGINAL_POSITION".into(),
    }];
    TranscriptAppendBatch::initial_projection(expected, rows).unwrap()
}

/// Publication preserves selective source identities and exact summary bytes.
/// An identical lost-ack retry stays idempotent, and complete settlement removes
/// only its receipt while leaving both projection and physical rows unchanged.
#[test]
fn initial_projection_selective_retry_and_settlement_are_exact() {
    let root = temp_root("initial-projection-selective");
    let store = AgentTranscriptStore::new(root.clone());
    let batch = selective_batch();
    for _ in 0..2 {
        store.accept_transcript_append_receipt(&batch, 7).unwrap();
    }
    assert_eq!(
        store.pending_append_receipts().unwrap(),
        vec![batch.entries().to_vec()]
    );
    assert_eq!(
        store.inspect(batch.conversation_id()).unwrap(),
        batch.entries()
    );
    assert_eq!(
        store
            .compaction_epoch(batch.conversation_id())
            .unwrap()
            .as_ref(),
        batch.initial_compaction_epoch()
    );
    // A row-only worker cannot retire an owner whose digest also commits an
    // initial projection, even though every row is already durable.
    assert!(store.settle_append_receipt(batch.entries(), 7).is_err());
    for _ in 0..2 {
        store.settle_transcript_append_receipt(&batch, 7).unwrap();
    }
    assert!(store.pending_append_receipts().unwrap().is_empty());
    let reopened = AgentTranscriptStore::new(root.clone());
    assert_eq!(
        reopened.inspect(batch.conversation_id()).unwrap(),
        batch.entries()
    );
    assert_eq!(
        reopened
            .compaction_epoch(batch.conversation_id())
            .unwrap()
            .as_ref(),
        batch.initial_compaction_epoch()
    );
    fs::remove_dir_all(root).unwrap();
}

/// A failure after raw rows or sidecar rename is not an admission ACK. The
/// journal remains the complete recovery source; restart repairs its own exact
/// prefix, sidecar and required-state marker without duplicating a raw row.
#[test]
fn initial_projection_restart_recovers_each_partial_publication_boundary() {
    for boundary in ["row-summary", "sidecar-rename", "required-marker"] {
        let root = temp_root(boundary);
        let store = AgentTranscriptStore::new(root.clone());
        let batch = selective_batch();
        match boundary {
            "row-summary" => store.fail_transcript_append_before_summary(),
            "sidecar-rename" => store.fail_next_compaction_epoch_before_rename(),
            "required-marker" => store.fail_next_compaction_epoch_before_marker(),
            _ => unreachable!(),
        }
        assert!(store.accept_transcript_append_receipt(&batch, 8).is_err());
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![batch.entries().to_vec()]
        );
        assert!(store.settle_transcript_append_receipt(&batch, 8).is_err());
        let reopened = AgentTranscriptStore::new(root.clone());
        reopened.recover_append_receipts().unwrap();
        assert!(reopened.pending_append_receipts().unwrap().is_empty());
        assert_eq!(
            reopened.inspect(batch.conversation_id()).unwrap(),
            batch.entries()
        );
        assert_eq!(
            reopened
                .compaction_epoch(batch.conversation_id())
                .unwrap()
                .as_ref(),
            batch.initial_compaction_epoch()
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// Metadata-only receipt identity remains recoverable and reserves its exact
/// conversation for fork fencing. Its zero key never becomes an archive row.
#[test]
fn initial_projection_empty_receipt_survives_restart_and_reserves_identity() {
    let root = temp_root("initial-projection-empty-recovery");
    let store = AgentTranscriptStore::new(root.clone());
    let batch = TranscriptAppendBatch::initial_projection(epoch("empty-child"), vec![]).unwrap();
    store.fail_next_compaction_epoch_before_marker();
    assert!(store.accept_transcript_append_receipt(&batch, 9).is_err());
    let receipt_path = fs::read_dir(root.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        receipt_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("-initial-")
    );
    let error = store.fork("empty-child", "other-child", 1).unwrap_err();
    assert!(error.message().contains("append receipts"), "{error}");
    let reopened = AgentTranscriptStore::new(root.clone());
    reopened.recover_append_receipts().unwrap();
    assert!(reopened.inspect("empty-child").unwrap().is_empty());
    assert_eq!(
        reopened.compaction_epoch("empty-child").unwrap(),
        Some(epoch("empty-child"))
    );
    let snapshot = reopened
        .lock_resume_replay_snapshot("empty-child", true)
        .unwrap();
    assert_eq!(snapshot.position.logical_high_water, 0);
    assert_eq!(snapshot.position.active_raw_entries, 0);
    drop(snapshot);
    assert!(reopened.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Another summary cannot overwrite a journal-owned initial projection even
/// when all row bytes match. Neither wrong-payload settlement nor recovery may
/// erase the original receipt or replace a newer independent durable epoch.
#[test]
fn initial_projection_rejects_changed_payload_and_durable_authority() {
    let root = temp_root("initial-projection-authority-conflict");
    let store = AgentTranscriptStore::new(root.clone());
    let batch = selective_batch();
    store.accept_transcript_append_receipt(&batch, 10).unwrap();
    let mut changed = batch.initial_compaction_epoch().unwrap().clone();
    changed.summary = "DIFFERENT_AUTHORITY".into();
    let changed =
        TranscriptAppendBatch::initial_projection(changed, batch.entries().to_vec()).unwrap();
    assert!(
        store
            .accept_transcript_append_receipt(&changed, 11)
            .is_err()
    );
    assert!(
        store
            .settle_transcript_append_receipt(&changed, 10)
            .is_err()
    );
    assert_eq!(
        store
            .compaction_epoch(batch.conversation_id())
            .unwrap()
            .as_ref(),
        batch.initial_compaction_epoch()
    );
    store
        .save_compaction_ranges(
            batch.conversation_id(),
            0,
            "INDEPENDENT_NEWER_SUMMARY",
            batch.initial_compaction_epoch().unwrap().ranges.clone(),
        )
        .unwrap();
    assert!(store.recover_append_receipts().is_err());
    assert_eq!(
        store
            .compaction_epoch(batch.conversation_id())
            .unwrap()
            .unwrap()
            .summary,
        "INDEPENDENT_NEWER_SUMMARY"
    );
    assert_eq!(
        store.pending_append_receipts().unwrap(),
        vec![batch.entries().to_vec()]
    );
    fs::remove_dir_all(root).unwrap();
}

/// Existing raw history without this initial receipt is not permission to
/// invent a summary authority. A rejected first claim leaves every file intact
/// and does not create a recovery journal that could later adopt that history.
#[test]
fn initial_projection_cannot_adopt_unowned_existing_raw_archive() {
    let root = temp_root("initial-projection-unowned-raw");
    let store = AgentTranscriptStore::new(root.clone());
    let row = entry("child", 1, TranscriptRole::Assistant);
    store.append(&row).unwrap();
    let batch =
        TranscriptAppendBatch::initial_projection(epoch("child"), vec![row.clone()]).unwrap();
    assert!(store.accept_transcript_append_receipt(&batch, 12).is_err());
    assert!(store.compaction_epoch("child").unwrap().is_none());
    assert_eq!(store.inspect("child").unwrap(), vec![row]);
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Digest validation covers projection bytes as well as rows. Corrupting only
/// a summary must reject the whole recovery snapshot before an unrelated valid
/// v1 receipt is appended; all original recovery evidence remains inspectable.
#[test]
fn initial_projection_digest_tampering_blocks_global_recovery_before_mutation() {
    let root = temp_root("initial-projection-digest");
    let store = AgentTranscriptStore::new(root.clone());
    let batch = selective_batch();
    store.fail_next_compaction_epoch_before_rename();
    assert!(store.accept_transcript_append_receipt(&batch, 13).is_err());
    let directory = root.join(".append-receipts");
    let path = fs::read_dir(&directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(receipt["version"], 2);
    receipt["compaction_epoch"]["summary"] = "TAMPERED_ONLY_METADATA".into();
    fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let other = entry("unrelated", 1, TranscriptRole::Assistant);
    store
        .accept_append_receipt(std::slice::from_ref(&other), 14)
        .unwrap();
    let before = fs::read(store.transcript_path(batch.conversation_id()).unwrap()).unwrap();
    assert!(store.recover_append_receipts().is_err());
    assert!(!store.transcript_path("unrelated").unwrap().exists());
    assert_eq!(
        fs::read(store.transcript_path(batch.conversation_id()).unwrap()).unwrap(),
        before
    );
    assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
    fs::remove_dir_all(root).unwrap();
}

/// Existing row-only journals keep their v1 format and recovery behavior while
/// projection-bearing receipts recover beside them. Version two is required
/// only for the complete initial payload, not a gratuitous global rewrite.
#[test]
fn initial_projection_recovery_preserves_ordinary_v1_receipts() {
    let root = temp_root("initial-projection-with-legacy");
    let store = AgentTranscriptStore::new(root.clone());
    let ordinary = entry("ordinary", 1, TranscriptRole::Assistant);
    store
        .accept_append_receipt(std::slice::from_ref(&ordinary), 15)
        .unwrap();
    let path = fs::read_dir(root.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let receipt: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(receipt["version"], 1);
    assert!(receipt.get("compaction_epoch").is_none());
    let batch = TranscriptAppendBatch::initial_projection(epoch("empty-child"), vec![]).unwrap();
    store.fail_next_compaction_epoch_before_rename();
    assert!(store.accept_transcript_append_receipt(&batch, 16).is_err());
    let reopened = AgentTranscriptStore::new(root.clone());
    reopened.recover_append_receipts().unwrap();
    assert_eq!(reopened.inspect("ordinary").unwrap(), vec![ordinary]);
    assert!(reopened.inspect("empty-child").unwrap().is_empty());
    assert_eq!(
        reopened.compaction_epoch("empty-child").unwrap(),
        Some(epoch("empty-child"))
    );
    assert!(reopened.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Pure batch validation rejects bad origins, foreign identities, protected
/// selective source and oversized metadata before any store is involved.
#[test]
fn initial_projection_batch_validation_rejects_invalid_complete_payloads() {
    assert!(TranscriptAppendBatch::rows(vec![]).is_err());
    assert!(
        TranscriptAppendBatch::initial_projection(
            epoch("child"),
            vec![entry("other", 1, TranscriptRole::Assistant)]
        )
        .is_err()
    );
    assert!(
        TranscriptAppendBatch::initial_projection(
            epoch("child"),
            vec![entry("child", 2, TranscriptRole::Assistant)]
        )
        .is_err()
    );
    let mut invalid = epoch("child");
    invalid.through_sequence = 1;
    assert!(
        TranscriptAppendBatch::initial_projection(
            invalid,
            vec![entry("child", 1, TranscriptRole::Assistant)]
        )
        .is_err()
    );
    let mut oversized = epoch("child");
    oversized.summary = "x".repeat(1024 * 1024);
    assert!(TranscriptAppendBatch::initial_projection(oversized, vec![]).is_err());
    let mut protected = epoch("child");
    protected.version = 2;
    protected.ranges = vec![crate::storage::transcript::AgentCompactionRange {
        first_sequence: 1,
        through_sequence: 1,
        summary: "cannot hide user".into(),
    }];
    assert!(
        TranscriptAppendBatch::initial_projection(
            protected,
            vec![entry("child", 1, TranscriptRole::User)]
        )
        .is_err()
    );
}

/// An initial projection is not an ordinary append capability. Neither a live
/// initial receipt nor an identical retired epoch allows a different immutable
/// initial payload to introduce new rows into an already initialized child.
#[test]
fn initial_projection_retry_cannot_extend_its_immutable_initial_payload() {
    for retained in [true, false] {
        let root = temp_root("initial-projection-extended-claim");
        let store = AgentTranscriptStore::new(root.clone());
        let batch = selective_batch();
        store.accept_transcript_append_receipt(&batch, 17).unwrap();
        if !retained {
            store.settle_transcript_append_receipt(&batch, 17).unwrap();
        }
        let mut rows = batch.entries().to_vec();
        rows.push(entry(batch.conversation_id(), 3, TranscriptRole::Assistant));
        let changed = TranscriptAppendBatch::initial_projection(
            batch.initial_compaction_epoch().unwrap().clone(),
            rows,
        )
        .unwrap();
        assert!(
            store
                .accept_transcript_append_receipt(&changed, 18)
                .is_err()
        );
        assert_eq!(
            store.inspect(batch.conversation_id()).unwrap(),
            batch.entries()
        );
        assert_eq!(
            store.pending_append_receipts().unwrap().len(),
            usize::from(retained)
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// Two individually valid journals can still disagree about initial authority.
/// Recovery must reject their combined snapshot before an unrelated valid
/// ordinary receipt writes anything; changing generation does not hide conflict.
#[test]
fn initial_projection_global_preflight_rejects_conflicting_valid_receipts() {
    let root = temp_root("initial-projection-global-conflict");
    let other_root = temp_root("initial-projection-other-owner");
    let store = AgentTranscriptStore::new(root.clone());
    let other_store = AgentTranscriptStore::new(other_root.clone());
    let batch = selective_batch();
    store.accept_transcript_append_receipt(&batch, 19).unwrap();
    let mut different = batch.initial_compaction_epoch().unwrap().clone();
    different.summary = "CONFLICTING_COMPLETE_JOURNAL".into();
    let different =
        TranscriptAppendBatch::initial_projection(different, batch.entries().to_vec()).unwrap();
    other_store
        .accept_transcript_append_receipt(&different, 20)
        .unwrap();
    let source = fs::read_dir(other_root.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let target = root
        .join(".append-receipts")
        .join(source.file_name().unwrap());
    fs::copy(source, target).unwrap();
    store
        .accept_append_receipt(&[entry("unrelated", 1, TranscriptRole::Assistant)], 21)
        .unwrap();
    assert!(store.recover_append_receipts().is_err());
    assert!(!store.transcript_path("unrelated").unwrap().exists());
    assert_eq!(
        store
            .compaction_epoch(batch.conversation_id())
            .unwrap()
            .as_ref(),
        batch.initial_compaction_epoch()
    );
    assert_eq!(
        fs::read_dir(root.join(".append-receipts")).unwrap().count(),
        3
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(other_root).unwrap();
}

/// A version-one identity cannot smuggle a projection that its historical
/// digest does not cover, and an unknown version cannot silently shed fields.
/// Journal bytes remain available when either unsupported form is rejected.
#[test]
fn initial_projection_receipt_version_shape_mismatch_fails_closed() {
    for version in [1, 3] {
        let root = temp_root("initial-projection-version");
        let store = AgentTranscriptStore::new(root.clone());
        let batch =
            TranscriptAppendBatch::initial_projection(epoch("empty-child"), vec![]).unwrap();
        store.accept_transcript_append_receipt(&batch, 22).unwrap();
        let path = fs::read_dir(root.join(".append-receipts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        receipt["version"] = version.into();
        fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(store.recover_append_receipts().is_err());
        assert!(path.exists());
        assert_eq!(
            store.compaction_epoch("empty-child").unwrap(),
            Some(epoch("empty-child"))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
