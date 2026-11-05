//! Initial raw origins/costs are exact receipt ownership even without a summary.
//!
//! Storage controls cover full admission, retry, recovery and integrity refusal.
//! Actual child rendering and pane-framing reuse are covered by runtime tests.

use super::*;
use crate::storage::transcript::{
    AgentCompactionEpoch, InitialTranscriptRowAccounting, TranscriptAppendBatch,
};
use mez_agent::{ContextBlock, ContextBlockRecord, ContextPlacement, ContextSourceKind};

/// Builds equal bodies with distinct original records and optional summary.
fn batch(summary: bool, empty: bool) -> TranscriptAppendBatch {
    let rows = if empty {
        Vec::new()
    } else {
        (1..=2)
            .map(|sequence| {
                let mut row = entry("accounted-child", sequence, TranscriptRole::Assistant);
                row.content = "equal inherited source 雪 {\"code\":[1,2,3]}".into();
                row
            })
            .collect::<Vec<_>>()
    };
    let mut origins = BTreeMap::new();
    let mut records = BTreeMap::new();
    for row in &rows {
        let record = ContextBlockRecord::allocate(&ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: ContextPlacement::ConversationAppend,
            label: "original inherited source".into(),
            content: row.content.clone(),
        })
        .unwrap();
        origins.insert(row.sequence, record.occurrence_id());
        records.insert(row.sequence, record);
    }
    let batch = if summary {
        let mut epoch = AgentCompactionEpoch {
            summary_records: None,
            version: 1,
            conversation_id: "accounted-child".into(),
            through_sequence: 0,
            summary: "INHERITED_HEAD".into(),
            ranges: vec![],
        };
        epoch.allocate_summary_records().unwrap();
        TranscriptAppendBatch::initial_projection(epoch, rows).unwrap()
    } else {
        TranscriptAppendBatch::rows(rows).unwrap()
    };
    batch
        .with_initial_row_accounting(InitialTranscriptRowAccounting::new(origins, records))
        .unwrap()
}

/// Inspects the same leased public read used by actual canonical replay.
fn assert_accounting(store: &AgentTranscriptStore, batch: &TranscriptAppendBatch) {
    let snapshot = store
        .conversation_replay_snapshot(batch.conversation_id(), &[], |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        })
        .unwrap();
    let expected = batch.initial_row_accounting().unwrap();
    assert_eq!(snapshot.view.logical, batch.entries());
    assert_eq!(&snapshot.row_origins, expected.origins());
    assert_eq!(&snapshot.row_records, expected.records());
}

/// A summary-free initial append still owns its original IDs and costs before
/// readiness. Exact clones retry/settle; another equal-body proposal cannot adopt
/// the old target merely because the original journal has already retired.
#[test]
fn initial_accounting_without_summary_publishes_and_settles_exact_owner() {
    let root = temp_root("initial-accounting-no-head");
    let store = AgentTranscriptStore::new(&root);
    let batch = batch(false, false);
    store.accept_transcript_append_receipt(&batch, 1).unwrap();
    assert_accounting(&store, &batch);
    store
        .accept_transcript_append_receipt(&batch.clone(), 1)
        .unwrap();
    store.settle_transcript_append_receipt(&batch, 1).unwrap();
    store.settle_transcript_append_receipt(&batch, 1).unwrap();
    let changed = batch
        .clone()
        .with_initial_row_accounting(InitialTranscriptRowAccounting::new(
            batch.initial_row_accounting().unwrap().origins().clone(),
            batch.initial_row_accounting().unwrap().records().clone(),
        ))
        .unwrap();
    assert!(store.accept_transcript_append_receipt(&changed, 2).is_err());
    assert!(store.settle_transcript_append_receipt(&changed, 2).is_err());
    assert_accounting(&store, &batch);
    fs::remove_dir_all(root).unwrap();
}

