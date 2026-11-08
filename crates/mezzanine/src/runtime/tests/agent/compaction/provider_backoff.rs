//! Real completed source, frozen whole-unit walkback and recursive rejection
//! recovery. First-turn negative controls remain in the adjacent first_turn
//! module; none of these positive fixtures invents closure in an active task.

use super::*;

fn context_error() -> MezError {
    MezError::invalid_state("provider context length exceeded").with_provider_failure_json(
        r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
    )
}

fn queued(
    older: &str,
    newer: &str,
    input_limit: Option<usize>,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, turn) = provider_limit_replay::recovery_source(older, newer, 1);
    if let Some(limit) = input_limit {
        let mut profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        profile
            .provider_options
            .insert("max_input_tokens".into(), limit.to_string());
        service.set_agent_turn_model_profile(turn.clone(), profile);
    }
    queue_rejection(&mut service, &turn);
    (service, store, turn)
}

fn queue_rejection(service: &mut RuntimeSessionService, turn: &str) {
    let transition = service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            turn,
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &context_error(),
        )
        .unwrap()
        .expect("actual context-limit recovery transition");
    assert!(transition.side_effects.iter().any(|effect| matches!(effect,
        RuntimeSideEffect::DispatchAgentCompaction { pane_id, .. } if pane_id == "%1"
    )));
    assert!(!service.agent_provider_task_is_pending(turn));
}

/// The offline rejection fixture uses the real configured wire encoder and
/// options. No credential, network request or fabricated append receipt is used.
fn claim(
    service: &mut RuntimeSessionService,
    mut task: crate::runtime::RuntimeAgentCompactionTask,
) -> usize {
    let provider = service
        .provider_registry()
        .provider(&task.model_profile.provider)
        .unwrap();
    let api = mez_agent::resolve_provider_api(&provider.kind, provider.api.as_deref()).unwrap();
    let options =
        crate::runtime::config::runtime_effective_provider_options(provider, &task.model_profile);
    let bytes = mez_agent::provider_request_input_estimate(&task.request, api, &options, false)
        .unwrap()
        .wire_bytes;
    task.compaction_request_shape = Some((api, options, false));
    service.claim_agent_compaction_task_state("%1", task);
    bytes
}

fn reject_compactor(service: &mut RuntimeSessionService) {
    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "invalid_state",
                "provider context length exceeded",
                Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
            )
            .unwrap()
    );
}

fn input(service: &RuntimeSessionService) -> String {
    service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A newest complete group that fits the configured raw reservation remains
/// exact through a rejected auxiliary request and recursive synthesis. Source
/// selection stays frozen; inference and commit alone cannot resume execution.
#[test]
fn runtime_compactor_walkback_preserves_newest_group() {
    let (mut service, store, second) = provider_limit_replay::recovery_source(
        &format!("OLDER_WALKBACK {}", "older ".repeat(3_000)),
        &format!("MIDDLE_WALKBACK {}", "middle ".repeat(3_000)),
        1,
    );
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &second,
        "NEWER_WALKBACK exact newest result",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let primary = service.session.layout_owner_client_id().unwrap().clone();
    let start = service
        .execute_agent_shell_command(&primary, "continue after rejection")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let turn = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    let before = service.agent_turn_contexts()[&turn].clone();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        before.clone(),
    )
    .unwrap();
    let selected = match owner
        .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 1).unwrap())
        .unwrap()
    {
        mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected real completed prefix: {other:?}"),
    };
    let expected_tail = selected.retained_tail().to_vec();
    assert!(
        expected_tail
            .iter()
            .any(|event| event.block().content.contains("NEWER_WALKBACK"))
    );
    let audit = store.inspect(&conversation).unwrap();
    queue_rejection(&mut service, &turn);
    assert!(input(&service).contains("OLDER_WALKBACK"));
    assert!(input(&service).contains("MIDDLE_WALKBACK"));
    assert!(!input(&service).contains("NEWER_WALKBACK"));
    let initial = service.take_pending_agent_compaction_task("%1").unwrap();
    let original_source = initial.replay_source.clone().unwrap();
    let original_bytes = claim(&mut service, initial);
    reject_compactor(&mut service);
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &original_source,
        retry.replay_source.as_ref().unwrap()
    ));
    let crate::runtime::RuntimeAgentCompactionTarget::ActiveTurn { pending_blocks, .. } =
        &retry.target
    else {
        panic!("expected original active source");
    };
    assert_eq!(pending_blocks.len(), 1);
    assert!(input(&service).contains("OLDER_WALKBACK"));
    assert!(!input(&service).contains("NEWER_WALKBACK"));
    assert!(
        mez_agent::openai_responses_request_body_with_stream(&retry.request, false)
            .unwrap()
            .len()
            < original_bytes
    );
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    complete_runtime_test_compaction(&mut service, "%1", "older chunk summary");
    assert!(input(&service).contains("MIDDLE_WALKBACK"));
    assert!(!input(&service).contains("NEWER_WALKBACK"));
    complete_runtime_test_compaction(&mut service, "%1", "middle chunk summary");
    assert!(input(&service).contains("older chunk summary"));
    assert!(input(&service).contains("middle chunk summary"));
    complete_runtime_test_compaction(&mut service, "%1", "older work summarized");
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "older work summarized");
    let after = &service.agent_turn_contexts()[&turn];
    assert_eq!(&after.chronology()[1..], expected_tail);
    assert!(
        !after
            .blocks()
            .iter()
            .any(|block| block.content.contains("OLDER_WALKBACK")
                || block.content.contains("MIDDLE_WALKBACK"))
    );
    assert_eq!(store.inspect(&conversation).unwrap(), audit);
}

