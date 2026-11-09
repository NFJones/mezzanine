//! Checked raw source owns its original tasks before any summary is published.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};
use mez_agent::{ReplayConversationId, ReplayEpochContext};

const CID: &str = "raw-task-history";

mod reservations;

/// Real canonical admissions create both task boundaries around one pending
/// assistant group. Encoding only projects those original occurrence records.
fn raw_tasks() -> (
    ReplayEpochContext,
    crate::storage::transcript::TranscriptAppendBatch,
) {
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new(CID).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    owner
        .admit_instruction_task(
            vec![ContextBlock::user_event("user prompt", "original task")],
            &[],
            &[],
            0,
        )
        .unwrap();
    let group = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
    owner
        .append_execution_events([mez_agent::ContextConversationAppend::assistant(
            "original assistant",
            "pending source",
            group.clone(),
        )])
        .unwrap();
    owner
        .admit_instruction_task(
            vec![
                ContextBlock::user_event("user prompt", "later task"),
                ContextBlock::user_event("user steering", "retain this exact follow-up"),
            ],
            &[],
            &[],
            0,
        )
        .unwrap();
    let rows = owner
        .context()
        .chronology()
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let block = event.block();
            let semantic =
                if let Some(start) = owner.task_admission_start(event.record().occurrence_id()) {
                    TranscriptContextEvent::task_admission(
                        event.sequence().get(),
                        start,
                        &block.label,
                        &block.content,
                    )
                    .unwrap()
                } else if block.source == ContextSourceKind::UserInstruction {
                    TranscriptContextEvent::user_event(
                        event.sequence().get(),
                        &block.label,
                        &block.content,
                    )
                    .unwrap()
                } else {
                    TranscriptContextEvent::execution_block_with_metadata(
                        block.source,
                        &block.label,
                        &block.content,
                        group.clone(),
                        1,
                        None,
                    )
                    .unwrap()
                };
            TranscriptEntry {
                conversation_id: CID.into(),
                sequence: index as u64 + 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::System,
                turn_id: if index < 2 {
                    "original-turn"
                } else {
                    "later-turn"
                }
                .into(),
                agent_id: "original-agent".into(),
                pane_id: "original-pane".into(),
                content: TranscriptContextRecord::new(semantic, event.record(), None)
                    .unwrap()
                    .to_transcript_content()
                    .unwrap(),
            }
        })
        .collect();
    (
        owner,
        crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap(),
    )
}

fn work(store: AgentTranscriptStore) -> RuntimeAgentHistoryEpochWork {
    RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store,
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "different-reader-pane".into(),
            conversation_id: CID.into(),
            ephemeral_source_entries: None,
            active_entries: Some(100),
            pending_entries: Default::default(),
        },
    }
}

/// Projects the original producer's actual lifecycle, without assigning a model
/// occurrence or acknowledging an ordinary append. This image can be held while
/// later publications change only the selected model history.
fn execution_control(owner: &ReplayEpochContext) -> TranscriptEntry {
    let state = owner.execution_states().unwrap().pop().unwrap();
    TranscriptEntry {
        conversation_id: CID.into(),
        sequence: 5,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "original-turn".into(),
        agent_id: "original-agent".into(),
        pane_id: "original-pane".into(),
        content: mez_agent::transcript::TranscriptExecutionStateRecord::new(state)
            .unwrap()
            .to_transcript_content()
            .unwrap(),
    }
}

