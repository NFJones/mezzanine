//! Stale authored estimates are repaired beside their original physical rows.
//!
//! The actual leased history worker must reuse the repair on cold replay without
//! rewriting the source envelope or promoting its descriptive cache to authority.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};

/// Alters only descriptive claims, retaining the actual producer occurrence.
fn stale(record: ContextBlockRecord) -> ContextBlockRecord {
    let mut value = serde_json::to_value(record).unwrap();
    value["counting_method"] = 99.into();
    value["framed_input_tokens"] = 0.into();
    value["native_input_tokens"] = 0.into();
    serde_json::from_value(value).unwrap()
}

/// Persists real canonical user/peer records, then makes one authored cost stale.
fn fixture() -> (
    Directory,
    AgentTranscriptStore,
    RuntimeAgentHistoryEpochWork,
    AgentContext,
) {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let original = persist(&store);
    let row = store.inspect("record-round-trip").unwrap()[1].clone();
    let (event, record, stable) = TranscriptContextRecord::from_transcript_content(&row.content)
        .unwrap()
        .unwrap()
        .into_parts();
    let content = TranscriptContextRecord::new(event, stale(record), stable)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    store
        .compare_and_swap_entry_content(
            "record-round-trip",
            2,
            "original-pane",
            &store.transcript_entry_revision(&row).unwrap(),
            content,
        )
        .unwrap();
    let work = RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store: store.clone(),
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "original-pane".into(),
            conversation_id: "record-round-trip".into(),
            ephemeral_source_entries: None,
            active_entries: Some(100),
            pending_entries: Vec::new(),
        },
    };
    (directory, store, work, original)
}

/// A stale authored method/count is repaired once under the original source
/// lease. A cold store reuses that exact record and performs no durable rewrite;
/// equal peer text remains distinct and original raw envelope bytes stay intact.
#[test]
fn authored_cost_repair_is_persisted_for_unchanged_cold_replay() {
    let (_directory, store, work, original) = fixture();
    let path = store.transcript_path("record-round-trip").unwrap();
    let raw = std::fs::read(&path).unwrap();
    let first = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(first.records.estimate_recomputations(), 1);
    let records = first.records.indices_with_offset(0).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|(_, record)| *record)
            .collect::<Vec<_>>(),
        original
            .chronology()
            .iter()
            .map(|event| event.record())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        first.records.rendered_row_records().unwrap(),
        vec![(2, records[1].1)]
    );
    let database = store.root().join("record-round-trip/row-origins.sqlite3");
    let before = std::fs::read(&database).unwrap();
    let cold = execute_runtime_agent_history_epoch_work(RuntimeAgentHistoryEpochWork {
        store: AgentTranscriptStore::new(store.root()),
        ..work
    })
    .unwrap();
    assert_eq!(cold.records.indices_with_offset(0).unwrap(), records);
    assert_eq!(cold.records.estimate_recomputations(), 0);
    assert_eq!(std::fs::read(&database).unwrap(), before);
    assert_eq!(std::fs::read(&path).unwrap(), raw);
}

/// Valid authored snapshots already own their estimates. A real leased read
/// must neither recompute them nor create redundant durable row-cost entries.
#[test]
fn valid_authored_costs_need_no_duplicate_durable_cache() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    persist(&store);
    let work = RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store: store.clone(),
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "original-pane".into(),
            conversation_id: "record-round-trip".into(),
            ephemeral_source_entries: None,
            active_entries: Some(100),
            pending_entries: Vec::new(),
        },
    };
    let rendered = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(rendered.records.estimate_recomputations(), 0);
    assert!(rendered.records.rendered_row_records().unwrap().is_empty());
    let db = rusqlite::Connection::open(store.root().join("record-round-trip/row-origins.sqlite3"))
        .unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM row_costs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

