//! Auxiliary transport retry regression tests. Source, epoch and waiting-turn
//! ownership must survive interrupted responses without partial publication.

use super::*;
use crate::runtime::AgentCompactionEvent;

/// Real lifecycle commands invalidate backoff/inflight generations. Human
/// pause retains a released retry as pending without allowing provider claims.
#[test]
fn compaction_transport_pause_stop_and_new_ingress() {
    for command in ["/stop", "/new", "pause"] {
        let (mut service, _, _) = queue_observed_input_compaction_with_second_group(None);
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        let primary = service
            .attach_primary(
                "transport-controller",
                true,
                Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        let generation = task.task_generation;
        service.claim_agent_compaction_task_state("%1", task);
        service
            .apply_agent_compaction_transition(failed(generation, "io"))
            .unwrap();
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
            let result = service
                .execute_agent_shell_command(&primary, command)
                .unwrap();
            assert!(
                !service.release_agent_compaction_retry("%1", retry),
                "{command}: {result}"
            );
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

/// Constructs a failed issued auxiliary event, including known incurred expense.
fn failed(generation: u64, kind: &str) -> AgentCompactionEvent {
    AgentCompactionEvent::Failed {
        pane_id: "%1".into(),
        task_generation: generation,
        kind: kind.into(),
        message: "provider HTTP response read failed: unexpected EOF during chunk size line".into(),
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 17,
            ..Default::default()
        },
        provider_failure_json: None,
        provider_raw_text: Some("provisional summary".into()),
    }
}

/// Configured finite exhaustion and genuinely nonretryable failures settle the
/// waiting turn, while unlimited transport policy overrides a zero finite limit.
#[test]
fn compaction_transport_policy_exhaustion_and_nonretryable_controls() {
    for (kind, unlimited, retries) in [
        ("io", false, 1),
        ("forbidden", true, 0),
        ("conflict", true, 0),
        ("io", true, 2),
    ] {
        let (mut service, store, turn) = queue_observed_input_compaction_with_second_group(None);
        service.configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
            max_attempts: if unlimited { 0 } else { 1 },
            unlimited,
            initial_delay_ms: 20,
            max_delay_ms: 40,
        });
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let before = store.inspect(&conversation).unwrap();
        for attempt in 0..=retries {
            let task = service.take_pending_agent_compaction_task("%1").unwrap();
            let generation = task.task_generation;
            service.claim_agent_compaction_task_state("%1", task);
            let event = failed(generation, kind);
            let transition = service
                .apply_agent_compaction_transition(event.clone())
                .unwrap();
            assert!(transition.applied);
            assert!(
                !service
                    .apply_agent_compaction_transition(event)
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
        assert_eq!(
            &store.inspect(&conversation).unwrap()[..before.len()],
            before.as_slice()
        );
        let usage = service.agent_token_usage_for_conversation(&conversation);
        assert_eq!(
            usage.values().map(|u| u.input_tokens).sum::<u64>(),
            17 * (retries + 1)
        );
    }
}

/// Stop, conversation replacement and failed timer admission invalidate the
/// held generation. No late timer can redispatch it or affect a replacement.
#[test]
fn compaction_transport_backoff_stop_replacement_and_admission_failure() {
    for disposition in ["stop", "replacement", "admission"] {
        let (mut service, _, turn) = queue_observed_input_compaction_with_second_group(None);
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        let generation = task.task_generation;
        service.claim_agent_compaction_task_state("%1", task);
        service
            .apply_agent_compaction_transition(failed(generation, "io"))
            .unwrap();
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

/// Successful complete summaries after backoff resume one ordinary turn. Old
/// failure/completion deliveries neither republish an epoch nor enqueue work.
#[test]
fn compaction_transport_success_resumes_once() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_second_group(None);
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    let generation = task.task_generation;
    let conversation = task.conversation_id.clone();
    let before = store.inspect(&conversation).unwrap();
    service.claim_agent_compaction_task_state("%1", task);
    service
        .apply_agent_compaction_transition(failed(generation, "io"))
        .unwrap();
    let retry = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert!(service.release_agent_compaction_retry("%1", retry));
    for _ in 0..8 {
        let Some(task) = service.take_pending_agent_compaction_task("%1") else {
            break;
        };
        let generation = task.task_generation;
        service.claim_agent_compaction_task_state("%1", task);
        let completed = AgentCompactionEvent::Completed {
            pane_id: "%1".into(),
            task_generation: generation,
            response: Box::new(runtime_test_compaction_response("complete compact summary")),
        };
        assert!(
            service
                .apply_agent_compaction_transition(completed.clone())
                .unwrap()
                .applied
        );
        assert!(
            !service
                .apply_agent_compaction_transition(completed)
                .unwrap()
                .applied
        );
    }
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.agent_provider_task_is_pending(&turn));
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .filter(|t| t.turn_id == turn)
            .count(),
        1
    );
    assert_eq!(
        &store.inspect(&conversation).unwrap()[..before.len()],
        before.as_slice()
    );
    assert!(
        !service
            .apply_agent_compaction_transition(failed(generation, "io"))
            .unwrap()
            .applied
    );
}

/// Manual chunk and synthesis state is private provisional work. Retrying an
/// interrupted request must preserve its completed siblings and pending source.
#[test]
fn compaction_transport_manual_chunk_position_is_frozen() {
    let (mut service, _, _) = queue_observed_input_compaction_with_second_group(None);
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    task.target = crate::runtime::agent_state::RuntimeAgentCompactionTarget::Conversation;
    task.resume_turn_id = None;
    task.conversation_chunks = Some(
        crate::runtime::agent_state::RuntimeConversationCompactionChunks {
            current: "current source".into(),
            pending: vec!["pending source".into()],
            summaries: vec!["prior complete summary".into()],
            synthesis_source_bytes: Some(123),
            failures: 2,
            completed: 1,
        },
    );
    let generation = task.task_generation;
    service.claim_agent_compaction_task_state("%1", task);
    service
        .apply_agent_compaction_transition(failed(generation, "io"))
        .unwrap();
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let chunks = retry.conversation_chunks.as_ref().unwrap();
    assert_eq!(chunks.current, "current source");
    assert_eq!(chunks.pending, ["pending source"]);
    assert_eq!(chunks.summaries, ["prior complete summary"]);
    assert_eq!(
        (
            chunks.failures,
            chunks.completed,
            chunks.synthesis_source_bytes
        ),
        (2, 1, Some(123))
    );
}

/// An interrupted HTTP 200 body is not a complete summary. Keep the frozen
/// observed-input task and its ordinary turn alive behind a delayed retry,
/// without changing authoritative history or publishing provisional text.
#[test]
fn compaction_transport_failure_retains_frozen_work() {
    let (mut service, store, turn_id) = queue_observed_input_compaction_with_second_group(None);
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    let frozen = task.request.clone();
    let generation = task.task_generation;
    let epoch = task.compaction_epoch;
    let conversation = task.conversation_id.clone();
    let before = store.inspect(&conversation).unwrap();
    service.claim_agent_compaction_task_state("%1", task);
    let transition = service.apply_agent_compaction_transition(AgentCompactionEvent::Failed {
        pane_id: "%1".into(), task_generation: generation, kind: "io".into(),
        message: "provider HTTP response read failed (status 200, content-encoding absent, timeout false, decode true, source error reading a body from connection -> unexpected EOF during chunk size line): error decoding response body".into(),
        usage: Default::default(), provider_failure_json: None, provider_raw_text: Some("incomplete summary".into()),
    }).unwrap();
    assert!(transition.applied);
    assert!(service.agent_is_compacting("%1"));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn_id).unwrap().state,
        AgentTurnState::Running
    );
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_ne!(retry.task_generation, generation);
    assert_eq!(retry.compaction_epoch, epoch);
    assert_eq!(retry.request, frozen);
    assert_eq!(store.inspect(&conversation).unwrap(), before);
    assert!(
        transition
            .side_effects
            .iter()
            .any(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { .. }))
    );
}