#[test]
fn raw_history_closed_control_preserves_exact_source_in_pending_settled_and_cold_views() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let (mut expected, original) = raw_tasks();
    let group = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
    let frontier = expected.context().chronology()[1].sequence();
    let receipt = expected.capture_execution_owner(&group, frontier).unwrap();
    expected.finish_execution_group(&receipt).unwrap();
    let mut rows = original.entries().to_vec();
    rows.push(execution_control(&expected));
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
    let mut captured = work(store.clone());
    captured.inputs.active_entries = Some(batch.entries().len());
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    for stage in 0..3 {
        if stage == 1 {
            store.commit_accounted_transcript_batch(&batch, 94).unwrap();
        }
        if stage == 2 {
            captured = work(AgentTranscriptStore::new(&directory.0));
        }
        let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
        let body = history.records.import_context(history.blocks).unwrap();
        for (actual, original) in body
            .chronology()
            .iter()
            .zip(expected.context().chronology())
        {
            assert_eq!(actual.block(), original.block());
            assert_eq!(actual.record(), original.record());
            assert_eq!(actual.sequence(), original.sequence());
            assert_eq!(actual.retention(), original.retention());
        }
        assert_eq!(body.chronology().len(), 4, "a control is not model source");
        assert!(!body.execution_group_is_pending(&group), "stage {stage}");
        let owner =
            ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
                .unwrap();
        assert!(owner.head().is_none());
        assert_eq!(owner.revision().get(), 0);
    }
}

/// A mixed legacy projection keeps genuine task boundaries and an original
/// closed control when copied before its first summary. Copying may not reopen
/// that group or borrow the late legacy row's producer for its original source.
#[test]
fn raw_history_mixed_legacy_copy_preserves_original_task_and_control() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let (mut owner, original) = raw_tasks();
    let group = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
    let receipt = owner
        .capture_execution_owner(&group, owner.context().chronology()[1].sequence())
        .unwrap();
    owner.finish_execution_group(&receipt).unwrap();
    let mut rows = original.entries().to_vec();
    rows.push(execution_control(&owner));
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
    store.commit_accounted_transcript_batch(&batch, 96).unwrap();
    let legacy = TranscriptEntry {
        conversation_id: CID.into(),
        sequence: 6,
        created_at_unix_seconds: 2,
        // Matching a real task's producer cannot turn a literal role row into
        // a task member during its independent copied projection.
        role: TranscriptRole::User,
        turn_id: "later-turn".into(),
        agent_id: "original-agent".into(),
        pane_id: "original-pane".into(),
        content: "literal legacy follow-up".into(),
    };
    store.append(&legacy).unwrap();
    let history = execute_runtime_agent_history_epoch_work(work(store.clone())).unwrap();
    let expected = history.replay_owner().unwrap().unwrap();
    assert!(!expected.context().execution_group_is_pending(&group));
    assert_eq!(
        &expected.context().chronology()[..4],
        owner.context().chronology()
    );
    let source = history
        .records
        .replay_memory_source()
        .unwrap()
        .fork_source(&expected)
        .unwrap();
    let copied = source.initial_batch("mixed-raw-child", 100).unwrap();
    store.accept_transcript_append_receipt(&copied, 1).unwrap();
    store.settle_transcript_append_receipt(&copied, 1).unwrap();
    let mut read = work(AgentTranscriptStore::new(&directory.0));
    read.inputs.conversation_id = "mixed-raw-child".into();
    let child = execute_runtime_agent_history_epoch_work(read)
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(
        child.context().chronology(),
        expected.context().chronology()
    );
    assert!(!child.context().execution_group_is_pending(&group));
    assert!(child.head().is_none());
    let audit = store.inspect(CID).unwrap();
    assert_eq!(&audit[..5], batch.entries());
    assert_eq!(audit[5], legacy);
}

