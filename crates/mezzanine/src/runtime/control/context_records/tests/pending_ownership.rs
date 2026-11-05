//! Pending legacy identity follows the actual append owner across its lifetime.
//!
//! Queue cancellation is a real ownership transition even when a replacement
//! append has equal visible text and reuses the same original conversation slot.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};
use crate::runtime::{RuntimeSideEffect, persistence::RuntimePersistenceComponent};

/// Uses the actual admitted effect and original-path snapshot, not a fabricated
/// cache key. Repeated reads and exact live retries retain the occurrence; a
/// cancelled queued owner cannot lend its identity to a new append at that slot.
#[test]
fn cancelled_pending_legacy_owner_cannot_lend_identity_to_a_reused_row_slot() {
    for (replacement_text, replacement_turn) in [
        ("equal pending source 雪", "first-owner"),
        ("equal pending source 雪", "second-owner"),
        ("different pending source", "second-owner"),
    ] {
        let directory = Directory::new();
        let store = AgentTranscriptStore::new(&directory.0);
        let committed = TranscriptEntry {
            conversation_id: "pending-owner".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::Assistant,
            turn_id: "committed-turn".into(),
            agent_id: "original-agent".into(),
            pane_id: "original-pane".into(),
            content: "committed source".into(),
        };
        store.append(&committed).unwrap();
        let path = store.transcript_path("pending-owner").unwrap();
        let mut owner = RuntimePersistenceComponent::default();
        let pending = TranscriptEntry {
            sequence: 2,
            turn_id: "first-owner".into(),
            content: "equal pending source 雪".into(),
            ..committed.clone()
        };
        let effect = |row: TranscriptEntry| RuntimeSideEffect::PersistTranscriptEntries {
            path: path.clone(),
            store: store.clone(),
            batch: crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![row])
                .unwrap(),
        };
        let first_effect = effect(pending.clone());
        assert!(
            owner
                .queue_checked_completion_transcript(first_effect.clone())
                .unwrap()
        );
        let capture = |owner: &RuntimePersistenceComponent| RuntimeAgentHistoryEpochWork {
            pending_capture_error: None,
            store: store.clone(),
            inputs: RuntimeAgentHistoryEpochInputs {
                pane_id: "original-pane".into(),
                conversation_id: "pending-owner".into(),
                ephemeral_source_entries: None,
                active_entries: Some(100),
                pending_entries: owner
                    .pending_transcript_context_at_path(&path, "pending-owner")
                    .unwrap(),
            },
        };
        let old_work = capture(&owner);
        let first = execute_runtime_agent_history_epoch_work(old_work.clone()).unwrap();
        let frozen = first.records.indices_with_offset(0).unwrap();
        assert_eq!(frozen.len(), 2);
        assert!(
            !owner
                .queue_checked_completion_transcript(first_effect)
                .unwrap()
        );
        let retry = execute_runtime_agent_history_epoch_work(capture(&owner)).unwrap();
        assert_eq!(retry.records.indices_with_offset(0).unwrap(), frozen);
        assert!(
            owner
                .queue_checked_completion_transcript(effect(pending.clone()))
                .is_err()
        );
        let needed = std::collections::BTreeSet::from([frozen[1].1.occurrence_id()]);
        assert!(
            first
                .source
                .as_ref()
                .unwrap()
                .selected_images(&needed)
                .unwrap()
                .contains_key(&frozen[1].1.occurrence_id())
        );
        owner.cancel_queued_transcript_entries_for_conversation("pending-owner");
        assert!(
            owner
                .pending_transcript_entries_at_path(&path, "pending-owner")
                .unwrap()
                .is_empty()
        );
        let replacement = TranscriptEntry {
            sequence: 2,
            turn_id: replacement_turn.into(),
            content: replacement_text.into(),
            ..committed.clone()
        };
        let replacement_effect = effect(replacement.clone());
        let RuntimeSideEffect::PersistTranscriptEntries {
            batch: replacement_batch,
            ..
        } = &replacement_effect
        else {
            unreachable!()
        };
        let replacement_batch = replacement_batch.clone();
        assert!(
            owner
                .queue_checked_completion_transcript(replacement_effect)
                .unwrap()
        );
        let next = execute_runtime_agent_history_epoch_work(capture(&owner)).unwrap();
        let records = next.records.indices_with_offset(0).unwrap();
        assert_eq!(records[0], frozen[0]);
        assert_ne!(
            records[1].1.occurrence_id(),
            frozen[1].1.occurrence_id(),
            "a new pending producer must not inherit a cancelled owner's origin"
        );
        assert_eq!(first.records.indices_with_offset(0).unwrap(), frozen);
        assert!(
            first
                .source
                .as_ref()
                .unwrap()
                .selected_images(&needed)
                .unwrap()
                .contains_key(&frozen[1].1.occurrence_id())
        );
        let db = rusqlite::Connection::open(store.root().join("pending-owner/row-origins.sqlite3"))
            .unwrap();
        let costs: i64 = db
            .query_row(
                "SELECT count(*) FROM row_costs WHERE origin IN (?1, ?2)",
                rusqlite::params![
                    String::from(frozen[1].1.occurrence_id()),
                    String::from(records[1].1.occurrence_id())
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(costs, 0);
        drop(db);
        assert_eq!(
            store.inspect("pending-owner").unwrap(),
            vec![committed.clone()]
        );
        let offered = owner.take_transcript_effects();
        assert_eq!(offered.len(), 1);
        owner.cancel_queued_transcript_entries_for_conversation("pending-owner");
        let inflight = execute_runtime_agent_history_epoch_work(capture(&owner)).unwrap();
        assert_eq!(inflight.records.indices_with_offset(0).unwrap(), records);
        store
            .commit_accounted_transcript_batch(&replacement_batch, 9)
            .unwrap();
        assert!(owner.settle_transcript_write_at_path(
            "pending-owner",
            2,
            &path,
            &replacement_batch
        ));
        assert!(execute_runtime_agent_history_epoch_work(old_work).is_err());
        let cold = execute_runtime_agent_history_epoch_work(RuntimeAgentHistoryEpochWork {
            store: AgentTranscriptStore::new(store.root()),
            ..capture(&owner)
        })
        .unwrap();
        assert_eq!(cold.records.indices_with_offset(0).unwrap(), records);
        assert_eq!(
            store.inspect("pending-owner").unwrap(),
            vec![committed, replacement]
        );
        assert!(
            first
                .source
                .as_ref()
                .unwrap()
                .selected_images(&needed)
                .unwrap()
                .contains_key(&frozen[1].1.occurrence_id())
        );
    }
}
