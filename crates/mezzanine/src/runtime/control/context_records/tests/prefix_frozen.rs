//! A real bounded legacy read becomes an immutable NoStore copy source.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork, RuntimeAgentPromptHistoryWork,
};

fn work(store: AgentTranscriptStore, conversation: &str) -> RuntimeAgentPromptHistoryWork {
    RuntimeAgentPromptHistoryWork {
        frozen_replay: None,
        memory_blocks: Vec::new(),
        transcript_work: Some(RuntimeAgentHistoryEpochWork {
            store,
            pending_capture_error: None,
            inputs: RuntimeAgentHistoryEpochInputs {
                pane_id: "%1".into(),
                conversation_id: conversation.into(),
                ephemeral_source_entries: None,
                active_entries: None,
                pending_entries: Default::default(),
            },
        }),
    }
}

/// Later parent edits and a newer summary cannot change the frozen selection.
/// A child's real initial receipt grants durability without publishing a parent
/// epoch or inferring original occurrences from a later archive row count.
#[test]
fn legacy_prefix_frozen_source_saves_exact_window_without_saving_parent() {
    const CID: &str = "legacy-frozen-parent";
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let rows = (1..=4)
        .map(|sequence| TranscriptEntry {
            conversation_id: CID.into(),
            sequence,
            created_at_unix_seconds: sequence,
            role: if sequence % 2 == 0 {
                TranscriptRole::Assistant
            } else {
                TranscriptRole::User
            },
            turn_id: format!("original-{sequence}"),
            agent_id: "original-agent".into(),
            pane_id: "%1".into(),
            content: format!("original frozen row {sequence}"),
        })
        .collect::<Vec<_>>();
    store.append_many(&rows).unwrap();
    store
        .save_compaction_epoch(CID, 1, "original frozen head")
        .unwrap();
    let path = store.root().join(CID).join("compaction-epoch.json");
    let before = std::fs::read(&path).unwrap();
    let (history, frozen) = work(store.clone(), CID).execute_loop_history(3).unwrap();
    let owner = history
        .replay_owner()
        .unwrap()
        .expect("typed legacy frozen owner");
    let frozen = frozen.expect("immutable legacy capsule");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(store.inspect(CID).unwrap(), rows);
    let expected = owner.context().chronology().to_vec();
    assert_eq!(expected.len(), 3);
    assert!(
        !expected
            .iter()
            .any(|event| event.block().content.contains("row 1")
                || event.block().content.contains("row 4"))
    );
    store
        .compare_and_swap_entry_content(
            CID,
            2,
            "%1",
            &store.transcript_entry_revision(&rows[1]).unwrap(),
            "later edited parent answer".into(),
        )
        .unwrap();
    store
        .save_compaction_epoch(CID, 2, "later parent head")
        .unwrap();
    let parent = std::fs::read(&path).unwrap();
    let parent_rows = store.inspect(CID).unwrap();
    let source = frozen.fork_source().unwrap();
    let batch = source.initial_batch("frozen-saved-child", 456).unwrap();
    store.accept_transcript_append_receipt(&batch, 91).unwrap();
    store.recover_append_receipts().unwrap();
    let (_, child) = AgentTranscriptStore::new(&directory.0)
        .load_replay_epoch("frozen-saved-child", AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(child.context().chronology(), expected);
    assert_eq!(std::fs::read(path).unwrap(), parent);
    assert_eq!(store.inspect(CID).unwrap(), parent_rows);
}

/// Covered preludes (including retired catalogs) back their original task
/// admissions without becoming child model members or replaying task effects.
#[test]
fn legacy_prefix_frozen_tasks_save_original_dependencies_and_mixed_framing() {
    use super::prefix_tasks::{CID, Source, fixture_with_source};
    for (through, source) in [
        (2, Source::Tasks),
        (2, Source::RetiredCatalog),
        (1, Source::MixedLegacy { collision: false }),
        (1, Source::MixedLegacy { collision: true }),
    ] {
        let (directory, store, original) = fixture_with_source(through, source);
        let rows = store.inspect(CID).unwrap();
        let path = store.root().join(CID).join("compaction-epoch.json");
        let before = std::fs::read(&path).unwrap();
        let (history, frozen) = work(store.clone(), CID)
            .execute_loop_history(rows.len() as u64)
            .unwrap();
        let owner = history.replay_owner().unwrap().unwrap();
        let expected = owner.context().chronology().to_vec();
        assert!(history.execution_events.is_empty());
        let mut next_task = history.blocks.clone();
        next_task.push(ContextBlock::user_event("user prompt", "next actual task"));
        let admitted = history
            .records
            .import_context_for_new_task(
                next_task,
                &[],
                &history.execution_events,
                Some((CID, history.blocks.len())),
            )
            .unwrap();
        assert!(admitted.has_replay_owner());
        let admitted = mez_agent::ReplayEpochContext::from_owned_context(
            owner.conversation_id().clone(),
            admitted,
        )
        .unwrap();
        assert_eq!(admitted.head(), owner.head());
        if through == 2 {
            let covered = original.context().chronology()[1].record().occurrence_id();
            assert!(
                !expected
                    .iter()
                    .any(|event| event.record().occurrence_id() == covered)
            );
        }
        let frozen = frozen.unwrap().freeze_for_fork().unwrap();
        let source = frozen.fork_source().unwrap();
        let batch = source.initial_batch("task-frozen-child", 789).unwrap();
        store.accept_transcript_append_receipt(&batch, 92).unwrap();
        store.recover_append_receipts().unwrap();
        let (_, child) = AgentTranscriptStore::new(&directory.0)
            .load_replay_epoch("task-frozen-child", AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(child.context().chronology(), expected);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(store.inspect(CID).unwrap(), rows);
    }
}

/// The real queued append supplies pending identity. A replacement at the same
/// slot receives another occurrence; neither capture acknowledges a parent row.
#[test]
fn legacy_prefix_frozen_pending_identity_survives_queue_replacement() {
    use crate::runtime::{RuntimeSideEffect, persistence::RuntimePersistenceComponent};
    const CID: &str = "legacy-frozen-pending";
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let committed = TranscriptEntry {
        conversation_id: CID.into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "committed".into(),
        agent_id: "original-agent".into(),
        pane_id: "%1".into(),
        content: "covered original prompt".into(),
    };
    store.append(&committed).unwrap();
    store
        .save_compaction_epoch(CID, 1, "pending original head")
        .unwrap();
    let path = store.transcript_path(CID).unwrap();
    let pending = TranscriptEntry {
        sequence: 2,
        role: TranscriptRole::Assistant,
        turn_id: "pending-turn".into(),
        content: "equal pending source 雪".into(),
        ..committed.clone()
    };
    let mut persistence = RuntimePersistenceComponent::default();
    let effect = || RuntimeSideEffect::PersistTranscriptEntries {
        path: path.clone(),
        store: store.clone(),
        batch: crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![pending.clone()])
            .unwrap(),
    };
    persistence
        .queue_checked_completion_transcript(effect())
        .unwrap();
    let capture = |persistence: &RuntimePersistenceComponent| {
        let mut work = work(store.clone(), CID);
        work.transcript_work
            .as_mut()
            .unwrap()
            .inputs
            .pending_entries = persistence
            .pending_transcript_context_at_path(&path, CID)
            .unwrap();
        work.execute_loop_history(2).unwrap()
    };
    let (history, frozen) = capture(&persistence);
    let expected = history
        .replay_owner()
        .unwrap()
        .unwrap()
        .context()
        .chronology()
        .to_vec();
    let again = capture(&persistence).0;
    assert_eq!(
        again
            .replay_owner()
            .unwrap()
            .unwrap()
            .context()
            .chronology(),
        expected
    );
    persistence.cancel_queued_transcript_entries_for_conversation(CID);
    persistence
        .queue_checked_completion_transcript(effect())
        .unwrap();
    let replacement = capture(&persistence).0.replay_owner().unwrap().unwrap();
    assert_ne!(
        replacement.context().chronology()[1]
            .record()
            .occurrence_id(),
        expected[1].record().occurrence_id()
    );
    let source = frozen.unwrap().fork_source().unwrap();
    let batch = source.initial_batch("pending-frozen-child", 890).unwrap();
    store.accept_transcript_append_receipt(&batch, 93).unwrap();
    store.recover_append_receipts().unwrap();
    let (_, child) = AgentTranscriptStore::new(&directory.0)
        .load_replay_epoch("pending-frozen-child", AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(child.context().chronology(), expected);
    assert_eq!(store.inspect(CID).unwrap(), vec![committed]);
    let db =
        rusqlite::Connection::open(store.root().join(CID).join("row-origins.sqlite3")).unwrap();
    let costs: i64 = db
        .query_row(
            "SELECT count(*) FROM row_costs WHERE origin=?1",
            [String::from(expected[1].record().occurrence_id())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(costs, 0);
}

/// A pinned read uses its original selected frontier and the same memory-only
/// source, rather than publishing a full parent epoch from a partial window.
#[test]
fn legacy_prefix_frozen_pinned_read_keeps_copy_custody_without_parent_migration() {
    use super::prefix_tasks::{CID, Source, fixture_with_source};
    for head_only in [false, true] {
        let (_directory, store, _) = fixture_with_source(2, Source::Tasks);
        let rows = store.inspect(CID).unwrap();
        let frontier = if head_only { 2 } else { rows.len() as u64 - 1 };
        let path = store.root().join(CID).join("compaction-epoch.json");
        let before = std::fs::read(&path).unwrap();
        let mut work = work(store.clone(), CID).transcript_work.unwrap();
        work.inputs.ephemeral_source_entries = Some(frontier);
        let history =
            crate::runtime::control::context::execute_runtime_agent_history_epoch_work(work)
                .unwrap();
        let expected = history
            .replay_owner()
            .unwrap()
            .unwrap()
            .context()
            .chronology()
            .to_vec();
        let frozen = crate::runtime::control::RuntimeFrozenReplayHistory::from_history(
            history.clone(),
            frontier,
        )
        .unwrap()
        .unwrap();
        frozen.fork_source().unwrap();
        if head_only {
            assert_eq!(expected.len(), 1);
        }
        // The first actual live continuation has no previous frozen binding. Its
        // original history source must carry the capsule across turn adoption.
        let owner = history.replay_owner().unwrap().unwrap();
        let body = history
            .records
            .import_context(history.blocks.clone())
            .unwrap();
        let frozen = crate::runtime::control::RuntimeFrozenReplayHistory::from_live(
            owner.conversation_id().clone(),
            &body,
            history.source,
            frontier,
            None,
            None,
        )
        .unwrap();
        let source = frozen.fork_source().unwrap();
        let batch = source.initial_batch("pinned-frozen-child", 901).unwrap();
        store.accept_transcript_append_receipt(&batch, 94).unwrap();
        store.recover_append_receipts().unwrap();
        let (_, child) = store
            .load_replay_epoch("pinned-frozen-child", AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(child.context().chronology(), expected);
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(store.inspect(CID).unwrap(), rows);
    }
}