/// A legacy routed handoff remains typed inert context through an independent
/// initial receipt and cold child replay. Its original occurrence/cost/producer
/// survive; later parent rows and protocol-looking user text cannot replace it.
#[test]
fn raw_history_routed_copy_preserves_original_projection_on_cold_reopen() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let payload = r#"{"version":1,"result_summary":"original routed evidence"}"#;
    for (index, (role, content)) in [
        (TranscriptRole::User, "original parent prompt".to_owned()),
        (
            TranscriptRole::System,
            TranscriptContextEvent::RoutedHandoff {
                content: payload.into(),
            }
            .to_transcript_content(),
        ),
        (
            TranscriptRole::Assistant,
            "original visible answer".to_owned(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append(&TranscriptEntry {
                conversation_id: CID.into(),
                sequence: index as u64 + 1,
                created_at_unix_seconds: 1,
                role,
                turn_id: "original-routed-turn".into(),
                agent_id: "original-agent".into(),
                pane_id: "original-pane".into(),
                content,
            })
            .unwrap();
    }
    let original_rows = store.inspect(CID).unwrap();
    let history = execute_runtime_agent_history_epoch_work(work(store.clone())).unwrap();
    let expected = history.replay_owner().unwrap().unwrap();
    let source = history
        .records
        .replay_memory_source()
        .unwrap()
        .fork_source(&expected)
        .unwrap();
    // The frozen source, not a later parent count/read, owns this copy.
    store
        .append(&TranscriptEntry {
            sequence: 4,
            role: TranscriptRole::User,
            content: "late parent must not enter frozen copy".into(),
            ..original_rows[0].clone()
        })
        .unwrap();
    let copied = source.initial_batch("routed-raw-child", 100).unwrap();
    let handoff = copied
        .entries()
        .iter()
        .find(|row| {
            TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
                .is_some_and(|record| {
                    matches!(record.event(),
                TranscriptContextEvent::RoutedHandoff {content} if content == payload)
                })
        })
        .unwrap();
    assert_eq!(handoff.turn_id, "original-routed-turn");
    assert_eq!(handoff.agent_id, "original-agent");
    assert_eq!(handoff.pane_id, "original-pane");
    store.accept_transcript_append_receipt(&copied, 1).unwrap();
    store.settle_transcript_append_receipt(&copied, 1).unwrap();
    let mut read = work(AgentTranscriptStore::new(&directory.0));
    read.inputs.conversation_id = "routed-raw-child".into();
    let child = execute_runtime_agent_history_epoch_work(read)
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(
        child.context().chronology(),
        expected.context().chronology()
    );
    assert!(child.head().is_none());
    assert_eq!(child.revision().get(), 0);
    assert_eq!(&store.inspect(CID).unwrap()[..3], original_rows.as_slice());
}

/// Pending legacy rows keep their actual append owner. The same captured read
/// can normalize only after that original receipt physically settles its rows.
#[test]
fn raw_history_legacy_pending_source_waits_for_original_settlement() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let original = TranscriptEntry {
        conversation_id: CID.into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "legacy-original".into(),
        agent_id: "original-agent".into(),
        pane_id: "original-pane".into(),
        content: "original committed legacy text".into(),
    };
    store.append(&original).unwrap();
    let pending = TranscriptEntry {
        sequence: 2,
        content: "actual pending legacy text".into(),
        ..original.clone()
    };
    let batch =
        crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![pending.clone()])
            .unwrap();
    let mut captured = work(store.clone());
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    let first = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
    let source = first.source.as_ref().unwrap().raw.as_ref().unwrap();
    let owner = first.replay_owner().unwrap().unwrap();
    let prepare = || {
        let mez_agent::ReplayCompactionPreparation::Ready(plan) = owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap()
        else {
            panic!("expected original raw source");
        };
        store.prepare_replay_epoch(
            crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                "publisher",
                "publisher-agent",
                "publisher-pane",
            )
            .unwrap()
            .capture_legacy_raw(source),
            CID,
            None,
            &owner,
            plan,
            mez_agent::ReplayHeadPublication::Summary("original source summary".into()),
        )
    };
    assert!(
        prepare().is_err(),
        "a read must not acknowledge a pending append"
    );
    assert_eq!(
        store.inspect(CID).unwrap().as_slice(),
        std::slice::from_ref(&original)
    );
    store.accept_transcript_append_receipt(&batch, 1).unwrap();
    // Execute the original physical/accounting owner; journal admission alone
    // is not an archive write and cannot authorize receipt settlement.
    assert!(
        prepare().is_err(),
        "journal admission is not physical settlement"
    );
    store.commit_accounted_transcript_batch(&batch, 1).unwrap();
    let history = execute_runtime_agent_history_epoch_work(captured).unwrap();
    let settled = history.replay_owner().unwrap().unwrap();
    assert!(settled.head().is_none());
    assert_eq!(settled.context().chronology().len(), 2);
    assert_eq!(settled.context().chronology(), owner.context().chronology());
    // The SAME frozen source can now prepare; no regenerated read or summary
    // is needed to acquire the original rows' real physical proof.
    prepare().unwrap();
    assert_eq!(store.inspect(CID).unwrap(), [original, pending]);
    assert!(store.compaction_epoch(CID).unwrap().is_none());
}

