//! Auxiliary transport retry through real common-graph worker results.
//! Original source and accepted private siblings survive exact retry, while
//! actor timers, pause, cancellation and publication retain their own authority.

use super::*;
use crate::runtime::AgentCompactionEvent;

/// A genuine auxiliary wave must not turn misleading failure metadata into
/// another malformed-output repair. Runtime reversible holding is downstream;
/// this control proves no repair/publication and conservation of incurred usage.
#[test]
fn budget_suspension_compaction_wave_never_repairs_provider_output() {
    let (mut service, store, turn) = replay_observed::queued();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let prior: u64 = service
        .agent_token_usage_for_conversation(&conversation)
        .values()
        .map(|u| u.input_tokens)
        .sum();
    let error = MezError::new(
        crate::error::MezErrorKind::BudgetSuspended,
        "provider MAAP output is malformed: context length exceeded; retry",
    )
    .with_provider_failure_json(r#"{"type":"malformed_model_output","status_code":429}"#)
    .with_provider_incurred_usage(mez_agent::ModelTokenUsage {
        input_tokens: 17,
        ..Default::default()
    });
    assert_eq!(
        run_runtime_test_compaction_wave(&mut service, "%1", Err(error)).len(),
        1
    );
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    let actual: u64 = service
        .agent_token_usage_for_conversation(&conversation)
        .values()
        .map(|u| u.input_tokens)
        .sum();
    assert_eq!(actual - prior, 17);
}

fn failure(kind: &str) -> MezError {
    let kind = match kind {
        "io" => crate::error::MezErrorKind::Io,
        "forbidden" => crate::error::MezErrorKind::Forbidden,
        "conflict" => crate::error::MezErrorKind::Conflict,
        _ => panic!("fixture error kind"),
    };
    MezError::new(
        kind,
        "provider HTTP response read failed: unexpected EOF during chunk size line",
    )
    .with_provider_incurred_usage(mez_agent::ModelTokenUsage {
        input_tokens: 17,
        ..Default::default()
    })
}

fn fail_model(service: &mut RuntimeSessionService, kind: &str) -> Vec<mez_agent::ModelRequest> {
    run_runtime_test_compaction_wave(service, "%1", Err(failure(kind)))
}

/// Constructs only a duplicate/stale delivery, never evidence of a model call.
fn failed(generation: u64, kind: &str) -> AgentCompactionEvent {
    let error = failure(kind);
    AgentCompactionEvent::Failed {
        pane_id: "%1".into(),
        task_generation: generation,
        kind: kind.into(),
        message: error.message().into(),
        usage: error.provider_incurred_usage(),
        provider_failure_json: None,
        provider_raw_text: Some("provisional summary".into()),
    }
}

/// Pause retains the released exact retry; Stop or replacement invalidates its
/// timer. New user input and actor lifecycle remain usable during backoff.
#[test]
fn compaction_transport_pause_stop_and_new_ingress() {
    for command in ["/stop", "/new", "pause"] {
        let mut service = test_runtime_service();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        let (mut service, _, _) = replay_observed::queued_with_service(service);
        let primary = service
            .attach_primary(
                "transport-controller",
                true,
                Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        fail_model(&mut service, "io");
        let retry = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        if command == "pause" {
            let target = service
                .capture_agent_lifecycle_target(&primary, "%1")
                .unwrap();
            service
                .pause_agent_lifecycle_target(&primary, &target)
                .unwrap();
            assert!(service.release_agent_compaction_retry("%1", retry));
            assert!(
                service
                    .claim_agent_compaction_task("%1", retry)
                    .unwrap()
                    .is_none()
            );
            assert!(service.agent_is_compacting("%1"));
        } else {
            if command == "/new" {
                let blocked = service
                    .execute_agent_shell_command(&primary, command)
                    .unwrap();
                assert!(blocked.contains("cannot mutate pane state"), "{blocked}");
                service
                    .execute_agent_shell_command(&primary, "/stop")
                    .unwrap();
            }
            service
                .execute_agent_shell_command(&primary, command)
                .unwrap();
            assert!(!service.release_agent_compaction_retry("%1", retry));
            assert!(!service.agent_is_compacting("%1"));
        }
        assert!(
            !service
                .apply_agent_compaction_transition(failed(generation, "io"))
                .unwrap()
                .applied
        );
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Real wire failures obey finite/unlimited transport policy. Duplicate events
/// neither add expense nor obtain another retry; nonretryable refusals settle.
#[test]
fn compaction_transport_policy_exhaustion_and_nonretryable_controls() {
    for (kind, unlimited, retries) in [
        ("io", false, 1),
        ("forbidden", true, 0),
        ("conflict", true, 0),
        ("io", true, 2),
    ] {
        let (mut service, store, turn) = replay_observed::queued();
        service.configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
            max_attempts: if unlimited { 0 } else { 1 },
            unlimited,
            initial_delay_ms: 20,
            max_delay_ms: 40,
        });
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn)
            .unwrap()
            .conversation_id
            .clone();
        let before = store.inspect(&conversation).unwrap();
        let prior: u64 = service
            .agent_token_usage_for_conversation(&conversation)
            .values()
            .map(|u| u.input_tokens)
            .sum();
        for attempt in 0..=retries {
            let generation = service
                .pending_agent_compaction_task_generation("%1")
                .unwrap();
            assert_eq!(fail_model(&mut service, kind).len(), 1);
            assert!(
                !service
                    .apply_agent_compaction_transition(failed(generation, kind))
                    .unwrap()
                    .applied
            );
            if attempt < retries || unlimited && kind == "io" {
                let next = service
                    .pending_agent_compaction_task_generation("%1")
                    .unwrap();
                let delay = service.agent_compaction_retry_delay("%1", next).unwrap();
                assert!((10..=40).contains(&delay));
                assert!(service.release_agent_compaction_retry("%1", next));
                assert!(!service.release_agent_compaction_retry("%1", next));
            } else {
                assert!(!service.agent_is_compacting("%1"));
                assert_eq!(
                    service.agent_turn_ledger().turn(&turn).unwrap().state,
                    AgentTurnState::Failed
                );
            }
        }
        assert!(store.inspect(&conversation).unwrap().starts_with(&before));
        let actual: u64 = service
            .agent_token_usage_for_conversation(&conversation)
            .values()
            .map(|u| u.input_tokens)
            .sum();
        assert_eq!(actual - prior, 17 * (retries + 1));
    }
}

/// Existing timer-admission failure and exact logical cancellation retire only
/// their own generation, even while another generation occupies the pane.
#[test]
fn compaction_transport_backoff_stop_replacement_and_admission_failure() {
    for disposition in ["stop", "replacement", "admission"] {
        let (mut service, _, turn) = replay_observed::queued();
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        fail_model(&mut service, "io");
        let retry = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        assert!(
            service
                .claim_agent_compaction_task("%1", retry)
                .unwrap()
                .is_none()
        );
        match disposition {
            "stop" => {
                service.cancel_current_agent_compaction_task("%1");
            }
            "replacement" => {
                let mut replacement = service
                    .pending_agent_compaction_task_for_tests("%1")
                    .unwrap()
                    .clone();
                service.cancel_current_agent_compaction_task("%1");
                replacement.compaction_epoch = 0;
                replacement.transport_retry_delay_ms = None;
                service.queue_agent_compaction_task(replacement);
            }
            _ => {
                assert!(
                    service
                        .fail_agent_compaction_retry_admission("%1", retry)
                        .unwrap()
                );
                assert!(
                    !service
                        .fail_agent_compaction_retry_admission("%1", retry)
                        .unwrap()
                );
                assert_eq!(
                    service.agent_turn_ledger().turn(&turn).unwrap().state,
                    AgentTurnState::Failed
                );
            }
        }
        assert!(!service.release_agent_compaction_retry("%1", retry));
        assert!(
            !service
                .apply_agent_compaction_transition(failed(generation, "io"))
                .unwrap()
                .applied
        );
        assert_eq!(
            service.agent_is_compacting("%1"),
            disposition == "replacement"
        );
    }
}

/// A successful immutable retry publishes once and admits one ordinary
/// continuation; failed request text and late duplicate events remain inert.
#[test]
fn compaction_transport_success_resumes_once() {
    let (mut service, store, turn) = replay_observed::queued();
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let failed_requests = fail_model(&mut service, "io");
    let retry = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert!(service.release_agent_compaction_retry("%1", retry));
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("complete compact summary")),
    );
    assert_eq!(
        sent, failed_requests,
        "the actual logical request is immutable"
    );
    assert!(
        !service
            .apply_agent_compaction_transition(failed(generation, "io"))
            .unwrap()
            .applied
    );
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "complete compact summary");
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .filter(|task| task.turn_id == turn)
            .count(),
        1
    );
}

