//! Real completed source and bounded genuine input-rejection recovery.
//! First-turn negative controls remain in the adjacent first_turn
//! module; none of these positive fixtures invents closure in an active task.

use super::*;

/// Every already-issued sibling settles, but a hold must dominate transport
/// failure regardless of completion order and must not acquire an actor retry.
#[test]
fn budget_suspension_mixed_compaction_wave_dominates_transport() {
    for hold_first in [false, true] {
        let (mut service, store, turn) = queued(
            &format!("OLDER_SOURCE {}", "older ".repeat(3000)),
            &format!("NEWER_SOURCE {}", "newer ".repeat(3000)),
            None,
        );
        // Genuine rejection repacks the frozen source into concurrent nodes.
        assert_eq!(
            run_runtime_test_compaction_wave(&mut service, "%1", Err(context_error())).len(),
            1
        );
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        let conversation = task.conversation_id.clone();
        let archive = store.inspect(&conversation).unwrap();
        let before: u64 = service
            .agent_token_usage_for_conversation(&conversation)
            .values()
            .map(|usage| usage.input_tokens)
            .sum();
        service.claim_agent_compaction_task_state("%1", task.clone());
        let mut calls = 0usize;
        let (result, requests) =
            crate::test_support::compaction_source::execute_with_inference(&service, &task, |_| {
                let hold = (calls == 0) == hold_first;
                calls += 1;
                let error = if hold {
                    MezError::new(
                        crate::error::MezErrorKind::BudgetSuspended,
                        "fixture budget hold",
                    )
                } else {
                    MezError::new(crate::error::MezErrorKind::Io, "fixture transport failure")
                };
                Err(
                    error.with_provider_incurred_usage(mez_agent::ModelTokenUsage {
                        input_tokens: 17,
                        output_tokens: 3,
                        ..Default::default()
                    }),
                )
            });
        assert!(
            requests.len() >= 2,
            "fixture must admit concurrent siblings"
        );
        let kind = result.as_ref().unwrap_err().kind();
        let usage = result.as_ref().unwrap_err().provider_incurred_usage();
        let event = crate::test_support::compaction_source::model_result_event(
            "%1",
            task.task_generation,
            result,
        );
        assert!(
            service
                .apply_agent_compaction_transition(event)
                .unwrap()
                .applied
        );
        let requeued = service.agent_is_compacting("%1");
        let after: u64 = service
            .agent_token_usage_for_conversation(&conversation)
            .values()
            .map(|usage| usage.input_tokens)
            .sum();
        let unpublished = store.compaction_epoch(&conversation).unwrap().is_none();
        let preserved = store.inspect(&conversation).unwrap().starts_with(&archive);
        // Retire any RED-run retry without issuing another provider request.
        if requeued {
            service.cancel_current_agent_compaction_task("%1");
        }
        service.terminate_all_pane_processes().unwrap();
        assert_eq!(
            kind,
            crate::error::MezErrorKind::BudgetSuspended,
            "hold_first={hold_first}"
        );
        assert!(
            !requeued,
            "suspension cannot acquire an automatic transport retry"
        );
        assert_eq!(usage.input_tokens, calls as u64 * 17);
        assert_eq!(usage.output_tokens, calls as u64 * 3);
        assert_eq!(after - before, calls as u64 * 17);
        assert!(unpublished && preserved);
        assert_eq!(
            service.agent_turn_ledger().turn(&turn).unwrap().state,
            AgentTurnState::Failed
        );
    }
}

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

/// A hidden provider ceiling must converge from repeated real rejections;
/// accepting every request after the first rejection cannot qualify recovery.
#[test]
fn runtime_compactor_repeated_input_rejection_reaches_provider_capacity() {
    qualify_repeated_input_rejection(false);
}

/// Even a provider that rejects every request must reach a truthful terminal
/// failure while the original source and accounted expense remain intact.
#[test]
fn runtime_compactor_repeated_input_rejection_stops_at_unusable_capacity() {
    qualify_repeated_input_rejection(true);
}

