//! One-slot dialogue, exact native origin, coalescing and fair retry.

use super::*;

/// Default task join must allow its child to ask and wait, its parent to answer
/// before that child completes, and the same child/result to finish exactly
/// once. Every provider step uses the real synchronous runtime application path;
/// no dependency, ownership table, result slot or scheduler wait is fabricated.
/// Mail commits before the child parks, then an ordinary delivery sweep must
/// admit the parent's serialized coordination without requiring another letter.
/// The join survives the interleaved parent answer and settles only on the real
/// child completion. Terminal cleanup owns all generated process resources.
#[test]
fn runtime_joined_parent_answers_child_before_completion_with_one_slot() {
    let mut service = DialogueRuntime(test_runtime_service());
    let transcript_store =
        crate::storage::transcript::AgentTranscriptStore::new(temp_root("joined-dialogue"));
    service.set_agent_transcript_store(transcript_store.clone());
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let _primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    let parent = service
        .start_agent_prompt_turn("%1", "delegate work and answer necessary clarification")
        .unwrap();
    let spawn = dialogue_provider(vec![runtime_spawn_agent_action(
        "joined-dialogue-spawn",
        "ask the parent for format, wait for its answer, then complete",
    )]);
    let execution = service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &spawn,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    service.start_ready_agent_turns().unwrap();
    let child = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id != parent.turn_id)
        .unwrap()
        .clone();
    // The synchronous fixture performs the same exact-root startup validation
    // the host normally supplies before queued native child work may acquire.
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    service.start_ready_agent_turns().unwrap();
    let child = service
        .agent_turn_ledger()
        .turn(&child.turn_id)
        .unwrap()
        .clone();
    if child.state != AgentTurnState::Running {
        let scheduler = service.agent_scheduler().snapshot();
        let parent_state = service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state;
        service.terminate_all_pane_processes().unwrap();
        panic!(
            "fixture child not started: child={:?} parent={parent_state:?} scheduler={scheduler:?}",
            child.state
        );
    }
    assert!(service.has_joined_subagent_dependency(&child.turn_id));
    service.set_pane_approval_policy_override(
        &child.pane_id,
        Some(mez_agent::ApprovalPolicy::AutoAllow),
    );
    let question = dialogue_provider(vec![dialogue_send(
        "joined-dialogue-question",
        &parent.agent_id,
        "JOINED_DIALOGUE_QUESTION: which format?",
    )]);
    let question_execution = service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &question,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(
        question_execution
            .action_results
            .iter()
            .any(|result| result.action_id == "joined-dialogue-question"
                && result.status == ActionStatus::Succeeded)
    );
    // MAAP wait is its own executable batch, following the accepted send.
    let wait = dialogue_provider(vec![mez_agent::AgentAction {
        id: "joined-dialogue-child-wait".into(),
        payload: mez_agent::AgentActionPayload::Wait {
            for_message: None,
            from_agent: None,
        },
    }]);
    let wait_execution = service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &wait,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(
        wait_execution
            .action_results
            .iter()
            .any(|result| result.action_id == "joined-dialogue-child-wait"
                && result.status == ActionStatus::Running)
    );
    service
        .deliver_pending_runtime_agent_messages(crate::runtime::current_unix_millis())
        .unwrap();
    let question_count = service
        .agent_turn_contexts()
        .get(&parent.turn_id)
        .unwrap()
        .blocks()
        .iter()
        .filter(|block| {
            block.source == ContextSourceKind::PeerMessage
                && block.content.contains("JOINED_DIALOGUE_QUESTION")
        })
        .count();
    assert_eq!(
        question_count, 1,
        "the actual child question must already have committed"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    // Capture the current failed liveness boundary only after proving delivery,
    // released capacity, retained dependency, and successful child send/wait.
    let observed_parent = service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .unwrap()
        .state;
    let observed_capacity = service.agent_scheduler().snapshot().active_capacity_used;
    let observed_parent_task = service
        .pending_agent_provider_tasks()
        .iter()
        .any(|task| task.turn_id == parent.turn_id);
    assert!(service.has_joined_subagent_dependency(&child.turn_id));
    if observed_parent != AgentTurnState::Running || !observed_parent_task {
        service.terminate_all_pane_processes().unwrap();
        panic!(
            "parent clarification must reacquire while child waits: parent={observed_parent:?} capacity={observed_capacity} provider_queued={observed_parent_task}"
        );
    }
    assert_eq!(observed_capacity, 1);
    let answer = dialogue_provider(vec![dialogue_send(
        "joined-dialogue-answer",
        &child.agent_id,
        "JOINED_DIALOGUE_ANSWER: use plain text",
    )]);
    service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &answer,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(crate::runtime::current_unix_millis())
        .unwrap();
    assert!(service.has_joined_subagent_dependency(&child.turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked,
        "coordination returns to the same pending join after answering"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert!(
        service
            .agent_turn_contexts()
            .get(&child.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::PeerMessage
                && block.content.contains("JOINED_DIALOGUE_ANSWER"))
    );
    let child_done = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            "JOINED_DIALOGUE_CHILD_RESULT",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &child_done,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(!service.has_joined_subagent_dependency(&child.turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let original_result_label = format!("action result {}", execution.action_results[0].action_id);
    let parent_context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert_eq!(
        parent_context
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::ActionResult
                && block.label == original_result_label)
            .count(),
        1,
        "the original spawn receives one final child result across the answer batch"
    );
    assert!(
        parent_context
            .blocks()
            .iter()
            .any(|block| block.label == original_result_label
                && block.content.contains("JOINED_DIALOGUE_CHILD_RESULT"))
    );
    let parent_done = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &parent.turn_id,
            &parent.agent_id,
            "parent used the joined result",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &parent_done,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.turn_id == child.turn_id)
            .count(),
        1
    );
    assert_eq!(
        service.agent_turn_ledger().turns().len(),
        2,
        "coordination must not spawn another turn"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Two real questions coalesce into one parent admission. Answering does not
/// release either awaited task, and settling the first child cannot overwrite
/// the second child's original result or resume the parent prematurely. The
/// latest answer batch remains distinct while both original results commit once.
#[test]
fn runtime_joined_dialogue_two_children_keep_original_results_and_one_answer_batch() {
    let (mut service, parent, children, original) = dialogue_fixture(2);
    for _ in 0..2 {
        let child = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id != parent.turn_id && turn.state == AgentTurnState::Running)
            .unwrap()
            .clone();
        dialogue_question_and_wait(&mut service, &child, &parent);
    }
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .filter(|task| task.turn_id == parent.turn_id)
            .count(),
        1
    );
    let answer = dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(
            children
                .iter()
                .map(|child| {
                    dialogue_send(
                        &format!("answer-{}", child.turn_id),
                        &child.agent_id,
                        "MATRIX_ANSWER",
                    )
                })
                .collect(),
        ),
    );
    assert!(
        answer
            .action_results
            .iter()
            .all(|result| result.status == ActionStatus::Succeeded)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    for remaining in (0..2).rev() {
        let child = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id != parent.turn_id && turn.state == AgentTurnState::Running)
            .unwrap()
            .clone();
        let execution = dialogue_child_done(&mut service, &child);
        let before = service
            .agent_turn_contexts()
            .get(&parent.turn_id)
            .unwrap()
            .clone();
        service
            .emit_subagent_task_result_for_execution(&child, &execution)
            .unwrap();
        assert_eq!(
            &before,
            service.agent_turn_contexts().get(&parent.turn_id).unwrap()
        );
        assert_eq!(service.joined_subagent_dependency_count(), remaining);
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            if remaining == 0 {
                AgentTurnState::Running
            } else {
                AgentTurnState::Blocked
            }
        );
    }
    let context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    for result in &original.action_results {
        let label = format!("action result {}", result.action_id);
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(
                    |block| block.source == ContextSourceKind::ActionResult && block.label == label
                )
                .count(),
            1
        );
    }
    assert_eq!(service.agent_turn_ledger().turns().len(), 3);
}

