//! Rejected completed summaries remain private while exact auxiliary work repairs them.

use super::*;

/// Invalid later chunks preserve earlier accepted summaries and pending input;
/// invalid synthesis repeats the exact private synthesis request rather than
/// re-staging its rejected text. A valid next response advances finite source.
#[test]
fn runtime_compaction_invalid_chunk_preserves_accepted_staging_and_synthesis() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    let RuntimeAgentCompactionTarget::ActiveTurn {
        completed_summaries,
        pending_blocks,
        current_blocks,
        completed_responses,
        ..
    } = &mut task.target
    else {
        panic!("active turn");
    };
    completed_summaries.push("accepted earlier chunk".into());
    pending_blocks.push(current_blocks.clone());
    *completed_responses = 32;
    let conversation = task.conversation_id.clone();
    service.queue_agent_compaction_task(task);
    complete_runtime_test_compaction(&mut service, "%1", "");
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let RuntimeAgentCompactionTarget::ActiveTurn {
        completed_summaries,
        pending_blocks,
        completed_responses,
        ..
    } = &retry.target
    else {
        panic!("active turn");
    };
    assert_eq!(completed_summaries, &["accepted earlier chunk"]);
    assert_eq!(pending_blocks.len(), 1);
    assert_eq!(*completed_responses, 32);
    complete_runtime_test_compaction(&mut service, "%1", "accepted later chunk");
    complete_runtime_test_compaction(&mut service, "%1", "accepted last chunk");
    let synthesis = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    assert!(
        synthesis
            .request
            .messages
            .last()
            .unwrap()
            .content
            .contains("accepted earlier chunk")
    );
    complete_runtime_test_compaction(&mut service, "%1", "");
    let correction = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(
        correction.request.messages.last(),
        synthesis.request.messages.last()
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert!(!service.agent_provider_task_is_pending(&turn));
    complete_runtime_test_compaction(&mut service, "%1", "accepted final synthesis");
    assert!(service.agent_provider_task_is_pending(&turn));
}

/// A completed batch with executable content is malformed summary output, not
/// authorization to run actions. Its finite correction replaces only inference
/// and requires a final summary response before ordinary work can continue.
#[test]
fn runtime_compaction_malformed_completed_batch_is_private_and_bounded() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    let conversation = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .conversation_id
        .clone();
    for attempt in 0..3 {
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        service.claim_agent_compaction_task_state("%1", task);
        let mut response = runtime_test_compaction_response("must not use malformed raw fallback");
        response.action_batch = Some(mez_agent::MaapBatch {
            rationale: "malformed fixture".into(),
            actions: vec![],
        });
        assert!(
            service
                .apply_agent_compaction_completed_event("%1", response)
                .unwrap()
        );
        assert_eq!(service.agent_is_compacting("%1"), attempt < 2);
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    }
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
}

/// Repeated blank responses have exactly two auxiliary corrections even when
/// transport retries are unlimited. No-progress oversized output fails on its
/// next completion rather than redispatching indefinitely or publishing it.
#[test]
fn runtime_compaction_summary_correction_exhausts_without_publication() {
    for empty in [false, true] {
        let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
        service.configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
            max_attempts: 0,
            unlimited: true,
            initial_delay_ms: 20,
            max_delay_ms: 40,
        });
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        let invalid = if empty {
            String::new()
        } else {
            "x ".repeat(50_000)
        };
        let attempts = if empty { 3 } else { 2 };
        for index in 0..attempts {
            complete_runtime_test_compaction(&mut service, "%1", &invalid);
            assert!(
                store
                    .compaction_epoch(&task.conversation_id)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(service.agent_is_compacting("%1"), index + 1 < attempts);
        }
        assert_eq!(
            service.agent_turn_ledger().turn(&turn).unwrap().state,
            AgentTurnState::Failed
        );
        assert!(!service.agent_provider_task_is_pending(&turn));
    }
}

