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

/// Genuine auxiliary input rejection repacks the same selected occurrences,
/// without walking down the configured retention policy or publishing siblings.
#[test]
fn runtime_compactor_input_rejection_preserves_frozen_source_and_tail() {
    let (mut service, store, turn) = queued(
        &format!("OLDER_SOURCE {}", "older ".repeat(3000)),
        &format!("NEWER_SOURCE {}", "newer ".repeat(3000)),
        None,
    );
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let source = original.replay_source.as_ref().unwrap().clone();
    let plan = source.frozen_plan().clone();
    let before = service.agent_turn_contexts()[&turn].clone();
    let rejected = run_runtime_test_compaction_wave(&mut service, "%1", Err(context_error()));
    assert_eq!(rejected.len(), 1);
    assert!(
        store
            .compaction_epoch(&original.conversation_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        retry.replay_source.as_ref().unwrap(),
        &source
    ));
    assert_eq!(retry.replay_source.as_ref().unwrap().frozen_plan(), &plan);
    let mut calls = 0;
    for _ in 0..32 {
        if service.pending_agent_compaction_tasks().is_empty() {
            break;
        }
        let requests = run_runtime_test_compaction_wave(
            &mut service,
            "%1",
            Ok(runtime_test_compaction_response(
                "one final frozen-source summary",
            )),
        );
        assert!(requests.len() <= 4);
        calls += requests.len();
        assert_eq!(service.agent_turn_contexts()[&turn], before);
    }
    assert!(
        calls >= 2,
        "a private fragment and its necessary carry are actual work"
    );
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "one final frozen-source summary");
    assert_eq!(
        &service.agent_turn_contexts()[&turn].chronology()[1..],
        plan.retained_tail()
    );
    for selected in plan.summary_input() {
        assert!(
            !service.agent_turn_contexts()[&turn]
                .chronology()
                .iter()
                .any(|event| event.record() == selected.record())
        );
    }
}

/// Genuine auxiliary framing capacity fails before inference without changing
/// source selection, truncating the exact task, or publishing a partial head.
#[test]
fn runtime_compactor_unusable_input_capacity_fails_without_model_dispatch() {
    let (mut service, store, turn) = queued("older source", "newer source", Some(1));
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let archive = store.inspect(&conversation).unwrap();
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("must never be requested")),
    );
    assert!(requests.is_empty());
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert!(store.inspect(&conversation).unwrap().starts_with(&archive));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
}

/// A fitting frozen input needs one summary even if that usable result is
/// longer than source or would make a subsequent ordinary request look large.
#[test]
fn runtime_context_limit_recovery_accepts_nonshrinking_usable_final() {
    let (mut service, store, turn) = queued("older source", "newer source", None);
    let summary = format!("{}END", "usable continuation ".repeat(4000));
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(&summary)),
    );
    assert_eq!(requests.len(), 1);
    assert!(service.pending_agent_compaction_tasks().is_empty());
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, &summary);
}