/// Failure validation uses the original claim before retiring it. A replaced
/// profile must fail the turn, rather than redispatch the old source under a
/// new owner or strand a running turn after the auxiliary claim disappears.
#[test]
fn runtime_compactor_walkback_validation_failure_settles_turn() {
    let (mut service, store, turn) = queued(
        &format!("older {}", "source ".repeat(3_000)),
        &format!("newer {}", "result ".repeat(3_000)),
        None,
    );
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let audit = store.inspect(&conversation).unwrap();
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    claim(&mut service, task);
    let mut replacement = service.agent_turn_model_profile(&turn).unwrap().clone();
    replacement.provider = "missing-walkback-provider".into();
    service.set_agent_turn_model_profile(turn.clone(), replacement);
    reject_compactor(&mut service);
    assert!(!service.agent_is_compacting("%1"));
    assert!(!service.agent_provider_task_is_pending(&turn));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(store.inspect(&conversation).unwrap().starts_with(&audit));
}

/// When retaining the newest source cannot fit the ordinary request, recursive
/// chunks must cover the entire original plan and synthesize one final head.
/// Rejections beyond the ordinary retry count still make strict wire progress.
#[test]
fn runtime_model_compaction_recursively_shrinks_without_exact_tail_fallback() {
    let (mut service, store, turn) = queued(
        &format!("older-backoff-marker {}", "older ".repeat(6_000)),
        &format!("newer-exact-marker {}", "newer ".repeat(6_000)),
        Some(20_000),
    );
    let before = service.agent_turn_contexts()[&turn].clone();
    let mut initial = service.take_pending_agent_compaction_task("%1").unwrap();
    let source = initial.replay_source.clone().unwrap();
    if let crate::runtime::RuntimeAgentCompactionTarget::ActiveTurn {
        compaction_backoff_attempt,
        ..
    } = &mut initial.target
    {
        *compaction_backoff_attempt = mez_agent::DEFAULT_PROVIDER_RETRY_POLICY.max_attempts;
    }
    let original_bytes = claim(&mut service, initial);
    reject_compactor(&mut service);
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &source,
        retry.replay_source.as_ref().unwrap()
    ));
    let crate::runtime::RuntimeAgentCompactionTarget::ActiveTurn {
        pending_blocks,
        compaction_backoff_attempt,
        ..
    } = &retry.target
    else {
        panic!("expected original active target");
    };
    assert_eq!(
        *compaction_backoff_attempt,
        mez_agent::DEFAULT_PROVIDER_RETRY_POLICY.max_attempts + 1
    );
    assert_eq!(pending_blocks.len(), 1);
    assert!(
        pending_blocks[0]
            .iter()
            .any(|block| block.content.contains("newer-exact-marker"))
    );
    assert!(input(&service).contains("older-backoff-marker"));
    assert!(!input(&service).contains("newer-exact-marker"));
    assert!(
        mez_agent::openai_responses_request_body_with_stream(&retry.request, false)
            .unwrap()
            .len()
            < original_bytes
    );
    complete_runtime_test_compaction(&mut service, "%1", "model-authored older-group summary");
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    assert!(!input(&service).contains("older-backoff-marker"));
    assert!(input(&service).contains("newer-exact-marker"));
    complete_runtime_test_compaction(&mut service, "%1", "model-authored newer-group summary");
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    assert!(input(&service).contains("model-authored older-group summary"));
    assert!(input(&service).contains("model-authored newer-group summary"));
    complete_runtime_test_compaction(&mut service, "%1", "recursive final-fit summary");
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "recursive final-fit summary");
    let after = &service.agent_turn_contexts()[&turn];
    assert!(
        !after
            .blocks()
            .iter()
            .any(|block| block.content.contains("older-backoff-marker")
                || block.content.contains("newer-exact-marker"))
    );
}

