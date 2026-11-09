//! Provider rejection recovery through real task source and epoch publication.
//! The original test name remains stable while source setup and acknowledgment
//! assertions use the canonical production boundaries.

use super::*;

/// Three tasks' provider rejections use actual task source. Successive publications
/// consume the prior typed head and whole inherited units in order, while
/// inference and physical publication alone cannot resume the provider turn.
#[test]
fn runtime_provider_context_limit_error_compacts_context_multiple_times() {
    let (mut service, store, mut turn_id) = recovery_source(
        &format!("ORIGINAL_FIRST_EPOCH_SOURCE {}", "source ".repeat(5_000)),
        &format!("ORIGINAL_INHERITED_TAIL {}", "tail ".repeat(200)),
        10,
    );
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    let mut inherited = Vec::new();
    let mut previous_head = None;
    let mut previous_summary = None;
    for attempt in 1u32..=3 {
        if attempt > 1 {
            turn_id = crate::test_support::compaction_source::advance_task(
                &mut service,
                &turn_id,
                "continue with genuinely fresh closed source",
            );
        }
        let before = service.agent_turn_contexts()[&turn_id].clone();
        let original = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&conversation).unwrap(),
            before.clone(),
        )
        .unwrap();
        let selected = match original
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 10).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("expected original closed source for epoch {attempt}: {other:?}"),
        };
        let expected_tail = before.chronology()[selected.summary_input().len()..].to_vec();
        if attempt == 1 {
            let group = expected_tail
                .iter()
                .find(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
                .unwrap()
                .execution_group_id()
                .unwrap();
            inherited = expected_tail
                .iter()
                .filter(|event| event.execution_group_id() == Some(group))
                .cloned()
                .collect();
            assert!(
                !inherited.is_empty(),
                "first epoch must retain a closed group"
            );
        } else {
            let group_source = if attempt == 2 {
                &expected_tail[..]
            } else {
                selected.summary_input()
            };
            assert!(
                inherited.iter().all(|event| group_source.contains(event)),
                "second epoch retains the group, third consumes its original occurrences"
            );
            assert_eq!(
                selected.summary_input()[0].record().occurrence_id(),
                previous_head.unwrap()
            );
        }
        assert!(
            service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &turn_id,
                    &error,
                    1,
                )
                .unwrap()
        );
        assert_eq!(service.agent_turn_contexts()[&turn_id], before);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        let input = crate::test_support::compaction_source::rendered_source(
            service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap(),
        )
        .unwrap();
        if attempt == 1 {
            assert!(input.contains("ORIGINAL_FIRST_EPOCH_SOURCE"));
            assert!(!input.contains("ORIGINAL_INHERITED_TAIL"));
        } else {
            assert_eq!(input.matches(previous_summary.unwrap()).count(), 1);
            assert_eq!(input.contains("ORIGINAL_INHERITED_TAIL"), attempt == 3);
            assert!(!input.contains("ORIGINAL_FIRST_EPOCH_SOURCE"));
        }
        let summary = match attempt {
            1 => "first model-authored summary",
            2 => "second model-authored summary",
            _ => "third model-authored summary",
        };
        complete_runtime_test_compaction(&mut service, "%1", summary);
        assert_eq!(service.agent_turn_contexts()[&turn_id], before);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        let completion = replay_publication::execute_pending_publication(&mut service);
        let duplicate = completion.clone();
        assert_eq!(service.agent_turn_contexts()[&turn_id], before);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
        let after = &service.agent_turn_contexts()[&turn_id];
        assert_eq!(after.chronology()[0].block().content, summary);
        assert_eq!(&after.chronology()[1..], expected_tail);
        let (_, cold) = AgentTranscriptStore::new(store.root())
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.revision().get(), u64::from(attempt));
        assert_eq!(cold.context().chronology(), after.chronology());
        assert_eq!(cold.context().stable_slots().len(), 0);
        let head = cold.head().unwrap().occurrence_id();
        assert_ne!(Some(head), previous_head);
        previous_head = Some(head);
        previous_summary = Some(summary);
        assert!(service.agent_provider_task_is_pending(&turn_id));
        assert!(service.pending_agent_compaction_tasks().is_empty());
        assert!(
            !service
                .apply_replay_epoch_completion(duplicate)
                .unwrap()
                .applied
        );
    }
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        normalized_pane_log_text(&pane_text).contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
    assert!(!pane_text.contains("no compactable active turn context remains"));
}