/// Raw/ledger/epoch failure boundaries keep one immutable recovery owner. Cold
/// recovery restores original placements/costs and never duplicates source rows.
#[test]
fn initial_accounting_recovers_all_existing_initial_publication_boundaries() {
    for boundary in [
        "row-summary",
        "sidecar-rename",
        "required-marker",
        "empty-marker",
        "row-marker",
        "empty-row-marker",
    ] {
        let root = temp_root(boundary);
        let store = AgentTranscriptStore::new(&root);
        let batch = batch(true, boundary.starts_with("empty"));
        match boundary {
            "row-summary" => store.fail_transcript_append_before_summary(),
            "sidecar-rename" => store.fail_next_compaction_epoch_before_rename(),
            "row-marker" | "empty-row-marker" => store.fail_next_metadata_write_after_promotion(),
            _ => store.fail_next_compaction_epoch_before_marker(),
        }
        assert!(store.accept_transcript_append_receipt(&batch, 3).is_err());
        assert!(store.settle_transcript_append_receipt(&batch, 3).is_err());
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            [batch.entries().to_vec()]
        );
        let reopened = AgentTranscriptStore::new(&root);
        reopened.recover_append_receipts().unwrap();
        assert!(reopened.pending_append_receipts().unwrap().is_empty());
        assert_accounting(&reopened, &batch);
        assert_eq!(
            reopened
                .compaction_epoch(batch.conversation_id())
                .unwrap()
                .as_ref(),
            batch.initial_compaction_epoch()
        );
        reopened.recover_append_receipts().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

/// Missing cost entries can recover only through their retained original owner.
/// Existing newer framing caches remain intact; a retry is not an instruction
/// to overwrite a descriptive revision with its older initial estimate.
#[test]
fn initial_accounting_retry_repairs_missing_costs_without_replacing_newer_cache() {
    let root = temp_root("initial-accounting-cost-repair");
    let store = AgentTranscriptStore::new(&root);
    let batch = batch(false, false);
    store.accept_transcript_append_receipt(&batch, 4).unwrap();
    let records = batch.initial_row_accounting().unwrap().records();
    let revised = records[&1]
        .revised(&ContextBlock::reference_event(
            ContextSourceKind::TranscriptAssistant,
            "new framing",
            "new rendering",
        ))
        .unwrap();
    let connection =
        rusqlite::Connection::open(root.join("accounted-child/row-origins.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE row_costs SET record=?1 WHERE origin=?2",
            rusqlite::params![
                serde_json::to_string(&revised).unwrap(),
                String::from(revised.occurrence_id()),
            ],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM row_costs WHERE origin=?1",
            [String::from(records[&2].occurrence_id())],
        )
        .unwrap();
    drop(connection);
    assert!(store.settle_transcript_append_receipt(&batch, 4).is_err());
    let reopened = AgentTranscriptStore::new(&root);
    reopened.recover_append_receipts().unwrap();
    let snapshot = reopened
        .conversation_replay_snapshot("accounted-child", &[], |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        })
        .unwrap();
    assert_eq!(snapshot.row_records[&1], revised);
    assert_eq!(snapshot.row_records[&2], records[&2]);
    assert!(reopened.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// The digest binds accounting-only changes and version5 header shape. Global
/// recovery refuses before replaying an unrelated otherwise valid row journal.
#[test]
fn initial_accounting_tampering_blocks_recovery_without_partial_mutation() {
    for field in ["namespace", "origin", "cost", "version"] {
        let root = temp_root(field);
        let store = AgentTranscriptStore::new(&root);
        let batch = batch(false, false);
        store.accept_transcript_append_receipt(&batch, 5).unwrap();
        let path = fs::read_dir(root.join(".append-receipts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(receipt["version"], 5);
        match field {
            "namespace" => {
                receipt["initial_row_accounting"]["namespace"] =
                    String::from(mez_agent::ContextBlockOccurrenceId::allocate()).into()
            }
            "origin" => {
                receipt["initial_row_accounting"]["origins"]["1"] =
                    String::from(mez_agent::ContextBlockOccurrenceId::allocate()).into()
            }
            "cost" => {
                receipt["initial_row_accounting"]["records"]["1"]["framed_input_tokens"] =
                    99999.into()
            }
            _ => receipt["version"] = 2.into(),
        }
        fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        store
            .accept_append_receipt(&[entry("unrelated", 1, TranscriptRole::Assistant)], 6)
            .unwrap();
        let before = fs::read(store.transcript_path(batch.conversation_id()).unwrap()).unwrap();
        assert!(store.recover_append_receipts().is_err());
        assert!(!store.transcript_path("unrelated").unwrap().exists());
        assert_eq!(
            fs::read(store.transcript_path(batch.conversation_id()).unwrap()).unwrap(),
            before
        );
        assert_eq!(
            fs::read_dir(root.join(".append-receipts")).unwrap().count(),
            2
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// Independently valid conflicting initial proposals are detected across the
/// complete snapshot, including summary-free owners, before unrelated recovery.
#[test]
fn initial_accounting_global_preflight_refuses_different_valid_originals() {
    let root = temp_root("initial-accounting-conflict");
    let other = temp_root("initial-accounting-foreign");
    let store = AgentTranscriptStore::new(&root);
    let foreign = AgentTranscriptStore::new(&other);
    let first = batch(false, false);
    let second = batch(false, false);
    store.accept_transcript_append_receipt(&first, 7).unwrap();
    foreign
        .accept_transcript_append_receipt(&second, 8)
        .unwrap();
    let path = fs::read_dir(other.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::copy(
        &path,
        root.join(".append-receipts")
            .join(path.file_name().unwrap()),
    )
    .unwrap();
    store
        .accept_append_receipt(&[entry("unrelated", 1, TranscriptRole::Assistant)], 9)
        .unwrap();
    assert!(store.recover_append_receipts().is_err());
    assert!(!store.transcript_path("unrelated").unwrap().exists());
    assert_accounting(&store, &first);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(other).unwrap();
}

/// Matching physical bytes do not prove inherited identity. A new initial
/// proposal cannot take over an ordinary already-existing unowned archive.
#[test]
fn initial_accounting_cannot_adopt_unowned_equal_raw_archive() {
    let root = temp_root("initial-accounting-unowned");
    let store = AgentTranscriptStore::new(&root);
    let batch = batch(false, false);
    store.append_many(batch.entries()).unwrap();
    let path = root.join("accounted-child/row-origins.sqlite3");
    let before = fs::read(&path).unwrap();
    assert!(store.accept_transcript_append_receipt(&batch, 10).is_err());
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(!root.join(".append-receipts").exists());
    fs::remove_dir_all(root).unwrap();
}

/// Pure batch validation requires every original position and the source's
/// authored identity. Unknown formats and changed typed UUIDs cannot reach I/O;
/// a literal user marker supplies no authored ID or execution authority.
#[test]
fn initial_accounting_validation_checks_complete_positions_and_authored_identity() {
    let original = batch(false, false);
    let accounting = original.initial_row_accounting().unwrap().clone();
    let mut rows = original.entries().to_vec();
    let record = accounting.records()[&1];
    let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        ContextSourceKind::TranscriptAssistant,
        "original inherited source",
        rows[0].content.clone(),
        mez_agent::ContextExecutionGroupId::new("inherited-authored").unwrap(),
        1,
        None,
    )
    .unwrap();
    rows[0].role = TranscriptRole::System;
    rows[0].content = mez_agent::transcript::TranscriptContextRecord::new(event, record, None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    assert!(
        TranscriptAppendBatch::rows(rows.clone())
            .unwrap()
            .with_initial_row_accounting(accounting.clone())
            .is_ok()
    );
    for fault in ["missing", "version", "authored"] {
        let mut encoded = serde_json::to_value(&accounting).unwrap();
        match fault {
            "missing" => {
                encoded["origins"].as_object_mut().unwrap().remove("2");
            }
            "version" => encoded["version"] = 2.into(),
            _ => {
                let foreign = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
                encoded["origins"]["1"] = foreign.clone().into();
                encoded["records"]["1"]["occurrence_id"] = foreign.into();
            }
        }
        let invalid = serde_json::from_value(encoded).unwrap();
        assert!(
            TranscriptAppendBatch::rows(rows.clone())
                .unwrap()
                .with_initial_row_accounting(invalid)
                .is_err(),
            "{fault}"
        );
    }
    rows[0].role = TranscriptRole::User;
    rows[0].content = "[mez-transcript-context-record/v1]\n{literal invalid metadata}".into();
    assert!(
        TranscriptAppendBatch::rows(rows)
            .unwrap()
            .with_initial_row_accounting(accounting)
            .is_ok()
    );
}

/// An immutable marker does not prove its mutable published placement/cost state
/// still agrees. Detect these nonrepairable conflicts before a sorted recovery
/// loop can append or retire an earlier conversation's otherwise valid receipt.
/// Missing costs and incomplete required markers remain separately recoverable.
#[test]
fn initial_accounting_global_preflight_checks_published_state_before_unrelated_replay() {
    for fault in ["placement", "cache-identity", "cache-format", "schema"] {
        let root = temp_root(fault);
        let store = AgentTranscriptStore::new(&root);
        let batch = batch(false, false);
        store.accept_transcript_append_receipt(&batch, 11).unwrap();
        let database = root.join("accounted-child/row-origins.sqlite3");
        let connection = rusqlite::Connection::open(&database).unwrap();
        if fault == "placement" {
            connection
                .execute(
                    "UPDATE origins SET origin=?1 WHERE sequence='1'",
                    [String::from(mez_agent::ContextBlockOccurrenceId::allocate())],
                )
                .unwrap();
        } else if fault == "schema" {
            connection.execute("DROP TABLE row_costs", []).unwrap();
        } else {
            let record = batch.initial_row_accounting().unwrap().records()[&1];
            let mut encoded = serde_json::to_value(record).unwrap();
            if fault == "cache-identity" {
                encoded["occurrence_id"] =
                    String::from(mez_agent::ContextBlockOccurrenceId::allocate()).into();
            } else {
                encoded["format_version"] = 99.into();
            }
            connection
                .execute(
                    "UPDATE row_costs SET record=?1 WHERE origin=?2",
                    rusqlite::params![encoded.to_string(), String::from(record.occurrence_id()),],
                )
                .unwrap();
        }
        drop(connection);
        store
            .accept_append_receipt(&[entry("000-earlier", 1, TranscriptRole::Assistant)], 12)
            .unwrap();
        let directory = root.join(".append-receipts");
        let receipts = || {
            fs::read_dir(&directory)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (
                        path.file_name().unwrap().to_os_string(),
                        fs::read(path).unwrap(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let before = receipts();
        let original = fs::read(&database).unwrap();
        let raw = fs::read(store.transcript_path("accounted-child").unwrap()).unwrap();
        assert!(store.recover_append_receipts().is_err(), "{fault}");
        assert!(
            !store.transcript_path("000-earlier").unwrap().exists(),
            "{fault}: unrelated archive changed"
        );
        assert_eq!(receipts(), before, "{fault}: unrelated receipt changed");
        assert_eq!(fs::read(&database).unwrap(), original);
        assert_eq!(
            fs::read(store.transcript_path("accounted-child").unwrap()).unwrap(),
            raw
        );
        fs::remove_dir_all(root).unwrap();
    }
}
