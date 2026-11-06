//! Original task admissions survive pending source, cold replay and publication.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};
use crate::test_support::replay_epoch::{CONVERSATION, Fixture};
use mez_agent::{
    ReplayCompactionPreparation, ReplayConversationId, ReplayEpochContext, ReplayHeadPublication,
    ReplayTailPolicy,
};

/// The actual canonical admission API creates both task boundaries. Encoding
/// projects their original records; no labels are used to invent retirement.
fn tasks(
    fixture: &Fixture,
) -> (
    ReplayEpochContext,
    crate::storage::transcript::TranscriptAppendBatch,
) {
    let mut owner = fixture.source.clone();
    let mut rows = fixture.delayed_original_rows();
    for task in 1..=2 {
        let start = owner.context().chronology().len();
        owner
            .admit_instruction_task(
                vec![
                    ContextBlock::task_prelude(
                        ContextSourceKind::SkillInstruction,
                        "task skill",
                        format!("exact skill {task}"),
                    ),
                    ContextBlock::user_event("user prompt", format!("task {task}")),
                    ContextBlock::user_event("user steering", format!("steering {task}")),
                ],
                &[],
                &[],
                1,
            )
            .unwrap();
        for event in &owner.context().chronology()[start..] {
            let block = event.block();
            let semantic =
                if let Some(first) = owner.task_admission_start(event.record().occurrence_id()) {
                    TranscriptContextEvent::task_admission(
                        event.sequence().get(),
                        first,
                        &block.label,
                        &block.content,
                    )
                } else if block.source == ContextSourceKind::UserInstruction {
                    TranscriptContextEvent::user_event(
                        event.sequence().get(),
                        &block.label,
                        &block.content,
                    )
                } else {
                    TranscriptContextEvent::prompt_boundary_with_event_sequence(
                        block.source,
                        event.sequence().get(),
                        &block.label,
                        &block.content,
                    )
                }
                .unwrap();
            rows.push(TranscriptEntry {
                conversation_id: CONVERSATION.into(),
                sequence: rows.len() as u64 + 1,
                created_at_unix_seconds: 123,
                role: TranscriptRole::System,
                turn_id: format!("actual-task-{task}"),
                agent_id: "original-task-agent".into(),
                pane_id: "original-task-pane".into(),
                content: TranscriptContextRecord::new(semantic, event.record(), None)
                    .unwrap()
                    .to_transcript_content()
                    .unwrap(),
            });
        }
    }
    (
        owner,
        crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap(),
    )
}

fn work(fixture: &Fixture) -> RuntimeAgentHistoryEpochWork {
    RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store: fixture.store.clone(),
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "different-replay-pane".into(),
            conversation_id: CONVERSATION.into(),
            ephemeral_source_entries: None,
            active_entries: Some(0),
            pending_entries: Default::default(),
        },
    }
}

#[test]
fn recorded_tasks_preserve_multiple_admissions_and_publication_source_witnesses() {
    let fixture = Fixture::new();
    let (expected, batch) = tasks(&fixture);
    let mut captured = work(&fixture);
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    for stage in 0..3 {
        if stage == 1 {
            fixture
                .store
                .commit_accounted_transcript_batch(&batch, 71)
                .unwrap();
        }
        if stage == 2 {
            captured.store = AgentTranscriptStore::new(&fixture.root);
        }
        let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
        let snapshot = history.records.replay_snapshot().unwrap();
        let body = history.records.import_context(history.blocks).unwrap();
        let owner = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(CONVERSATION).unwrap(),
            body,
        )
        .unwrap();
        owner.validate_source_extension(&fixture.source).unwrap();
        assert_eq!(
            owner.context().chronology().len(),
            expected.context().chronology().len()
        );
        for (actual, expected) in owner
            .context()
            .chronology()
            .iter()
            .zip(expected.context().chronology())
        {
            assert_eq!(actual.block(), expected.block());
            assert_eq!(actual.record(), expected.record());
            assert_eq!(actual.sequence(), expected.sequence());
            assert_eq!(actual.retention(), expected.retention());
            assert_eq!(actual.execution_group_id(), expected.execution_group_id());
            assert_eq!(actual.provider_owner(), expected.provider_owner());
        }
        assert!(owner.context().execution_group_is_pending(&fixture.group));
        if stage == 2 {
            let plan = match owner
                .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
                .unwrap()
            {
                ReplayCompactionPreparation::Ready(plan) => plan,
                other => panic!("expected closed prefix: {other:?}"),
            };
            let proposal = fixture
                .store
                .prepare_replay_epoch(
                    crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                        "publisher",
                        "publisher-agent",
                        "publisher-pane",
                    )
                    .unwrap()
                    .capture(Some(&snapshot)),
                    CONVERSATION,
                    Some(fixture.identity.clone()),
                    &owner,
                    plan,
                    ReplayHeadPublication::Summary("task lifecycle head".into()),
                )
                .unwrap();
            fixture.store.publish_replay_epoch(&proposal).unwrap();
            let replay = execute_runtime_agent_history_epoch_work(work(&fixture)).unwrap();
            let body = replay.records.import_context(replay.blocks).unwrap();
            assert!(body.execution_group_is_pending(&fixture.group));
            assert_eq!(
                body.chronology().last().unwrap().block().content,
                "steering 2"
            );
            assert_eq!(
                fixture.store.inspect(CONVERSATION).unwrap(),
                batch.entries()
            );
        }
    }
}

/// A corrupt boundary or an old root without the new explicit witness must
/// refuse the read. Neither can weaken the original head or native protection.
#[test]
fn recorded_tasks_refuse_missing_overlapping_future_and_legacy_boundaries() {
    for fault in ["missing", "overlap", "future", "legacy"] {
        let fixture = Fixture::new();
        let (_, batch) = tasks(&fixture);
        let mut rows = batch.entries().to_vec();
        let admissions = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let record =
                    TranscriptContextRecord::from_transcript_content(&row.content).ok()??;
                matches!(record.event(), TranscriptContextEvent::TaskAdmission { .. })
                    .then_some((index, record))
            })
            .collect::<Vec<_>>();
        let (index, record) = &admissions[if fault == "overlap" { 1 } else { 0 }];
        let TranscriptContextEvent::TaskAdmission {
            event_sequence,
            label,
            content,
            ..
        } = record.event()
        else {
            unreachable!()
        };
        let event = if fault == "legacy" {
            TranscriptContextEvent::user_event(*event_sequence, label, content).unwrap()
        } else {
            let first = match fault {
                "missing" => ContextBlockOccurrenceId::allocate(),
                "future" => admissions[1].1.record().occurrence_id(),
                _ => match admissions[0].1.event() {
                    TranscriptContextEvent::TaskAdmission { task_start, .. } => *task_start,
                    _ => unreachable!(),
                },
            };
            TranscriptContextEvent::task_admission(*event_sequence, first, label, content).unwrap()
        };
        rows[*index].content = TranscriptContextRecord::new(event, record.record(), None)
            .unwrap()
            .to_transcript_content()
            .unwrap();
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
        fixture
            .store
            .commit_accounted_transcript_batch(&batch, 72)
            .unwrap();
        assert!(
            execute_runtime_agent_history_epoch_work(work(&fixture)).is_err(),
            "{fault}"
        );
        let (identity, source) = fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(identity, fixture.identity);
        assert_eq!(
            source.context().chronology(),
            fixture.source.context().chronology()
        );
    }
}
