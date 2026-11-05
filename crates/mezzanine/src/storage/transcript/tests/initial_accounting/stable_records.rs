//! Initial stable baselines share exact receipt and immutable-marker ownership.
//!
//! Baselines precede child row one. Later raw snapshots replace them only in
//! views containing that carrier; malformed required state cannot invent IDs.

use super::*;
use crate::storage::transcript::ConversationTranscriptRead;
use mez_agent::{AgentContext, StableContextBlockRecord, StableContextSlotId};

/// Produces bounded actual typed metadata and its independent user occurrence.
fn source(text: &str) -> AgentContext {
    let mut context = AgentContext::import_durable_blocks_with_stable_sources(
        vec![ContextBlock {
            source: ContextSourceKind::System,
            placement: ContextPlacement::StablePrefix,
            label: "configured agent system prompt".into(),
            content: text.into(),
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
        .append_user_event("user", "actual user source")
        .unwrap();
    context
}

/// Adds a version-two baseline without changing the initial proposal namespace.
fn seeded(stable: Vec<StableContextBlockRecord>, empty: bool) -> TranscriptAppendBatch {
    let batch = batch(true, empty);
    let accounting = batch
        .initial_row_accounting()
        .unwrap()
        .clone()
        .with_stable_records(Some(stable))
        .unwrap();
    batch.with_initial_row_accounting(accounting).unwrap()
}

/// Recovery preserves a seed even with no physical tail; later carrier claims
/// supersede it only within their visible window. Through-prefix lookup must
/// retain the older seed when a full read also contains a future snapshot.
#[test]
fn initial_stable_baseline_recovers_and_obeys_later_carrier_frontiers() {
    for empty in [false, true] {
        let root = temp_root("initial-stable-recovery");
        let store = AgentTranscriptStore::new(&root);
        let old = source("original trusted source 雪").stable_block_records();
        let batch = seeded(old.clone(), empty);
        assert_eq!(
            batch
                .initial_replay_snapshot()
                .unwrap()
                .stable_records
                .unwrap()
                .records()
                .unwrap(),
            old
        );
        store.fail_next_metadata_write_after_promotion();
        assert!(store.accept_transcript_append_receipt(&batch, 7).is_err());
        let cold = AgentTranscriptStore::new(&root);
        cold.recover_append_receipts().unwrap();
        assert!(cold.pending_append_receipts().unwrap().is_empty());
        let boundary = batch.entries().len() as u64;
        let snapshot = cold
            .conversation_replay_snapshot("accounted-child", &[], |_| {
                Some((ConversationTranscriptRead::After(boundary), true))
            })
            .unwrap();
        assert_eq!(snapshot.stable_records.unwrap().records().unwrap(), old);
        let later = source("later current trusted source");
        let event = later.chronology().last().unwrap();
        let slots = later.stable_block_records();
        let mut row = entry("accounted-child", boundary + 1, TranscriptRole::System);
        row.content = mez_agent::transcript::TranscriptContextRecord::new(
            mez_agent::TranscriptContextEvent::user_event(
                event.sequence().get(),
                "user",
                "actual user source",
            )
            .unwrap(),
            event.record(),
            Some(slots.clone()),
        )
        .unwrap()
        .to_transcript_content()
        .unwrap();
        cold.commit_accounted_transcript_batch(
            &TranscriptAppendBatch::owned_rows(vec![row]).unwrap(),
            8,
        )
        .unwrap();
        let snapshot = cold
            .conversation_replay_snapshot("accounted-child", &[], |_| {
                Some((ConversationTranscriptRead::All, true))
            })
            .unwrap();
        assert_eq!(
            snapshot.stable_records.as_ref().unwrap().records().unwrap(),
            slots
        );
        assert_eq!(
            snapshot.stable_records_through(boundary).unwrap().unwrap(),
            old
        );
        assert_eq!(
            snapshot
                .stable_records_through(boundary + 1)
                .unwrap()
                .unwrap(),
            slots
        );
        let empty_source = source("unused current slot");
        let empty_event = empty_source.chronology().last().unwrap();
        let mut empty_row = entry("accounted-child", boundary + 2, TranscriptRole::System);
        empty_row.content = mez_agent::transcript::TranscriptContextRecord::new(
            mez_agent::TranscriptContextEvent::user_event(
                empty_event.sequence().get(),
                "user",
                "actual user source",
            )
            .unwrap(),
            empty_event.record(),
            Some(vec![]),
        )
        .unwrap()
        .to_transcript_content()
        .unwrap();
        cold.commit_accounted_transcript_batch(
            &TranscriptAppendBatch::owned_rows(vec![empty_row]).unwrap(),
            9,
        )
        .unwrap();
        let snapshot = cold
            .conversation_replay_snapshot("accounted-child", &[], |_| {
                Some((ConversationTranscriptRead::After(boundary + 2), true))
            })
            .unwrap();
        assert!(
            snapshot
                .stable_records
                .as_ref()
                .unwrap()
                .records()
                .unwrap()
                .is_empty()
        );
        let mut bad = entry("accounted-child", boundary + 3, TranscriptRole::System);
        bad.content = "[mez-transcript-context-record/v99]\nprivate invalid source".into();
        cold.append(&bad).unwrap();
        let snapshot = cold
            .conversation_replay_snapshot("accounted-child", &[], |_| {
                Some((ConversationTranscriptRead::After(boundary + 3), true))
            })
            .unwrap();
        assert!(
            snapshot.stable_records.unwrap().records().is_err(),
            "bad current metadata borrowed the initial seed"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// The existing v5 whole-proposal digest must bind every v2 stable field.
/// Even valid replacement metadata or a downgrade to a genuine v1 shape cannot
/// alter the original journal; global recovery leaves unrelated rows untouched.
#[test]
fn initial_stable_baseline_receipt_tampering_refuses_all_partial_replay() {
    for alteration in ["replace", "empty", "downgrade"] {
        let root = temp_root("initial-stable-receipt");
        let store = AgentTranscriptStore::new(&root);
        let batch = seeded(source("original source").stable_block_records(), false);
        store.fail_transcript_append_before_summary();
        assert!(store.accept_transcript_append_receipt(&batch, 11).is_err());
        let path = fs::read_dir(root.join(".append-receipts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(receipt["version"], 5);
        match alteration {
            "replace" => {
                receipt["initial_row_accounting"]["stable"] =
                    serde_json::to_value(source("changed source").stable_block_records()).unwrap()
            }
            "empty" => receipt["initial_row_accounting"]["stable"] = serde_json::json!([]),
            _ => {
                receipt["initial_row_accounting"]["version"] = serde_json::json!(1);
                receipt["initial_row_accounting"]
                    .as_object_mut()
                    .unwrap()
                    .remove("stable");
            }
        }
        fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        store
            .accept_append_receipt(&[entry("aaa-unrelated", 1, TranscriptRole::Assistant)], 12)
            .unwrap();
        let raw = fs::read(store.transcript_path("accounted-child").unwrap()).unwrap();
        let journal = fs::read(&path).unwrap();
        assert!(store.recover_append_receipts().is_err(), "{alteration}");
        assert_eq!(
            fs::read(store.transcript_path("accounted-child").unwrap()).unwrap(),
            raw
        );
        assert_eq!(fs::read(&path).unwrap(), journal);
        assert!(!store.transcript_path("aaa-unrelated").unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }
}

/// Genuine version-one payloads keep their exact serialized fields. Version
/// two requires a well-formed nonaliased snapshot and its published immutable
/// marker; deletion or corruption refuses reads without changing source bytes.
#[test]
fn initial_stable_baseline_validates_versions_and_required_marker() {
    let old = batch(false, false);
    let value = serde_json::to_value(old.initial_row_accounting().unwrap()).unwrap();
    assert_eq!(value["version"], 1);
    assert!(value.get("stable").is_none());
    let restored: InitialTranscriptRowAccounting = serde_json::from_value(value).unwrap();
    assert_eq!(&restored, old.initial_row_accounting().unwrap());
    let slots = source("trusted source").stable_block_records();
    let mut alias = serde_json::to_value(&slots[0]).unwrap();
    alias["slot_id"] = serde_json::json!("another/producer");
    for invalid in [
        vec![slots[0].clone(), slots[0].clone()],
        vec![slots[0].clone(), serde_json::from_value(alias).unwrap()],
    ] {
        assert!(
            old.initial_row_accounting()
                .unwrap()
                .clone()
                .with_stable_records(Some(invalid))
                .is_err()
        );
    }
    for fault in ["delete", "version", "missing-snapshot"] {
        let root = temp_root("initial-stable-required");
        let store = AgentTranscriptStore::new(&root);
        let batch = seeded(slots.clone(), false);
        store.accept_transcript_append_receipt(&batch, 9).unwrap();
        store.settle_transcript_append_receipt(&batch, 9).unwrap();
        let connection =
            Connection::open(root.join("accounted-child/row-origins.sqlite3")).unwrap();
        if fault == "delete" {
            connection
                .execute("DELETE FROM initial_inheritance", [])
                .unwrap();
        } else {
            let mut value = serde_json::to_value(batch.initial_row_accounting().unwrap()).unwrap();
            if fault == "version" {
                value["version"] = serde_json::json!(99);
            } else {
                value.as_object_mut().unwrap().remove("stable");
            }
            connection
                .execute(
                    "UPDATE initial_inheritance SET payload=?1",
                    [value.to_string()],
                )
                .unwrap();
        }
        drop(connection);
        let raw = fs::read(store.transcript_path("accounted-child").unwrap()).unwrap();
        assert!(
            store
                .conversation_replay_snapshot("accounted-child", &[], |_| Some((
                    ConversationTranscriptRead::All,
                    true
                )))
                .is_err(),
            "{fault}"
        );
        assert_eq!(
            fs::read(store.transcript_path("accounted-child").unwrap()).unwrap(),
            raw
        );
        store
            .append(&entry("independent-healthy", 1, TranscriptRole::User))
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
