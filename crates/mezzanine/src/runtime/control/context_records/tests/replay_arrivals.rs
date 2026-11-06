//! Actual immutable append source joins a frozen head through the shared worker.
//! Arrival import preserves source identity and explicit sequence reservations;
//! it grants no historical task, native execution or ordinary append receipt.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};
use crate::test_support::replay_epoch::{CONVERSATION, Fixture};

fn work(fixture: &Fixture) -> RuntimeAgentHistoryEpochWork {
    RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store: fixture.store.clone(),
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "arrival-reader-pane".into(),
            conversation_id: CONVERSATION.into(),
            ephemeral_source_entries: None,
            active_entries: Some(0),
            pending_entries: Default::default(),
        },
    }
}

struct Arrivals {
    batch: crate::storage::transcript::TranscriptAppendBatch,
    reserved: u64,
    user: ContextBlock,
    user_record: ContextBlockRecord,
    result: ContextBlock,
    result_record: ContextBlockRecord,
}

/// Both replay and publication tests use the same original batch construction.
fn arrivals(fixture: &Fixture) -> Arrivals {
    use mez_agent::{ImportedExecutionEvent, ProviderTranscriptEvent};
    let reserved = fixture.source.checkpoint().unwrap().next_event_sequence() + 9;
    let user = ContextBlock::user_event("new user", "genuine later user intent");
    let user_record = ContextBlockRecord::allocate(&user).unwrap();
    let native = fixture.source.context().chronology().last().unwrap();
    let result = ContextBlock::reference_event(
        ContextSourceKind::TranscriptTool,
        "later native result",
        ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
            provider_id: "original-provider".into(),
            tool_call_id: "original-call".into(),
            content: "actual settled result".into(),
        }
        .to_transcript_content(),
    );
    let result_record = ContextBlockRecord::allocate(&result).unwrap();
    let metadata = ImportedExecutionEvent::new(
        result.clone(),
        fixture.group.clone(),
        2,
        native.provider_owner().cloned(),
    )
    .unwrap()
    .with_record(result_record)
    .unwrap();
    let mut rows = fixture.delayed_original_rows();
    for (event, record) in [
        (
            TranscriptContextEvent::user_event(reserved, &user.label, &user.content).unwrap(),
            user_record,
        ),
        (
            TranscriptContextEvent::execution_block_with_metadata(
                result.source,
                &result.label,
                &result.content,
                fixture.group.clone(),
                2,
                metadata.provider_owner().cloned(),
            )
            .unwrap(),
            result_record,
        ),
    ] {
        rows.push(TranscriptEntry {
            conversation_id: CONVERSATION.into(),
            sequence: rows.len() as u64 + 1,
            created_at_unix_seconds: 123,
            role: TranscriptRole::System,
            turn_id: "later-original-turn".into(),
            agent_id: "later-original-agent".into(),
            pane_id: "later-original-pane".into(),
            content: TranscriptContextRecord::new(event, record, None)
                .unwrap()
                .to_transcript_content()
                .unwrap(),
        });
    }
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
    Arrivals {
        batch,
        reserved,
        user,
        user_record,
        result,
        result_record,
    }
}