/// The rejection bound is the actual full encoded ordinary request, never a
/// sentinel byte count. An expanded summary cannot publish a head or redispatch
/// that request even when its auxiliary output is otherwise valid and bounded.
#[test]
fn runtime_context_limit_recovery_does_not_resend_non_shrinking_request() {
    let (mut service, store, turn) = provider_limit_replay::recovery_source(
        &format!("monotonic-gate-marker {}", "evidence ".repeat(1_000)),
        "original result",
        1,
    );
    let original_frontier = service.agent_turn_contexts()[&turn].event_sequence_high_water_mark();
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let provider = RuntimeBatchProvider {
        response: runtime_say_response(&turn, "accepted observation", false),
    };
    let execution = service
        .execute_agent_turn_with_provider(&turn, &provider, profile)
        .unwrap();
    let rejected_bytes =
        mez_agent::openai_responses_request_body_with_stream(&execution.request, false)
            .unwrap()
            .len();
    service
        .record_claimed_agent_provider_context_for_tests(&turn, original_frontier)
        .unwrap();
    queue_rejection(&mut service, &turn);
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let audit = store.inspect(&conversation).unwrap();
    let task = service
        .pending_agent_compaction_task_mut_for_tests("%1")
        .unwrap();
    if let crate::runtime::RuntimeAgentCompactionTarget::ActiveTurn {
        rejected_request_bytes,
        rejected_request_stream,
        ..
    } = &mut task.target
    {
        *rejected_request_bytes = Some(rejected_bytes);
        *rejected_request_stream = Some(false);
    }
    complete_runtime_test_compaction(&mut service, "%1", &"expanded-summary ".repeat(6_000));
    assert!(
        service.take_replay_epoch_attempt_for_tests().is_none(),
        "a non-shrinking continuation must not reach the publication worker"
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!service.agent_provider_task_is_pending(&turn));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(store.inspect(&conversation).unwrap().starts_with(&audit));
    let text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        normalized_pane_log_text(&text)
            .contains("could not produce a smaller OpenAI Responses request"),
        "{text}"
    );
}

/// Bounded output may still make the complete continuation too large. A real
/// smaller candidate may publish; repeating an oversized candidate must stop
/// after its failed size-progress check without writing or changing live source.
#[test]
fn runtime_canonical_final_request_retries_require_complete_size_progress() {
    for reduces in [false, true] {
        let (mut service, store, turn) = queued(
            &format!("ORIGINAL_LARGE_PREFIX {}", "source ".repeat(8_000)),
            "original final result",
            Some(20_000),
        );
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn)
            .unwrap()
            .conversation_id
            .clone();
        let before = service.agent_turn_contexts()[&turn].clone();
        let large = "large-summary ".repeat(8_000);
        complete_runtime_test_compaction(&mut service, "%1", &large);
        assert_eq!(service.agent_turn_contexts()[&turn], before);
        assert!(!service.agent_provider_task_is_pending(&turn));
        assert!(service.take_replay_epoch_attempt_for_tests().is_none());
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        let crate::runtime::RuntimeAgentCompactionTarget::ActiveTurn {
            final_request_retry,
            ..
        } = &retry.target
        else {
            panic!("expected same original source target");
        };
        assert_eq!(final_request_retry.attempts, 1);
        assert!(final_request_retry.last_input_tokens.is_some());
        assert!(
            final_request_retry
                .summary_ceiling
                .is_some_and(|ceiling| ceiling > 0)
        );
        assert!(input(&service).contains("ORIGINAL_LARGE_PREFIX"));
        if reduces {
            complete_runtime_test_compaction(&mut service, "%1", "valid complete candidate");
            assert_eq!(service.agent_turn_contexts()[&turn], before);
            replay_observed::adopt(&mut service, &turn);
            replay_observed::assert_head(&service, &store, &turn, "valid complete candidate");
        } else {
            complete_runtime_test_compaction(&mut service, "%1", &large);
            assert!(service.pending_agent_compaction_tasks().is_empty());
            assert!(service.take_replay_epoch_attempt_for_tests().is_none());
            assert!(!service.agent_provider_task_is_pending(&turn));
            assert_eq!(
                service.agent_turn_ledger().turn(&turn).unwrap().state,
                AgentTurnState::Failed
            );
            assert!(
                store
                    .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                    .unwrap()
                    .is_none()
            );
        }
    }
}
