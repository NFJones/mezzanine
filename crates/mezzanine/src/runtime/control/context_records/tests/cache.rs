//! Actual worker replay must import, persist and reuse legacy local costs.
//!
//! The store stays the original source owner. These tests inspect its private
//! descriptive projection separately from semantic rows and canonical outputs.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};

/// Builds the real leased history worker against identical independent rows.
fn fixture() -> (
    Directory,
    AgentTranscriptStore,
    RuntimeAgentHistoryEpochWork,
) {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    for sequence in 1..=3 {
        store
            .append(&TranscriptEntry {
                conversation_id: "legacy-costs".into(),
                sequence,
                created_at_unix_seconds: 1,
                role: TranscriptRole::Assistant,
                turn_id: "original-turn".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "equal 雪 code {\"rows\":[1,2,3]}".into(),
            })
            .unwrap();
    }
    let work = RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store: store.clone(),
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "%1".into(),
            conversation_id: "legacy-costs".into(),
            ephemeral_source_entries: None,
            active_entries: Some(3),
            pending_entries: Vec::new(),
        },
    };
    (directory, store, work)
}

/// The real history import must persist each emitted original estimate beside
/// its origin before returning it. Cold replay must retain those exact records;
/// raw transcript source must never be rewritten to carry the cached costs.
#[test]
fn legacy_history_worker_persists_estimates_for_cold_replay() {
    let (_directory, store, work) = fixture();
    let path = store.transcript_path("legacy-costs").unwrap();
    let raw = std::fs::read(&path).unwrap();
    let first = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(first.records.estimate_recomputations(), 3);
    let original = first.records.indices_with_offset(0).unwrap();
    assert_eq!(original.len(), 3);
    let database =
        rusqlite::Connection::open(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    for (_, record) in &original {
        let encoded: String = database
            .query_row(
                "SELECT record FROM row_costs WHERE origin=?1",
                [String::from(record.occurrence_id())],
                |row| row.get(0),
            )
            .unwrap();
        let saved: ContextBlockRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(saved, *record);
    }
    drop(database);
    let before = std::fs::read(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    let reopened = AgentTranscriptStore::new(store.root());
    let second = execute_runtime_agent_history_epoch_work(RuntimeAgentHistoryEpochWork {
        store: reopened,
        ..work
    })
    .unwrap();
    assert_eq!(second.records.indices_with_offset(0).unwrap(), original);
    assert_eq!(second.records.estimate_recomputations(), 0);
    assert_eq!(
        std::fs::read(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap(),
        before
    );
    assert_eq!(std::fs::read(&path).unwrap(), raw);
}

/// A semantic edit invalidates only its original rendered record. Repeated cold
/// reads reuse the repaired cache, and identical untouched occurrences retain
/// both their own IDs and exact costs rather than sharing a text-keyed cache.
#[test]
fn legacy_cost_revision_recounts_only_edited_occurrence_once() {
    let (_directory, store, work) = fixture();
    let original = execute_runtime_agent_history_epoch_work(work.clone())
        .unwrap()
        .records
        .indices_with_offset(0)
        .unwrap();
    let row = store.inspect("legacy-costs").unwrap()[0].clone();
    store
        .compare_and_swap_entry_content(
            "legacy-costs",
            1,
            "%1",
            &store.transcript_entry_revision(&row).unwrap(),
            "changed 雪 dense code {\"more\":[1,2,3,4,5,6,7,8,9]}".into(),
        )
        .unwrap();
    let changed = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    let records = changed.records.indices_with_offset(0).unwrap();
    assert_eq!(changed.records.estimate_recomputations(), 1);
    assert_eq!(records[0].1.occurrence_id(), original[0].1.occurrence_id());
    assert_ne!(records[0], original[0]);
    assert_eq!(&records[1..], &original[1..]);
    let repeated = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(repeated.records.estimate_recomputations(), 0);
    assert_eq!(repeated.records.indices_with_offset(0).unwrap(), records);
}

/// Tampered method/count/revision claims are descriptive input. Actual source
/// repairs just that cache without adopting the supplied size or changing ID.
#[test]
fn stale_legacy_cost_claim_is_repaired_against_actual_source() {
    let (_directory, store, work) = fixture();
    let original = execute_runtime_agent_history_epoch_work(work.clone())
        .unwrap()
        .records
        .indices_with_offset(0)
        .unwrap();
    let record = original[1].1;
    let mut stale = serde_json::to_value(record).unwrap();
    stale["counting_method"] = 99.into();
    stale["framed_input_tokens"] = 0.into();
    stale["native_input_tokens"] = 0.into();
    let connection =
        rusqlite::Connection::open(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE row_costs SET record=?1 WHERE origin=?2",
            rusqlite::params![stale.to_string(), String::from(record.occurrence_id())],
        )
        .unwrap();
    drop(connection);
    let repaired = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(repaired.records.estimate_recomputations(), 1);
    assert_eq!(repaired.records.indices_with_offset(0).unwrap(), original);
    let repeated = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(repeated.records.estimate_recomputations(), 0);
}

/// A pending slot is not committed membership. Cache only the durable rows,
/// then establish the pending occurrence's cost once its original row settles.
#[test]
fn legacy_pending_cost_is_not_persisted_before_original_row_commit() {
    let (_directory, store, mut work) = fixture();
    let mut pending = store.inspect("legacy-costs").unwrap()[0].clone();
    pending.sequence = 4;
    pending.content = "late accepted legacy source".into();
    work.inputs.pending_entries.push(pending.clone());
    work.inputs.active_entries = Some(4);
    let first = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(first.records.estimate_recomputations(), 4);
    let connection =
        rusqlite::Connection::open(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM row_costs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 3);
    drop(connection);
    store.append(&pending).unwrap();
    let settled = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(settled.records.estimate_recomputations(), 1);
    assert_eq!(
        settled.records.indices_with_offset(0).unwrap(),
        first.records.indices_with_offset(0).unwrap()
    );
    let repeated = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(repeated.records.estimate_recomputations(), 0);
}

/// Saved copies and survivor resequencing preserve caches by original ID. Only
/// a genuinely new append needs an estimate after the namespace is rotated.
#[test]
fn saved_fork_and_resequence_reuse_original_legacy_costs() {
    let (_directory, store, mut work) = fixture();
    let original = execute_runtime_agent_history_epoch_work(work.clone())
        .unwrap()
        .records
        .indices_with_offset(0)
        .unwrap();
    store.fork("legacy-costs", "child-costs", 2).unwrap();
    work.inputs.conversation_id = "child-costs".into();
    let child = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(child.records.estimate_recomputations(), 0);
    assert_eq!(child.records.indices_with_offset(0).unwrap(), original);
    store.delete_entry("child-costs", 1).unwrap();
    work.inputs.active_entries = Some(2);
    let retained = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(retained.records.estimate_recomputations(), 0);
    let expected = original[1..]
        .iter()
        .enumerate()
        .map(|(index, (_, record))| (index, *record))
        .collect::<Vec<_>>();
    assert_eq!(retained.records.indices_with_offset(0).unwrap(), expected);
    let mut appended = store.inspect("child-costs").unwrap()[0].clone();
    appended.sequence = 3;
    store.append(&appended).unwrap();
    work.inputs.active_entries = Some(3);
    let after = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(after.records.estimate_recomputations(), 1);
    assert_eq!(
        &after.records.indices_with_offset(0).unwrap()[..2],
        &expected
    );
}

/// Version-one placement migrates cost schema under the same actual history
/// lease, preserving origin and raw bytes. A failed projection publishes no
/// costs; a later successful original worker can recover useful operation.
#[test]
fn legacy_cost_schema_import_and_failed_projection_preserve_source() {
    let (_directory, store, work) = fixture();
    let raw = std::fs::read(store.transcript_path("legacy-costs").unwrap()).unwrap();
    let connection =
        rusqlite::Connection::open(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    connection
        .execute_batch("DROP TABLE row_costs; PRAGMA user_version=1;")
        .unwrap();
    drop(connection);
    let old_database =
        std::fs::read(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    store
        .save_user_objective("legacy-costs", Some("metadata admission alone"))
        .unwrap();
    assert_eq!(
        std::fs::read(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap(),
        old_database
    );
    let failed = store.conversation_replay_projection::<()>(
        "legacy-costs",
        &[],
        |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        },
        |_| Err(MezError::invalid_state("failed pure source projection")),
    );
    assert!(failed.is_err());
    let connection =
        rusqlite::Connection::open(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM row_costs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(count, 0);
    drop(connection);
    let recovered = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(recovered.records.estimate_recomputations(), 3);
    let repeated = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(repeated.records.estimate_recomputations(), 0);
    assert_eq!(
        std::fs::read(store.transcript_path("legacy-costs").unwrap()).unwrap(),
        raw
    );
}

/// A malformed/future cache or mismatched occurrence cannot supply size claims
/// to a request. Refusal keeps original source/cache evidence intact, while a
/// separate conversation continues through the same real worker successfully.
#[test]
fn invalid_legacy_cache_refuses_only_its_original_conversation() {
    for fault in ["identity", "future", "malformed"] {
        let (_directory, store, work) = fixture();
        let first = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
        let original = first.records.indices_with_offset(0).unwrap()[0].1;
        let mut value = serde_json::to_value(original).unwrap();
        if fault == "identity" {
            value["occurrence_id"] = String::from(ContextBlockOccurrenceId::allocate()).into();
        } else {
            value["format_version"] = 99.into();
        }
        let encoded = if fault == "malformed" {
            "{".into()
        } else {
            value.to_string()
        };
        let database = store.root().join("legacy-costs/row-origins.sqlite3");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE row_costs SET record=?1 WHERE origin=?2",
                rusqlite::params![encoded, String::from(original.occurrence_id()),],
            )
            .unwrap();
        drop(connection);
        let before = std::fs::read(&database).unwrap();
        let raw = std::fs::read(store.transcript_path("legacy-costs").unwrap()).unwrap();
        assert!(
            execute_runtime_agent_history_epoch_work(work.clone()).is_err(),
            "{fault}"
        );
        assert_eq!(std::fs::read(&database).unwrap(), before, "{fault}");
        assert_eq!(
            std::fs::read(store.transcript_path("legacy-costs").unwrap()).unwrap(),
            raw,
            "{fault}"
        );
        let mut row = store.inspect("legacy-costs").unwrap()[0].clone();
        row.conversation_id = "unaffected-costs".into();
        store.append(&row).unwrap();
        let mut unaffected = work;
        unaffected.inputs.conversation_id = "unaffected-costs".into();
        unaffected.inputs.active_entries = Some(1);
        assert_eq!(
            execute_runtime_agent_history_epoch_work(unaffected)
                .unwrap()
                .records
                .estimate_recomputations(),
            1
        );
    }
}

/// A renderer cannot attach another occurrence's otherwise valid record to an
/// original row. Detect mismatched ownership before publishing any cost update.
#[test]
fn rendered_legacy_cache_rejects_foreign_original_occurrence() {
    let (_directory, store, _) = fixture();
    let record = ContextBlockRecord::allocate(&ContextBlock {
        source: ContextSourceKind::TranscriptAssistant,
        placement: ContextPlacement::ConversationAppend,
        label: "foreign".into(),
        content: "foreign evidence".into(),
    })
    .unwrap();
    let result = store.conversation_replay_projection(
        "legacy-costs",
        &[],
        |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        },
        |_| Ok(((), vec![(1, record)])),
    );
    assert!(result.is_err());
    let connection =
        rusqlite::Connection::open(store.root().join("legacy-costs/row-origins.sqlite3")).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM row_costs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