/// Uses actual first-head adoption and the original live retirement handle.
/// The control's ordinary append is delayed unchanged across further heads;
/// neither archived model rows nor newer publisher identity may replace it.
fn late_original_execution_control(head_count: usize, fault: Option<&str>) {
    use crate::storage::transcript::replay_epoch::ReplaySourceOrigin;
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let (mut owner, original) = raw_tasks();
    store
        .commit_accounted_transcript_batch(&original, 95)
        .unwrap();
    let group = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
    let frontier = owner.context().chronology()[1].sequence();
    let receipt = owner.capture_execution_owner(&group, frontier).unwrap();
    let mut snapshot = None;
    let mut held_control = None;
    for head in 1..=head_count {
        if head == 3 {
            owner
                .admit_instruction_task(
                    vec![ContextBlock::user_event("user prompt", "third task")],
                    &[],
                    &[],
                    0,
                )
                .unwrap();
        }
        let plan = match owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("expected head {head}: {other:?}"),
        };
        let origin =
            ReplaySourceOrigin::new("publisher-turn", "publisher-agent", "publisher-pane").unwrap();
        let capture = if head == 1 {
            origin.capture_raw(original.entries())
        } else {
            origin.capture(snapshot.as_ref())
        };
        let proposal = store
            .prepare_replay_epoch(
                capture,
                CID,
                snapshot.as_ref().map(
                    |s: &crate::storage::transcript::replay_epoch::ReplayEpochSnapshot| {
                        s.identity().clone()
                    },
                ),
                &owner,
                plan,
                mez_agent::ReplayHeadPublication::Summary(format!("exact head {head}")),
            )
            .unwrap();
        let ack = store.publish_replay_epoch(&proposal).unwrap();
        snapshot = Some(ack.snapshot(&store, &proposal).unwrap());
        let mut body = owner.into_owned_context();
        ack.adopt(&store, &proposal, &mut body).unwrap();
        owner =
            ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
                .unwrap();
        if head == 1 {
            owner.finish_execution_group(&receipt).unwrap();
            held_control = Some(
                crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![
                    execution_control(&owner),
                ])
                .unwrap(),
            );
        }
    }
    let held_control = held_control.unwrap();
    store
        .commit_accounted_transcript_batch(&held_control, 96)
        .unwrap();
    let archive = directory.0.join(CID).join("history.tsv");
    let manifest = directory.0.join(CID).join("compaction-epoch.json");
    let raw_before = std::fs::read(&archive).unwrap();
    let head_before = std::fs::read(&manifest).unwrap();
    if let Some(fault) = fault {
        let connection =
            rusqlite::Connection::open(directory.0.join(CID).join("row-origins.sqlite3")).unwrap();
        match fault {
            "missing-schema" => connection
                .execute_batch("DROP TABLE execution_sources;")
                .unwrap(),
            "missing-source" => {
                connection
                    .execute("DELETE FROM execution_sources", [])
                    .unwrap();
            }
            "changed-descriptor" => {
                connection.execute("UPDATE execution_sources SET payload=replace(payload, 'original-agent', 'different-agent')", []).unwrap();
            }
            "missing-image" => {
                connection
                    .execute("DELETE FROM row_digests WHERE sequence='2'", [])
                    .unwrap();
            }
            "legacy-v4" => connection
                .execute_batch("DROP TABLE execution_sources; PRAGMA user_version=4;")
                .unwrap(),
            _ => panic!("unknown original custody fault"),
        }
    }
    let replay =
        execute_runtime_agent_history_epoch_work(work(AgentTranscriptStore::new(&directory.0)));
    if fault == Some("legacy-v4") {
        let connection =
            rusqlite::Connection::open(directory.0.join(CID).join("row-origins.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            4
        );
    }
    if fault.is_some() && !(head_count == 1 && fault == Some("legacy-v4")) {
        let error = replay.unwrap_err();
        assert!(error.message().contains("execution "), "{fault:?}: {error}");
        assert_eq!(std::fs::read(archive).unwrap(), raw_before);
        assert_eq!(std::fs::read(manifest).unwrap(), head_before);
        return;
    }
    let replay = replay.unwrap();
    let body = replay.records.import_context(replay.blocks).unwrap();
    assert_eq!(body.chronology(), owner.context().chronology());
    assert!(!body.execution_group_is_pending(&group));
    assert_eq!(
        store.inspect(CID).unwrap().last(),
        held_control.entries().last()
    );
}