/// A legacy first range may have no independent allowance because later live
/// evidence consumes its small budget. The actual configured request fits one
/// canonical prefix summary and must resume with all later raw source intact.
#[test]
fn runtime_zero_budget_recovery_resumes_fitting_first_segment() {
    let (mut service, store, turn_id) = recovery_source(
        &format!("FIRST_PREFIX_SOURCE {}", "first ".repeat(200)),
        "original first task completed",
        10,
    );
    let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    let provider = RuntimeBatchProvider {
        response: runtime_say_response(
            &turn_id,
            &format!("SECOND_LIVE_SOURCE {}", "second ".repeat(200)),
            false,
        ),
    };
    let execution = service
        .execute_agent_turn_with_provider(&turn_id, &provider, profile.clone())
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let body = service.agent_turn_contexts()[&turn_id].clone();
    let active = body
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::UserInstruction
                && event.retention() == mez_agent::ContextRetention::Exact
        })
        .unwrap()
        .clone();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        body.clone(),
    )
    .unwrap();
    let selected = match owner
        .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 10).unwrap())
        .unwrap()
    {
        mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected completed original prefix: {other:?}"),
    };
    let expected_tail = body.chronology()[selected.summary_input().len()..].to_vec();
    // Derive the original first-range overflow from this real request instead
    // of removing its stable policy. The later closed group makes the legacy
    // planner advertise multiple anchored segments; the actual profile is large
    // enough to fit one prefix rewrite and preserve that group as exact raw.
    let roomy = mez_agent::plan_model_context_compaction_at_consumed_sequence(
        &body,
        30_000,
        1,
        body.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert!(roomy.changes_context());
    let retained_cost = roomy.planning_budget().0 - roomy.summary_budget_words();
    let legacy = mez_agent::plan_model_context_compaction_at_consumed_sequence(
        &body,
        retained_cost - 1,
        1,
        body.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert!(legacy.requires_additional_segments());
    assert!(
        service
            .queue_agent_context_limit_recovery_compaction(
                &turn_id,
                "provider-context-limit-test".into(),
                profile,
                1,
            )
            .unwrap()
    );
    let input = crate::test_support::compaction_source::rendered_source(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap(),
    )
    .unwrap();
    assert!(input.contains("FIRST_PREFIX_SOURCE"));
    assert!(!input.contains("SECOND_LIVE_SOURCE"));
    complete_runtime_test_compaction(&mut service, "%1", "first completed prefix summarized");
    let completion = replay_publication::execute_pending_publication(&mut service);
    assert!(
        service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    let after = &service.agent_turn_contexts()[&turn_id];
    assert_eq!(
        after.chronology()[0].block().content,
        "first completed prefix summarized"
    );
    assert_eq!(&after.chronology()[1..], expected_tail);
    assert!(after.chronology().contains(&active));
    assert!(
        after
            .chronology()
            .iter()
            .any(|event| event.block().content.contains("SECOND_LIVE_SOURCE"))
    );
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology(), after.chronology());
}

/// The configured policy can progress even when the entire closed history fits
/// its optional reservation. A real provider rejection must reach that planner,
/// including when every legacy percentage would incorrectly report no work.
#[test]
fn runtime_context_limit_recovery_replans_retained_tail_noop() {
    for tiny in [false, true] {
        let prompt = if tiny {
            "ORIGINAL_TAIL_TASK tiny".into()
        } else {
            format!("ORIGINAL_TAIL_TASK {}", "task ".repeat(500))
        };
        let answer = if tiny {
            "EXACT_RETAINED_ANSWER done".into()
        } else {
            format!("EXACT_RETAINED_ANSWER {}", "answer ".repeat(500))
        };
        let (mut service, store, turn_id) = recovery_source(&prompt, &answer, 10);
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .conversation_id
            .clone();
        let body = service.agent_turn_contexts()[&turn_id].clone();
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&conversation).unwrap(),
            body.clone(),
        )
        .unwrap();
        let configured = match owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 10).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("actual completed task cannot progress: {other:?}"),
        };
        if tiny {
            let legacy = mez_agent::plan_model_context_compaction_at_consumed_sequence(
                &body,
                30_000,
                1,
                body.event_sequence_high_water_mark(),
            )
            .unwrap();
            assert!(
                !legacy.changes_context(),
                "tiny source should expose the obsolete no-work gate"
            );
        } else {
            let relaxed = match owner
                .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 1).unwrap())
                .unwrap()
            {
                mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
                other => panic!("expected a different optional-tail cut: {other:?}"),
            };
            assert!(relaxed.summary_input().len() > configured.summary_input().len());
        }
        let expected_tail = body.chronology()[configured.summary_input().len()..].to_vec();
        let error = MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(
                r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
            );
        assert!(
            service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &turn_id,
                    &error,
                    1,
                )
                .unwrap(),
            "configured canonical source was hidden (tiny={tiny})"
        );
        assert_eq!(
            service.agent_turn_contexts()[&turn_id].chronology(),
            body.chronology()
        );
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        assert!(task.replay_source.is_some());
        let input = crate::test_support::compaction_source::rendered_source(task).unwrap();
        assert!(
            !task
                .request
                .as_ref()
                .unwrap()
                .messages
                .iter()
                .any(|message| {
                    message
                        .content
                        .contains("within 0 whitespace-delimited words")
                        || message.content.contains("within 0 estimated input tokens")
                })
        );
        for event in configured.summary_input() {
            assert!(input.contains(&event.block().content));
        }
        assert!(
            !input.contains("EXACT_RETAINED_ANSWER"),
            "configured tail entered summary input"
        );
        complete_runtime_test_compaction(&mut service, "%1", "configured tail preserved");
        let completion = replay_publication::execute_pending_publication(&mut service);
        assert!(
            service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
        let actual = &service.agent_turn_contexts()[&turn_id];
        assert_eq!(
            actual.chronology()[0].block().content,
            "configured tail preserved"
        );
        assert_eq!(&actual.chronology()[1..], expected_tail);
        let (_, cold) = AgentTranscriptStore::new(store.root())
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.context().chronology(), actual.chronology());
        assert!(service.agent_provider_task_is_pending(&turn_id));
        assert_eq!(service.agent_compaction_raw_retention_percent(), 10);
    }
}