fn qualify_repeated_input_rejection(always_reject: bool) {
    const CALL_BOUND: usize = 64;
    let (mut service, store, turn) = queued(
        &format!("FROZEN_SOURCE {}", "older source 💡 ".repeat(3000)),
        "retained completed answer",
        None,
    );
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let source = original.replay_source.as_ref().unwrap().clone();
    let plan = source.frozen_plan().clone();
    let before = service.agent_turn_contexts()[&turn].clone();
    let archive = store.inspect(&original.conversation_id).unwrap();
    let configured = service
        .provider_registry()
        .provider(&original.model_profile.provider)
        .unwrap();
    let api = mez_agent::resolve_provider_api(&configured.kind, configured.api.as_deref()).unwrap();
    let options =
        crate::runtime::runtime_effective_provider_options(configured, &original.model_profile);
    let usage = mez_agent::ModelTokenUsage {
        input_tokens: 17,
        output_tokens: 3,
        ..Default::default()
    };
    let mut ceiling = None;
    let mut calls = 0usize;
    let mut rejected = 0usize;
    let mut bound_exceeded = false;
    for _ in 0..32 {
        let Some(task) = service.take_pending_agent_compaction_task("%1") else {
            break;
        };
        assert!(std::sync::Arc::ptr_eq(
            task.replay_source.as_ref().unwrap(),
            &source
        ));
        assert_eq!(task.replay_source.as_ref().unwrap().frozen_plan(), &plan);
        service.claim_agent_compaction_task_state("%1", task.clone());
        let (result, requests) = crate::test_support::compaction_source::execute_with_inference(
            &service,
            &task,
            |request| {
                calls += 1;
                // End a RED run through normal failure settlement rather than
                // issuing thousands of near-identical rejected requests.
                if calls > CALL_BOUND {
                    bound_exceeded = true;
                    return Err(
                        MezError::invalid_state("fixture recovery call bound exceeded")
                            .with_provider_incurred_usage(usage),
                    );
                }
                let measured =
                    mez_agent::provider_request_input_estimate(request, api, &options, false)
                        .unwrap()
                        .input_tokens;
                let ceiling = *ceiling.get_or_insert(measured / 3);
                if always_reject || measured > ceiling {
                    rejected += 1;
                    Err(context_error().with_provider_incurred_usage(usage))
                } else {
                    let mut response = runtime_test_compaction_response("bounded complete summary");
                    response.usage = usage;
                    Ok(response)
                }
            },
        );
        assert!(requests.len() <= 4);
        let event = crate::test_support::compaction_source::model_result_event(
            "%1",
            task.task_generation,
            result,
        );
        assert!(
            service
                .apply_agent_compaction_transition(event)
                .unwrap()
                .applied
        );
        if service.agent_is_compacting("%1") {
            assert_eq!(service.agent_turn_contexts().get(&turn), Some(&before));
        } else {
            // Terminal refusal retires the live turn cache; the durable source
            // remains authoritative and is checked below before any adoption.
            assert!(always_reject);
            assert_eq!(
                service.agent_turn_ledger().turn(&turn).unwrap().state,
                AgentTurnState::Failed
            );
        }
        assert!(
            store
                .compaction_epoch(&original.conversation_id)
                .unwrap()
                .is_none()
        );
    }
    assert!(
        !bound_exceeded,
        "real input rejection must make bounded progress; calls={calls}"
    );
    assert!(calls <= CALL_BOUND);
    assert!(
        rejected >= 2,
        "the provider must reject more than one request"
    );
    assert!(
        service.pending_agent_compaction_tasks().is_empty(),
        "input recovery must finish within bounded waves; calls={calls} rejected={rejected}"
    );
    let model = mez_agent::ModelTokenUsageKey::new(
        &original.model_profile.provider,
        &original.model_profile.model,
    );
    let accounted = service.agent_token_usage_for_conversation(&original.conversation_id);
    assert_eq!(
        (
            accounted[&model].input_tokens,
            accounted[&model].output_tokens
        ),
        (calls as u64 * 17, calls as u64 * 3)
    );
    if always_reject {
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(service.take_replay_epoch_attempt_for_tests().is_none());
        assert!(
            store
                .inspect(&original.conversation_id)
                .unwrap()
                .starts_with(&archive)
        );
        assert_eq!(
            service.agent_turn_ledger().turn(&turn).unwrap().state,
            AgentTurnState::Failed
        );
    } else {
        replay_observed::adopt(&mut service, &turn);
        replay_observed::assert_head(&service, &store, &turn, "bounded complete summary");
        assert_eq!(
            &service.agent_turn_contexts()[&turn].chronology()[1..],
            plan.retained_tail()
        );
        assert_eq!(service.pending_agent_provider_tasks().len(), 1);
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