/// An already full fair queue must retain a committed child question rather
/// than consume its wake. Once an unrelated queued sibling starts, a delivery
/// sweep reacquires the same parent after that sibling finishes. The source
/// transferred for answering retains the sibling's already completed result.
#[test]
fn runtime_joined_dialogue_queue_full_retries_committed_question_after_sibling_progress() {
    let (mut service, parent, children, original) = dialogue_fixture(2);
    service
        .agent_scheduler_mut()
        .set_queue_limits(1, 64 * 1024 * 1024)
        .unwrap();
    let first = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id != parent.turn_id && turn.state == AgentTurnState::Running)
        .unwrap()
        .clone();
    dialogue_step(
        &mut service,
        &first.turn_id,
        dialogue_provider(vec![dialogue_send(
            "queue-question",
            &parent.agent_id,
            "QUEUE_FULL_QUESTION",
        )]),
    );
    assert!(
        service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == parent.turn_id)
    );
    let now = crate::runtime::current_unix_millis();
    // Drain transport, delivery outcome and presentation work: the committed
    // coordination receipt must independently keep the next retry scheduled.
    for generation in 1..=3 {
        let transition = service
            .apply_peer_message_delivery_timer(now, generation)
            .unwrap();
        assert!(
            !transition.side_effects.is_empty(),
            "QueueFull lost its retry timer"
        );
    }
    dialogue_step(
        &mut service,
        &first.turn_id,
        dialogue_provider(vec![mez_agent::AgentAction {
            id: "queue-child-wait".into(),
            payload: mez_agent::AgentActionPayload::Wait {
                for_message: None,
                from_agent: None,
            },
        }]),
    );
    let sibling = children
        .iter()
        .find(|child| child.turn_id != first.turn_id)
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&sibling.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    service
        .deliver_pending_runtime_agent_messages(crate::runtime::current_unix_millis())
        .unwrap();
    dialogue_child_done(&mut service, sibling);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "queue-answer",
            &first.agent_id,
            "QUEUE_FULL_ANSWER",
        )]),
    );
    for _ in 0..3 {
        service
            .deliver_pending_runtime_agent_messages(crate::runtime::current_unix_millis())
            .unwrap();
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
    }
    dialogue_child_done(&mut service, &first);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    let context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    for result in &original.action_results {
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|block| block.source == ContextSourceKind::ActionResult
                    && block.label == format!("action result {}", result.action_id))
                .count(),
            1
        );
    }
}