#[test]
fn raw_history_late_original_control_after_first_head() {
    late_original_execution_control(1, None);
}

#[test]
fn raw_history_late_original_control_after_second_head() {
    late_original_execution_control(2, None);
}

#[test]
fn raw_history_late_original_control_after_third_head() {
    late_original_execution_control(3, None);
}

/// Source-index absence cannot become guessed proof after escrow retirement;
/// damaged required metadata refuses without altering raw bytes or the head.
#[test]
fn raw_history_late_control_rejects_lost_or_changed_original_indexed_custody() {
    for fault in [
        "missing-schema",
        "missing-source",
        "changed-descriptor",
        "missing-image",
        "legacy-v4",
    ] {
        late_original_execution_control(3, Some(fault));
    }
}

#[test]
fn raw_history_legacy_index_can_use_current_original_escrow_without_backfill() {
    late_original_execution_control(1, Some("legacy-v4"));
}

/// Neither a newly accounted control nor a matching group name can reassign
/// original producer fields or refer to source physically after that control.
#[test]
fn raw_history_execution_control_rejects_foreign_producer_and_future_member() {
    for fault in ["turn", "agent", "pane", "future-member", "missing-member"] {
        let directory = Directory::new();
        let store = AgentTranscriptStore::new(&directory.0);
        let (mut owner, original) = raw_tasks();
        let group = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
        let receipt = owner
            .capture_execution_owner(&group, owner.context().chronology()[1].sequence())
            .unwrap();
        owner.finish_execution_group(&receipt).unwrap();
        let mut control = execution_control(&owner);
        let mut rows = original.entries().to_vec();
        match fault {
            "turn" => control.turn_id = "foreign-turn".into(),
            "agent" => control.agent_id = "foreign-agent".into(),
            "pane" => control.pane_id = "foreign-pane".into(),
            "future-member" => {
                control.sequence = 2;
                rows[1].sequence = 5;
            }
            "missing-member" => {
                rows.remove(1);
            }
            _ => unreachable!(),
        }
        rows.push(control);
        rows.sort_by_key(|row| row.sequence);
        for (index, row) in rows.iter_mut().enumerate() {
            row.sequence = index as u64 + 1;
        }
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
        store.commit_accounted_transcript_batch(&batch, 97).unwrap();
        assert!(
            execute_runtime_agent_history_epoch_work(work(store.clone())).is_err(),
            "{fault}"
        );
        assert_eq!(store.inspect(CID).unwrap(), batch.entries());
    }
}

/// Ordinary CAS cannot amend or introduce descriptive lifecycle authority.
/// The same bytes in a User row are literal content and remain editable.
#[test]
fn raw_history_execution_controls_are_immutable_under_ordinary_content_edits() {
    for mode in ["existing-control", "introduced-control", "user-literal"] {
        let directory = Directory::new();
        let store = AgentTranscriptStore::new(&directory.0);
        let (mut owner, original) = raw_tasks();
        let group = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
        let receipt = owner
            .capture_execution_owner(&group, owner.context().chronology()[1].sequence())
            .unwrap();
        owner.finish_execution_group(&receipt).unwrap();
        let mut row = execution_control(&owner);
        let replacement = if mode == "introduced-control" {
            let original_control = row.content.clone();
            row.content = "plain original audit".into();
            original_control
        } else {
            "ordinary edited text".into()
        };
        if mode == "user-literal" {
            row.role = TranscriptRole::User;
        }
        let mut rows = original.entries().to_vec();
        rows.push(row.clone());
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
        store.commit_accounted_transcript_batch(&batch, 98).unwrap();
        let result = store.compare_and_swap_entry_content(
            CID,
            row.sequence,
            &row.pane_id,
            &store.transcript_entry_revision(&row).unwrap(),
            replacement,
        );
        if mode == "user-literal" {
            assert!(matches!(
                result.unwrap(),
                crate::storage::transcript::CompareAndSwapTranscriptEntryResult::Updated(_)
            ));
        } else {
            assert!(
                result.unwrap_err().message().contains("lifecycle control"),
                "{mode}"
            );
            assert_eq!(store.inspect(CID).unwrap(), batch.entries());
        }
    }
}

