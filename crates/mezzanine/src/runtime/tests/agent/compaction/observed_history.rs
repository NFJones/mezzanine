//! Observed-input publication retains exact current source and later arrivals.
//!
//! A real completed task supplies the eligible prefix. Its successor owns all
//! exact instructions; bare historical rows never manufacture task completion.
//! Inference, physical publication and actor adoption remain separate boundaries.

use super::*;

pub(super) fn queued() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    queued_with_cap(20_000)
}

/// Another original controller appends an exact task after capture. Its actual
/// semantic occurrence and ordinary accounted append supply source custody;
/// neither local pane counters nor invented assistant closure do so. The
/// original running body is deliberately unchanged until publication/replay.
pub(super) fn append_external_task(
    service: &RuntimeSessionService,
    store: &AgentTranscriptStore,
    turn: &str,
    text: &str,
) -> mez_agent::ConversationEvent {
    let conversation = &service
        .agent_turn_ledger()
        .turn(turn)
        .unwrap()
        .conversation_id;
    let mut owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(conversation).unwrap(),
        service.agent_turn_contexts()[turn].clone(),
    )
    .unwrap();
    owner
        .admit_instruction_task(
            vec![ContextBlock::user_event("user prompt", text)],
            &[],
            &[],
            0,
        )
        .unwrap();
    let event = owner.context().chronology().last().unwrap().clone();
    let semantic = mez_agent::TranscriptContextEvent::task_admission(
        event.sequence().get(),
        owner
            .task_admission_start(event.record().occurrence_id())
            .unwrap(),
        &event.block().label,
        &event.block().content,
    )
    .unwrap();
    let row = TranscriptEntry {
        conversation_id: conversation.clone(),
        sequence: store.next_sequence(conversation).unwrap(),
        created_at_unix_seconds: 2,
        role: TranscriptRole::System,
        turn_id: "external-original-task".into(),
        agent_id: "agent-%external".into(),
        pane_id: "%external".into(),
        content: mez_agent::transcript::TranscriptContextRecord::new(
            semantic,
            event.record(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap(),
    };
    store
        .commit_accounted_transcript_batch(
            &crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![row]).unwrap(),
            1,
        )
        .unwrap();
    event
}

/// Freeze the real model allowance before any task or compactor is admitted.
pub(super) fn prepared_with_cap(
    input_limit: usize,
    native_shell: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("observed-canonical-history"));
    service.set_agent_transcript_store(store.clone());
    let shell_mode = if native_shell { "native" } else { "pane" };
    let permissions = if native_shell {
        "[permissions]\nsandbox=\"policy-only\"\n"
    } else {
        ""
    };
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "observed-canonical-history".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                r#"[agents]
default_provider="runtime-batch"
default_model_profile="observed-canonical-history"
shell_mode="{shell_mode}"
compaction_raw_retention_percent=1
[providers.runtime-batch]
kind="openai"
models=["test"]
default_model="test"
[model_profiles.observed-canonical-history]
provider="runtime-batch"
model="test"
context_window_tokens=40000
max_input_tokens={input_limit}
{permissions}
"#
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    if native_shell {
        service.start_initial_pane_process(None).unwrap();
        wait_until_primary_shell_foreground(&mut service, "%1");
        service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
        service.permission_policy_mut().set_approval_bypass(true);
    }
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
        &mut service,
        &primary,
        &"TYPED_OLD_WORK ".repeat(80),
        "the original work completed",
        "CURRENT_USER_PROMPT Continue with the collected evidence.",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    for (label, text) in [
        (
            "older steering",
            "EXACT_OLDER_USER_INSTRUCTION Keep the deployment rollback plan available.",
        ),
        (
            "second steering",
            "EXACT_SECOND_USER_INSTRUCTION Preserve the migration ordering.",
        ),
        (
            "third steering",
            "EXACT_THIRD_USER_INSTRUCTION Do not remove the compatibility check.",
        ),
    ] {
        service
            .agent_turn_contexts_mut()
            .get_mut(&turn)
            .unwrap()
            .append_user_event(label, text)
            .unwrap();
    }
    (service, store, turn)
}