/// A selected-source rewrite is an integrity failure even when output is blank.
/// Correctable output must not mask a genuinely changed frozen chronology or
/// queue another inference against corrupt evidence.
#[test]
fn runtime_compaction_invalid_summary_does_not_retry_mutated_source() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn)
        .unwrap()
        .replace_after_compaction(vec![ContextBlock::user_event("mutated", "new source")])
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "");
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(
        store
            .compaction_epoch(&task.conversation_id)
            .unwrap()
            .is_none()
    );
}

/// Discarded output still incurred expense once; duplicate delivery cannot
/// account it again, overwrite the ordinary execution sample or retire the
/// newly queued correction. Cancellation then makes its late completion inert.
#[test]
fn runtime_compaction_invalid_summary_usage_duplicate_and_cancel_are_exact() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    let generation = task.task_generation;
    let conversation = task.conversation_id.clone();
    let before = service.agent_token_usage_for_conversation(&conversation);
    let sample = service.agent_latest_request_usage(&conversation).cloned();
    service.claim_agent_compaction_task_state("%1", task);
    let mut response = runtime_test_compaction_response("");
    response.usage.input_tokens = 37;
    response.usage.output_tokens = 5;
    let event = crate::runtime::AgentCompactionEvent::Completed {
        pane_id: "%1".into(),
        task_generation: generation,
        response: Box::new(response),
    };
    assert!(
        service
            .apply_agent_compaction_transition(event.clone())
            .unwrap()
            .applied
    );
    let expense = service.agent_token_usage_for_conversation(&conversation);
    assert_ne!(expense, before);
    assert!(
        !service
            .apply_agent_compaction_transition(event)
            .unwrap()
            .applied
    );
    assert_eq!(
        expense,
        service.agent_token_usage_for_conversation(&conversation)
    );
    assert_eq!(
        sample,
        service.agent_latest_request_usage(&conversation).cloned()
    );
    let retry = service.take_pending_agent_compaction_task("%1").unwrap();
    let generation = retry.task_generation;
    service.claim_agent_compaction_task_state("%1", retry);
    service.cancel_current_agent_compaction_task("%1");
    assert!(
        !service
            .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
                pane_id: "%1".into(),
                task_generation: generation,
                response: Box::new(runtime_test_compaction_response("must not publish")),
            })
            .unwrap()
            .applied
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert!(!service.agent_provider_task_is_pending(&turn));
}

/// Complete but over-budget, empty and byte-heavy outputs must not settle the
/// waiting turn. Each case retains the frozen source and logical epoch under a
/// fresh generation, publishes nothing, and resumes ordinary work once after a
/// valid model response. This covers rejection before final-request sizing.
#[test]
fn runtime_compaction_corrects_invalid_completed_summary_before_publication() {
    for kind in ["budget", "empty", "bytes"] {
        let (mut service, store, turn_id) = queue_observed_input_compaction_with_exact_history();
        let original = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } =
            &original.target
        else {
            panic!("expected active turn");
        };
        let invalid = match kind {
            "budget" => "word ".repeat(plan.summary_budget_words() + 1),
            "empty" => " \n\t".to_string(),
            _ => "界".repeat(128 * 1024 / 3 + 1),
        };
        complete_runtime_test_compaction(&mut service, "%1", &invalid);
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap_or_else(|| panic!("{kind}: invalid output must queue auxiliary correction"));
        assert_ne!(retry.task_generation, original.task_generation);
        assert_eq!(retry.compaction_epoch, original.compaction_epoch);
        assert_eq!(retry.resume_turn_id, original.resume_turn_id);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            store
                .compaction_epoch(&original.conversation_id)
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .agent_turn_ledger()
                .turns()
                .iter()
                .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
        );
        complete_runtime_test_compaction(&mut service, "%1", "valid short summary");
        let epoch = store
            .compaction_epoch(&original.conversation_id)
            .unwrap()
            .unwrap();
        assert_eq!(epoch.ranges.len(), 1);
        assert_eq!(epoch.ranges[0].summary, "valid short summary");
        assert!(service.agent_provider_task_is_pending(&turn_id));
        assert!(!service.agent_is_compacting("%1"));
    }
}