/// Interrupted synthesis retains accepted private raw siblings. Its exact retry
/// uses their summaries without regenerating raw source or publishing early.
#[test]
fn compaction_transport_manual_chunk_position_is_frozen() {
    const CID: &str = "manual-transport-chunks";
    let (mut service, store) =
        manual_model::queued(CID, "original manual chunk source ".repeat(4000));
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let before = store.inspect(CID).unwrap();
    // The configured cap already requires multiple raw requests in one wave.
    let raw = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("prior complete summary")),
    );
    assert!(!raw.is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    let failed_requests = fail_model(&mut service, "io");
    assert_eq!(failed_requests.len(), 1);
    assert!(
        failed_requests[0]
            .messages
            .last()
            .unwrap()
            .content
            .contains("prior complete summary")
    );
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        retry.replay_source.as_ref().unwrap(),
        original.replay_source.as_ref().unwrap()
    ));
    assert_eq!(retry.compaction_epoch, original.compaction_epoch);
    let generation = retry.task_generation;
    assert!(service.release_agent_compaction_retry("%1", generation));
    assert_eq!(store.inspect(CID).unwrap(), before);
    assert!(
        store
            .load_replay_epoch(CID, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("accepted final synthesis")),
    );
    assert_eq!(sent, failed_requests);
    let completion = replay_publication::execute_pending_publication(&mut service);
    service.apply_replay_epoch_completion(completion).unwrap();
    let (_, cold) = store
        .load_replay_epoch(CID, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "accepted final synthesis"
    );
    assert_eq!(store.inspect(CID).unwrap(), before);
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// HTTP 200 EOF is retryable transport failure, never a completed summary.
/// The original operation and waiting turn remain alive behind its exact timer.
#[test]
fn compaction_transport_failure_retains_frozen_work() {
    let (mut service, store, turn) = replay_observed::queued();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let before = store.inspect(&task.conversation_id).unwrap();
    fail_model(&mut service, "io");
    assert!(service.agent_is_compacting("%1"));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Running
    );
    assert!(!service.agent_provider_task_is_pending(&turn));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_ne!(retry.task_generation, task.task_generation);
    assert_eq!(retry.compaction_epoch, task.compaction_epoch);
    assert!(std::sync::Arc::ptr_eq(
        retry.replay_source.as_ref().unwrap(),
        task.replay_source.as_ref().unwrap()
    ));
    assert_eq!(retry.request, task.request);
    assert_eq!(store.inspect(&task.conversation_id).unwrap(), before);
    assert!(
        store
            .load_replay_epoch(&task.conversation_id, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
}