/// Pending, settled and cold reads retain exact source and conservative group
/// protection. First publication captures the old producer rather than lending
/// the publisher's turn identity to imported source; cold replay exposes one head.
#[test]
fn raw_history_keeps_tasks_pending_groups_and_first_publication_provenance() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let (expected, batch) = raw_tasks();
    let mut captured = work(store.clone());
    captured.inputs.active_entries = Some(batch.entries().len());
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    for stage in 0..3 {
        if stage == 1 {
            store.commit_accounted_transcript_batch(&batch, 91).unwrap();
        }
        if stage == 2 {
            captured.store = AgentTranscriptStore::new(&directory.0);
        }
        let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
        assert!(history.records.replay_snapshot().is_none());
        let body = history.records.import_context(history.blocks).unwrap();
        assert!(body.has_replay_owner());
        let owner =
            ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
                .unwrap();
        assert!(owner.head().is_none());
        assert_eq!(owner.revision().get(), 0);
        for (actual, original) in owner
            .context()
            .chronology()
            .iter()
            .zip(expected.context().chronology())
        {
            assert_eq!(actual.block(), original.block());
            assert_eq!(actual.record(), original.record());
            assert_eq!(actual.sequence(), original.sequence());
            assert_eq!(actual.retention(), original.retention());
        }
        assert_eq!(owner.context().chronology().len(), 4);
        assert!(owner.context().execution_group_is_pending(
            &mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap()
        ));
        if stage == 2 {
            let plan = match owner
                .prepare_compaction(mez_agent::ReplayTailPolicy::new(1000, 0).unwrap())
                .unwrap()
            {
                mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
                other => panic!("expected historical task prefix: {other:?}"),
            };
            assert_eq!(plan.summary_input().len(), 1);
            let origin = crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                "publisher-turn",
                "publisher-agent",
                "publisher-pane",
            )
            .unwrap();
            assert!(
                store
                    .prepare_replay_epoch(
                        origin.clone().capture(None),
                        CID,
                        None,
                        &owner,
                        plan.clone(),
                        mez_agent::ReplayHeadPublication::Summary(
                            "must retain original producer".into()
                        )
                    )
                    .is_err()
            );
            let originals = history.source.unwrap().raw_replay_rows(&owner).unwrap();
            let proposal = store
                .prepare_replay_epoch(
                    origin.capture_raw(&originals),
                    CID,
                    None,
                    &owner,
                    plan,
                    mez_agent::ReplayHeadPublication::Summary("historical task summarized".into()),
                )
                .unwrap();
            store.publish_replay_epoch(&proposal).unwrap();
            let replay = execute_runtime_agent_history_epoch_work(work(AgentTranscriptStore::new(
                &directory.0,
            )))
            .unwrap();
            let body = replay.records.import_context(replay.blocks).unwrap();
            assert_eq!(
                body.chronology()[0].block().content,
                "historical task summarized"
            );
            assert_eq!(body.chronology().len(), 4);
            assert_eq!(body.chronology()[1..], owner.context().chronology()[1..]);
            assert_eq!(store.inspect(CID).unwrap(), batch.entries());
        }
    }
}