/// Authentication failure must preserve original source and ordinary usage,
/// without publishing a head, narrowing selected source or retrying the provider.
#[test]
fn runtime_model_compaction_non_context_failure_does_not_back_off() {
    let (mut service, store, turn_id) = recovery_source(
        &format!(
            "terminal-older-marker terminal-newer-marker {}",
            "history ".repeat(3000)
        ),
        "prior task settled",
        10,
    );
    let conversation_id = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let audit = store.inspect(&conversation_id).unwrap();
    let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    let execution_usage = mez_agent::ModelTokenUsage {
        input_tokens: 1200,
        output_tokens: 20,
        reasoning_tokens: 5,
        cached_input_tokens: Some(100),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        execution_usage,
        execution_usage,
        Some(&profile),
    );
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            &turn_id,
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("actual context-recovery transition");
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    assert!(task.replay_source.is_some());
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "forbidden",
                "provider authentication rejected the compaction request",
                Some(r#"{"status_code":401,"error":{"code":"invalid_api_key"}}"#),
            )
            .unwrap()
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert_eq!(
        service.agent_turn_ledger().turn(&turn_id).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let failure = normalized_pane_log_text(&pane_text);
    assert!(
        failure
            .contains("automatic provider-context-limit compaction failed before provider retry"),
        "{pane_text}"
    );
    assert!(
        !failure.contains("automatic output-limit compaction"),
        "{pane_text}"
    );
    assert_eq!(
        service
            .agent_latest_request_usage(&conversation_id)
            .unwrap()
            .usage,
        execution_usage
    );
    assert!(
        store
            .load_replay_epoch(&conversation_id, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(store.inspect(&conversation_id).unwrap().starts_with(&audit));
}

/// Admits and completes the original task with an offline provider before a
/// real successor retires its instruction. Every consumer receives sealed source
/// and settled group metadata; none inserts or reclassifies eligible history.
pub(super) fn recovery_source(
    historical_prompt: &str,
    answer: &str,
    retained_percent: u8,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    crate::test_support::compaction_source::recovery_source(
        test_runtime_service(),
        temp_root("canonical-provider-limit-recovery"),
        historical_prompt,
        answer,
        retained_percent,
    )
}

/// Verifies provider context-limit API errors trigger active-turn compaction
/// and retry before the turn is failed.
///
/// The proactive threshold path can miss provider-specific tokenization or
/// hidden request overhead. When the provider rejects the request anyway, the
/// runtime must compact the stored active-turn context before retrying so the
/// same oversized payload is not sent again.
#[test]
fn runtime_provider_context_limit_error_compacts_context_and_retries() {
    let (mut service, store, turn_id) = recovery_source(
        &format!("ORIGINAL_PROVIDER_LIMIT_SOURCE {}", "cp ".repeat(10_000)),
        "original earlier task completed",
        10,
    );
    let context = &service.agent_turn_contexts()[&turn_id];
    let before_context = context.blocks().to_vec();
    let error = MezError::invalid_state("OpenAI Responses API returned status 400: context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"message":"maximum context length exceeded","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        );
    let transition = service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            &turn_id,
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("context-limit recovery transition");
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::DispatchAgentCompaction { pane_id, .. } if pane_id == "%1"
    )));
    assert_eq!(
        service
            .agent_turn_contexts()
            .get(&turn_id)
            .unwrap()
            .blocks(),
        before_context.as_slice()
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    assert_eq!(service.provider_retry_scheduler_mut().attempt(&turn_id), 1);
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_user_event(
            "post-boundary steering",
            "preserve this post-boundary steering exactly",
        )
        .unwrap();

    complete_runtime_test_compaction(&mut service, "%1", "model-authored context summary");
    assert!(
        !service.agent_provider_task_is_pending(&turn_id),
        "inference is not storage adoption"
    );
    let completion = replay_publication::execute_pending_publication(&mut service);
    assert!(
        !service.agent_provider_task_is_pending(&turn_id),
        "physical publication is not live adoption"
    );
    assert!(
        service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    let compacted_context = service
        .agent_turn_contexts()
        .get(&turn_id)
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compacted_context.contains("model-authored context summary"));
    assert!(compacted_context.contains("preserve this post-boundary steering exactly"));
    assert!(!compacted_context.contains("ORIGINAL_PROVIDER_LIMIT_SOURCE"));
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert_eq!(service.provider_retry_scheduler_mut().attempt(&turn_id), 1);
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 1);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "model-authored context summary"
    );
    assert!(
        !cold
            .context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("ORIGINAL_PROVIDER_LIMIT_SOURCE"))
    );
    // Probe a copy of the real reducer with a duplicate recovery callback. The
    // accepted publication already completed that exact attempt; this must not
    // create a new timer or leave the original reducer in Recovering.
    let mut retry = service.provider_retry_scheduler_mut().clone();
    assert_eq!(
        retry.apply(mez_agent::ProviderRetryEvent::RecoveryCompleted {
            turn_id: turn_id.clone(),
            attempt: 1,
            result: mez_agent::ProviderRetryRecoveryResult::Ready,
        }),
        mez_agent::ProviderRetryTransition::Ignored
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.kind == EventKind::AgentStatus
            && event
                .payload
                .contains(r#""recovery":"provider_context_limit_compaction""#)
    }));
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(
        trace.contains(
            "provider request recovery resuming, reason: provider context limit compaction completed",
        ),
        "{trace}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = normalized_pane_log_text(&pane_text);
    assert!(
        pane_text_unwrapped.contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
}