/// The same original pending proposal is read before append, after real receipt
/// settlement, and after cold reopen. A later native result continues the actual
/// frozen declaration. Gapped original user sequences must not be renumbered.
#[test]
fn replay_arrivals_survive_pending_settlement_and_cold_reopen_once() {
    use mez_agent::ContextEventSequence;
    let fixture = Fixture::new();
    let Arrivals {
        batch,
        reserved,
        user,
        user_record,
        result,
        result_record,
    } = arrivals(&fixture);
    let native = fixture.source.context().chronology().last().unwrap();
    let mut captured = work(&fixture);
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    let mut expected = None;
    for stage in 0..3 {
        if stage == 1 {
            fixture
                .store
                .commit_accounted_transcript_batch(&batch, 91)
                .unwrap();
        } else if stage == 2 {
            fixture
                .store
                .commit_accounted_transcript_batch(&batch, 91)
                .unwrap();
            captured = work(&fixture);
            captured.store = AgentTranscriptStore::new(&fixture.root);
            captured.inputs.pane_id = "cold-different-pane".into();
        }
        let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
        let mut blocks = vec![guidance("current trusted policy")];
        blocks.extend(history.blocks);
        let context = history.records.import_context(blocks).unwrap();
        assert_eq!(
            context.chronology().len(),
            fixture.source.context().chronology().len() + 2
        );
        assert_eq!(
            &context.chronology()[..3],
            fixture.source.context().chronology()
        );
        let arrivals = &context.chronology()[3..];
        assert_eq!(arrivals[0].record(), user_record);
        assert_eq!(arrivals[0].block(), &user);
        assert_eq!(arrivals[0].retention(), mez_agent::ContextRetention::Exact);
        assert_eq!(
            arrivals[0].sequence(),
            ContextEventSequence::new(reserved).unwrap()
        );
        assert_eq!(arrivals[1].record(), result_record);
        assert_eq!(arrivals[1].block(), &result);
        assert_eq!(arrivals[1].provider_owner(), native.provider_owner());
        assert!(context.execution_group_is_pending(&fixture.group));
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(CONVERSATION).unwrap(),
            context.clone(),
        )
        .unwrap();
        assert!(owner.members().starts_with(fixture.source.members()));
        assert!(
            owner.members()[fixture.source.members().len()..]
                .iter()
                .all(|member| member.born_in() == fixture.source.revision())
        );
        if let Some(expected) = &expected {
            assert_eq!(context.chronology(), expected);
        } else {
            expected = Some(context.chronology().to_vec());
        }
        assert_eq!(
            fixture.store.inspect(CONVERSATION).unwrap().len(),
            if stage == 0 { 0 } else { batch.entries().len() }
        );
    }
}

/// New source may be published before its ordinary batch settles. The proposal
/// uses the original history capture, not the later publisher's producer fields;
/// late rows must still deduplicate after this new head covers those arrivals.
#[test]
fn replay_arrival_capture_qualifies_next_publication_before_ordinary_settlement() {
    use crate::storage::transcript::replay_epoch::ReplaySourceOrigin;
    use mez_agent::{
        ReplayCompactionPreparation, ReplayConversationId, ReplayEpochContext,
        ReplayHeadPublication, ReplayTailPolicy,
    };
    let fixture = Fixture::new();
    let arrivals = arrivals(&fixture);
    let mut captured = work(&fixture);
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&arrivals.batch)
            .unwrap();
    let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
    let snapshot = history.records.replay_snapshot().unwrap();
    let mut blocks = vec![guidance("current trusted policy")];
    blocks.extend(history.blocks);
    let body = history.records.import_context(blocks).unwrap();
    let mut owner = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new(CONVERSATION).unwrap(),
        body,
    )
    .unwrap();
    owner.validate_source_extension(&fixture.source).unwrap();
    let instructions = owner
        .context()
        .chronology()
        .iter()
        .filter(|event| event.retention() == mez_agent::ContextRetention::Exact)
        .map(|event| event.sequence())
        .collect::<Vec<_>>();
    let task = owner.begin_instruction_task().unwrap();
    owner
        .bind_instruction_occurrences(&task, &instructions)
        .unwrap();
    owner.complete_instruction_task(&task).unwrap();
    let frontier = owner.context().chronology().last().unwrap().sequence();
    let handle = owner
        .capture_execution_owner(&fixture.group, frontier)
        .unwrap();
    owner.finish_execution_group(&handle).unwrap();
    let plan = match owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected ready source, got {other:?}"),
    };
    let foreign = Fixture::new();
    let foreign_history = execute_runtime_agent_history_epoch_work(work(&foreign)).unwrap();
    let foreign_snapshot = foreign_history.records.replay_snapshot().unwrap();
    assert!(
        fixture
            .store
            .prepare_replay_epoch(
                ReplaySourceOrigin::new("publisher", "publisher-agent", "publisher-pane")
                    .unwrap()
                    .capture(Some(&foreign_snapshot)),
                CONVERSATION,
                Some(fixture.identity.clone()),
                &owner,
                plan.clone(),
                ReplayHeadPublication::Summary("foreign capture".into()),
            )
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        fixture.identity
    );
    let proposal = fixture
        .store
        .prepare_replay_epoch(
            ReplaySourceOrigin::new("later-publisher", "publisher-agent", "publisher-pane")
                .unwrap()
                .capture(Some(&snapshot)),
            CONVERSATION,
            Some(fixture.identity.clone()),
            &owner,
            plan,
            ReplayHeadPublication::Summary("arrival head".into()),
        )
        .unwrap();
    let mut body = owner.into_owned_context();
    let receipt = fixture.store.publish_replay_epoch(&proposal).unwrap();
    receipt.adopt(&fixture.store, &proposal, &mut body).unwrap();
    assert!(fixture.store.inspect(CONVERSATION).unwrap().is_empty());
    for settled in [false, true] {
        if settled {
            fixture
                .store
                .commit_accounted_transcript_batch(&arrivals.batch, 92)
                .unwrap();
        }
        let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
        assert_eq!(history.blocks.len(), 1);
        assert_eq!(history.blocks[0].content, "arrival head");
        assert_eq!(
            history
                .records
                .import_context(history.blocks)
                .unwrap()
                .chronology(),
            body.chronology()
        );
        let fresh = execute_runtime_agent_history_epoch_work(work(&fixture)).unwrap();
        assert_eq!(fresh.blocks[0].content, "arrival head");
    }
}