/// An explicit boundary must resolve inside its exact captured projection.
/// Truncation or forged task starts cannot invent a completed earlier task.
#[test]
fn raw_history_rejects_missing_task_boundaries_before_provider_admission() {
    for fault in ["missing", "future", "foreign-producer", "mixed-missing"] {
        let directory = Directory::new();
        let store = AgentTranscriptStore::new(&directory.0);
        let (_, batch) = raw_tasks();
        let mut rows = batch.entries().to_vec();
        let index = if fault == "foreign-producer" { 2 } else { 0 };
        let record = TranscriptContextRecord::from_transcript_content(&rows[index].content)
            .unwrap()
            .unwrap();
        let TranscriptContextEvent::TaskAdmission {
            event_sequence,
            label,
            content,
            ..
        } = record.event()
        else {
            unreachable!()
        };
        let start = if fault == "missing" || fault == "mixed-missing" {
            ContextBlockOccurrenceId::allocate()
        } else if fault == "foreign-producer" {
            TranscriptContextRecord::from_transcript_content(&rows[1].content)
                .unwrap()
                .unwrap()
                .record()
                .occurrence_id()
        } else {
            TranscriptContextRecord::from_transcript_content(&rows[2].content)
                .unwrap()
                .unwrap()
                .record()
                .occurrence_id()
        };
        rows[index].content = TranscriptContextRecord::new(
            TranscriptContextEvent::task_admission(*event_sequence, start, label, content).unwrap(),
            record.record(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap();
        if fault == "mixed-missing" {
            rows.push(TranscriptEntry {
                sequence: rows.len() as u64 + 1,
                role: TranscriptRole::Assistant,
                turn_id: "legacy-unaccounted-turn".into(),
                content: "legacy source needing migration".into(),
                ..rows[0].clone()
            });
        }
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
        store.commit_accounted_transcript_batch(&batch, 92).unwrap();
        assert!(
            execute_runtime_agent_history_epoch_work(work(store.clone())).is_err(),
            "{fault}"
        );
        assert!(
            store
                .load_replay_epoch(CID, AgentContext::empty())
                .unwrap()
                .is_none()
        );
        assert_eq!(store.inspect(CID).unwrap(), batch.entries());
    }
}

/// A first head can back actual pending originals without pretending their
/// ordinary archive append has happened. Its late exact append settles once
/// and never reintroduces the covered task into canonical model history.
#[test]
fn raw_history_first_head_backs_pending_originals_without_append_acknowledgment() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let (_, batch) = raw_tasks();
    let mut captured = work(store.clone());
    captured.inputs.active_entries = Some(batch.entries().len());
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
    let body = history.records.import_context(history.blocks).unwrap();
    let owner =
        ReplayEpochContext::from_owned_context(ReplayConversationId::new(CID).unwrap(), body)
            .unwrap();
    let plan = match owner
        .prepare_compaction(mez_agent::ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    {
        mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected historical task prefix: {other:?}"),
    };
    let originals = history.source.unwrap().raw_replay_rows(&owner).unwrap();
    let proposal = store
        .prepare_replay_epoch(
            crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                "publisher",
                "publisher-agent",
                "publisher-pane",
            )
            .unwrap()
            .capture_raw(&originals),
            CID,
            None,
            &owner,
            plan,
            mez_agent::ReplayHeadPublication::Summary("pending original task summarized".into()),
        )
        .unwrap();
    store.publish_replay_epoch(&proposal).unwrap();
    assert!(store.inspect(CID).unwrap().is_empty());
    assert_eq!(captured.inputs.pending_entries.entries(), batch.entries());
    let replay = execute_runtime_agent_history_epoch_work(captured).unwrap();
    let before = replay.records.import_context(replay.blocks).unwrap();
    assert_eq!(
        before.chronology()[0].block().content,
        "pending original task summarized"
    );
    store.commit_accounted_transcript_batch(&batch, 93).unwrap();
    let replay =
        execute_runtime_agent_history_epoch_work(work(AgentTranscriptStore::new(&directory.0)))
            .unwrap();
    let after = replay.records.import_context(replay.blocks).unwrap();
    assert_eq!(after.chronology(), before.chronology());
    assert_eq!(store.inspect(CID).unwrap(), batch.entries());
}