/// A transport-accepted envelope with the actual child agent label and matching
/// correlation text still cannot impersonate that task's accepted model send.
/// It remains ordinary untrusted context; only a real child action can lift the
/// join for coordination, and duplicate receive sweeps cannot renew that wake.
#[test]
fn runtime_joined_dialogue_requires_exact_native_task_send_not_sender_labels() {
    let (mut service, parent, children, _) = dialogue_fixture(1);
    let child = &children[0];
    let now = crate::runtime::current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity(&child.agent_id, None, "agent", &[], now)
        .unwrap();
    let recipient = AgentId::opaque(parent.agent_id.clone()).unwrap();
    let envelope = mez_agent::messaging::Envelope {
        protocol: "mmp/1",
        id: "unowned-child-question".into(),
        message_type: "send".into(),
        time: format!("runtime:{now}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient),
        correlation_id: Some(child.turn_id.clone()),
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "UNOWNED_TASK_QUESTION".into(),
        extension_fields: Vec::new(),
    };
    service
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            envelope,
            mez_agent::messaging::MessageScope::Session,
            now,
        )
        .unwrap();
    service.deliver_pending_runtime_agent_messages(now).unwrap();
    assert!(
        service
            .agent_turn_contexts()
            .get(&parent.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::PeerMessage
                && block.content.contains("UNOWNED_TASK_QUESTION"))
    );
    assert!(
        service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == parent.turn_id)
    );
    assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
    dialogue_question_and_wait(&mut service, child, &parent);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
}

/// Separate questions from the same actual child require separate serialized
/// parent responses, while reusing one original spawn and retained result slot.
/// A consumed receipt cannot keep retrying or renew an answer after transport
/// settles. The final real child result still becomes one canonical action row.
#[test]
fn runtime_joined_dialogue_repeated_questions_reuse_one_original_task() {
    let (mut service, parent, children, original) = dialogue_fixture(1);
    let child = &children[0];
    for round in 1..=2 {
        dialogue_step(
            &mut service,
            &child.turn_id,
            dialogue_provider(vec![dialogue_send(
                &format!("round-question-{round}"),
                &parent.agent_id,
                &format!("ROUND_QUESTION_{round}"),
            )]),
        );
        dialogue_step(
            &mut service,
            &child.turn_id,
            dialogue_provider(vec![mez_agent::AgentAction {
                id: format!("round-wait-{round}"),
                payload: mez_agent::AgentActionPayload::Wait {
                    for_message: None,
                    from_agent: None,
                },
            }]),
        );
        assert!(service.agent_provider_task_is_pending(&parent.turn_id));
        dialogue_step(
            &mut service,
            &parent.turn_id,
            dialogue_provider(vec![dialogue_send(
                &format!("round-answer-{round}"),
                &child.agent_id,
                &format!("ROUND_ANSWER_{round}"),
            )]),
        );
        assert_eq!(service.joined_subagent_dependency_count(), 1);
        assert_eq!(
            service
                .retained_joined_sources_for_tests(&parent.turn_id)
                .len(),
            1
        );
        for tick in 1..=3 {
            let transition = service
                .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick)
                .unwrap();
            assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
            if tick == 3 {
                assert!(
                    transition.side_effects.is_empty(),
                    "consumed mail renewed timer"
                );
            }
        }
    }
    dialogue_child_done(&mut service, child);
    assert!(
        service
            .retained_joined_sources_for_tests(&parent.turn_id)
            .is_empty()
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 2);
    let context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::ActionResult
                && block.label == format!("action result {}", original.action_results[0].action_id))
            .count(),
        1
    );
}