/// Valid envelope bytes cannot replace the actual pending proposal, duplicate an
/// occurrence, rewind reserved source, or supply a discontinuous native ordinal.
#[test]
fn replay_arrivals_refuse_unowned_duplicate_and_invalid_causal_source() {
    for alteration in 0..5 {
        let fixture = Fixture::new();
        let mut rows = arrivals(&fixture).batch.entries().to_vec();
        if alteration == 1 {
            let mut duplicate = rows[rows.len() - 2].clone();
            duplicate.sequence = rows.len() as u64 + 1;
            rows.push(duplicate);
            assert!(
                crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows.clone())
                    .is_err()
            );
            // Exercise a damaged explicit raw archive as well as the normal
            // append owner's earlier refusal, without manufacturing a receipt.
            for row in &rows {
                fixture.store.append(row).unwrap();
            }
            assert!(execute_runtime_agent_history_epoch_work(work(&fixture)).is_err());
            assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), rows);
            continue;
        } else if alteration == 2 {
            let user_index = rows.len() - 2;
            let original =
                TranscriptContextRecord::from_transcript_content(&rows[user_index].content)
                    .unwrap()
                    .unwrap();
            let TranscriptContextEvent::UserEvent { label, content, .. } = original.event() else {
                unreachable!()
            };
            rows[user_index].content = TranscriptContextRecord::new(
                TranscriptContextEvent::user_event(
                    fixture.source.checkpoint().unwrap().next_event_sequence() - 1,
                    label,
                    content,
                )
                .unwrap(),
                original.record(),
                None,
            )
            .unwrap()
            .to_transcript_content()
            .unwrap();
        } else if alteration == 3 || alteration == 4 {
            let row = rows.last_mut().unwrap();
            let original = TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
                .unwrap();
            let TranscriptContextEvent::ExecutionBlock {
                source,
                label,
                content,
                provider_owner,
                ..
            } = original.event()
            else {
                unreachable!()
            };
            let block = ContextBlock::reference_event(
                *source,
                label,
                if alteration == 4 {
                    mez_agent::ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
                        provider_id: "foreign-provider".into(),
                        tool_call_id: "original-call".into(),
                        content: "foreign result".into(),
                    }
                    .to_transcript_content()
                } else {
                    content.clone()
                },
            );
            row.content = TranscriptContextRecord::new(
                TranscriptContextEvent::execution_block_with_metadata(
                    *source,
                    label,
                    &block.content,
                    fixture.group.clone(),
                    if alteration == 3 { 3 } else { 2 },
                    if alteration == 4 {
                        mez_agent::ProviderContinuityOwner::new(
                            mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
                            "foreign-provider",
                        )
                    } else {
                        provider_owner.clone()
                    },
                )
                .unwrap(),
                original.record().restored(&block).unwrap(),
                None,
            )
            .unwrap()
            .to_transcript_content()
            .unwrap();
        }
        let mut captured = work(&fixture);
        captured.inputs.pending_entries = if alteration == 0 {
            crate::runtime::persistence::RuntimePendingTranscriptContext::unaccounted(rows)
        } else {
            let batch =
                crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
            crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch)
                .unwrap()
        };
        assert!(
            execute_runtime_agent_history_epoch_work(captured).is_err(),
            "alteration={alteration}"
        );
        assert!(fixture.store.inspect(CONVERSATION).unwrap().is_empty());
    }
}