pub(super) fn queued_with_cap(
    input_limit: usize,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, turn) = prepared_with_cap(input_limit, false);
    let mut response = runtime_say_response(&turn, "continue", false);
    response.usage.input_tokens = input_limit as u64;
    response.usage.output_tokens = 1;
    let provider = RuntimeBatchProvider { response };
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let execution = service
        .execute_agent_turn_with_provider(&turn, &provider, profile)
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        execution.latest_response_usage.input_tokens,
        input_limit as u64
    );
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(task.replay_source.is_some());
    let input = &task.request.messages.last().unwrap().content;
    assert!(input.contains("TYPED_OLD_WORK"));
    for marker in [
        "CURRENT_USER_PROMPT",
        "EXACT_OLDER_USER_INSTRUCTION",
        "EXACT_SECOND_USER_INSTRUCTION",
        "EXACT_THIRD_USER_INSTRUCTION",
    ] {
        assert!(
            !input.contains(marker),
            "exact source entered summary input: {marker}"
        );
    }
    (service, store, turn)
}

/// A post-plan assistant occurrence is admitted through the original live
/// owner and stays pending. The actual publication must back it before its
/// ordinary archive append, preserving the current user root and all records.
#[test]
fn runtime_observed_compaction_refresh_preserves_current_user_prompt() {
    let (mut service, store, turn) = queued();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let group = mez_agent::ContextExecutionGroupId::new("late-active-transcript").unwrap();
    let body = service.agent_turn_contexts_mut().get_mut(&turn).unwrap();
    body.update_replay_owner(|owner| {
        owner.append_execution_events([mez_agent::ContextConversationAppend::assistant(
            "late active transcript",
            "LATE_ACTIVE_TURN_TRANSCRIPT_ENTRY",
            group.clone(),
        )])?;
        Ok(())
    })
    .unwrap();
    let before = service.agent_turn_contexts()[&turn].clone();
    let arrival = before.chronology().last().unwrap().clone();
    assert!(before.execution_group_is_pending(&group));
    assert!(
        !store
            .inspect(&conversation)
            .unwrap()
            .iter()
            .any(|row| row.content.contains("LATE_ACTIVE_TURN_TRANSCRIPT_ENTRY"))
    );
    let summary = "observed input summary without exact user instructions";
    complete_runtime_test_compaction(&mut service, "%1", summary);
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, summary);
    let after = &service.agent_turn_contexts()[&turn];
    assert!(
        !after
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
    assert_eq!(
        after
            .chronology()
            .iter()
            .filter(|event| event.record() == arrival.record())
            .count(),
        1
    );
    assert!(after.chronology().contains(&arrival));
    assert!(after.execution_group_is_pending(&group));
    for event in before
        .chronology()
        .iter()
        .filter(|event| event.retention() == mez_agent::ContextRetention::Exact)
    {
        assert!(
            after.chronology().contains(event),
            "current exact source changed"
        );
    }
    assert!(
        after
            .blocks()
            .iter()
            .any(|block| block.label == "user prompt"
                && block.content == "CURRENT_USER_PROMPT Continue with the collected evidence.")
    );
    assert_eq!(
        after.chronology()[0].block().source,
        ContextSourceKind::CompactionPrefix
    );
    assert!(service.agent_provider_task_is_pending(&turn));
}

/// Exact instructions omitted from summary input survive both accepted replay
/// and normal next-task composition with their source roles, IDs and costs.
#[test]
fn runtime_observed_compaction_retains_unsummarized_exact_history_for_replay() {
    let (mut service, store, turn) = queued();
    let exact = service.agent_turn_contexts()[&turn]
        .chronology()
        .iter()
        .filter(|event| event.block().content.starts_with("EXACT_"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(exact.len(), 3);
    let summary = "observed input summary without exact user instructions";
    complete_runtime_test_compaction(&mut service, "%1", summary);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, summary);
    for event in &exact {
        assert!(
            service.agent_turn_contexts()[&turn]
                .chronology()
                .contains(event)
        );
    }
    let next = service
        .agent_context_for_pane_prompt("%1", "NEXT_USER_PROMPT Continue.", 0)
        .unwrap();
    assert_eq!(
        next.chronology()[0].block().source,
        ContextSourceKind::CompactionPrefix
    );
    assert_eq!(next.chronology()[0].block().content, summary);
    assert!(
        !next
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
    for original in &exact {
        let matches = next
            .chronology()
            .iter()
            .filter(|event| event.record() == original.record())
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].block(), original.block());
        assert_eq!(matches[0].sequence(), original.sequence());
    }
}