/// Pending authored source keeps its producer identity but cannot publish a
/// repair before commitment. Once the exact row commits, its cache is repaired
/// and cold replay stops recomputing, without changing either raw envelope.
#[test]
fn pending_authored_repair_waits_for_its_actual_raw_commit() {
    let (_directory, store, mut work, _original) = fixture();
    let mut pending = store.inspect("record-round-trip").unwrap()[2].clone();
    let block = ContextBlock::reference_event(
        ContextSourceKind::PeerMessage,
        "pending peer",
        "pending 雪 source",
    );
    let record = ContextBlockRecord::allocate(&block).unwrap();
    let event = TranscriptContextEvent::prompt_boundary_with_event_sequence(
        block.source,
        100_000,
        block.label.clone(),
        block.content.clone(),
    )
    .unwrap();
    pending.sequence = 4;
    pending.content = TranscriptContextRecord::new(event, stale(record), None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    work.inputs.pending_entries.push(pending.clone());
    let first = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(first.records.estimate_recomputations(), 2);
    assert_eq!(first.records.indices_with_offset(0).unwrap()[3].1, record);
    let database = store.root().join("record-round-trip/row-origins.sqlite3");
    let db = rusqlite::Connection::open(&database).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM row_costs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    drop(db);
    store.append(&pending).unwrap();
    work.inputs.pending_entries.clear();
    let raw = std::fs::read(store.transcript_path("record-round-trip").unwrap()).unwrap();
    let committed = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(committed.records.estimate_recomputations(), 1);
    assert_eq!(
        committed.records.indices_with_offset(0).unwrap()[3].1,
        record
    );
    let cold = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(cold.records.estimate_recomputations(), 0);
    assert_eq!(
        std::fs::read(store.transcript_path("record-round-trip").unwrap()).unwrap(),
        raw
    );
}

/// A valid-looking cost for a different occurrence cannot replace an authored
/// row's owner. Refuse only that conversation without touching evidence or
/// preventing an independent conversation from replaying successfully.
#[test]
fn foreign_authored_cache_is_contained_without_repairing_away_ownership() {
    let (_directory, store, work, original) = fixture();
    execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    let event = &original.chronology()[1];
    let foreign = ContextBlockRecord::allocate(event.block()).unwrap();
    let database = store.root().join("record-round-trip/row-origins.sqlite3");
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute(
        "UPDATE row_costs SET record=?1 WHERE origin=?2",
        rusqlite::params![
            serde_json::to_string(&foreign).unwrap(),
            String::from(event.record().occurrence_id())
        ],
    )
    .unwrap();
    drop(db);
    let bytes = std::fs::read(&database).unwrap();
    let raw = std::fs::read(store.transcript_path("record-round-trip").unwrap()).unwrap();
    assert!(execute_runtime_agent_history_epoch_work(work.clone()).is_err());
    assert_eq!(std::fs::read(&database).unwrap(), bytes);
    assert_eq!(
        std::fs::read(store.transcript_path("record-round-trip").unwrap()).unwrap(),
        raw
    );
    let mut row = store.inspect("record-round-trip").unwrap()[0].clone();
    row.conversation_id = "unaffected-authored-cache".into();
    row.role = TranscriptRole::Assistant;
    row.content = "independent source".into();
    store.append(&row).unwrap();
    let mut independent = work;
    independent.inputs.conversation_id = row.conversation_id;
    assert!(execute_runtime_agent_history_epoch_work(independent).is_ok());
}

/// A later semantic edit invalidates the original row's repaired cache without
/// changing its occurrence or touching an equal, independent peer occurrence.
/// Repeated replay must reuse this second repair rather than the stale envelope.
#[test]
fn authored_cost_cache_revises_only_the_edited_semantic_row() {
    let (_directory, store, work, original) = fixture();
    let first = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    let before = first.records.indices_with_offset(0).unwrap();
    let event = &original.chronology()[1];
    let semantic = TranscriptContextEvent::prompt_boundary_with_event_sequence(
        ContextSourceKind::PeerMessage,
        event.sequence().get(),
        "peer",
        "edited 雪 dense {\"rows\":[1,2,3,4,5,6,7,8,9]}",
    )
    .unwrap();
    let row = store.inspect("record-round-trip").unwrap()[1].clone();
    let content = TranscriptContextRecord::new(semantic, stale(event.record()), None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    store
        .compare_and_swap_entry_content(
            "record-round-trip",
            2,
            "original-pane",
            &store.transcript_entry_revision(&row).unwrap(),
            content,
        )
        .unwrap();
    let raw = std::fs::read(store.transcript_path("record-round-trip").unwrap()).unwrap();
    let changed = execute_runtime_agent_history_epoch_work(work.clone()).unwrap();
    assert_eq!(changed.records.estimate_recomputations(), 1);
    let records = changed.records.indices_with_offset(0).unwrap();
    assert_ne!(records[1].1, before[1].1);
    assert_eq!(records[1].1.occurrence_id(), before[1].1.occurrence_id());
    assert_eq!(records[0], before[0]);
    assert_eq!(records[2], before[2]);
    assert_eq!(
        changed.records.rendered_row_records().unwrap(),
        vec![(2, records[1].1)]
    );
    let cold = execute_runtime_agent_history_epoch_work(work).unwrap();
    assert_eq!(cold.records.indices_with_offset(0).unwrap(), records);
    assert_eq!(cold.records.estimate_recomputations(), 0);
    assert_eq!(
        std::fs::read(store.transcript_path("record-round-trip").unwrap()).unwrap(),
        raw
    );
}
