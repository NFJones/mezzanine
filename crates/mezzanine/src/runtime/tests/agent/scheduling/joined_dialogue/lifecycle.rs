//! Primary pause, approval, cancellation, replacement and archive custody.

use super::*;

/// Explicit human inhibition survives a committed child question and empty
/// transport. Resume permits that same receipt to reacquire fairly; pausing and
/// resuming again after answering cannot replay the answer or finish the join.
#[test]
fn runtime_joined_dialogue_human_pause_preserves_question_and_same_await() {
    let (mut service, parent, children, _, primary) = dialogue_fixture_with_actions(1, Vec::new());
    let child = &children[0];
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    let generation = service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    dialogue_question_and_wait(&mut service, child, &parent);
    for tick in 1..=3 {
        let transition = service
            .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick)
            .unwrap();
        assert!(!transition.side_effects.is_empty());
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
    }
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    assert!(
        service
            .resume_agent_lifecycle_target(&primary, &target, generation)
            .unwrap()
    );
    service
        .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), 4)
        .unwrap();
    assert!(service.agent_provider_task_is_pending(&parent.turn_id));
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "paused-answer",
            &child.agent_id,
            "PAUSED_ANSWER",
        )]),
    );
    assert_eq!(service.joined_subagent_dependency_count(), 1);
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    let generation = service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    assert!(
        service
            .resume_agent_lifecycle_target(&primary, &target, generation)
            .unwrap()
    );
    assert!(
        service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == parent.turn_id)
    );
    assert!(
        !service
            .agent_scheduler()
            .queued_turns()
            .any(|work| work.turn_id == parent.turn_id)
    );
    for tick in 5..=7 {
        service
            .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick)
            .unwrap();
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
    }
    dialogue_child_done(&mut service, child);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert!(service.agent_provider_task_is_pending(&parent.turn_id));
}

/// An actual answer planned under Ask must retain its real approval owner.
/// Repeated delivery cannot dispatch it or request another model. Exact primary
/// approval sends it once, then returns the parent to its original child await.
#[test]
fn runtime_joined_dialogue_answer_approval_does_not_release_join() {
    let (mut service, parent, children, _, primary) = dialogue_fixture_with_actions(1, Vec::new());
    let child = &children[0];
    dialogue_question_and_wait(&mut service, child, &parent);
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::Ask));
    let answer = dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "approved-answer",
            &child.agent_id,
            "APPROVED_ANSWER",
        )]),
    );
    assert_eq!(answer.action_results[0].status, ActionStatus::Blocked);
    let approval = service
        .blocked_approvals()
        .pending()
        .into_iter()
        .find(|approval| {
            approval.pane_id == parent.pane_id && approval.action_kind == "send_message"
        })
        .unwrap()
        .clone();
    for tick in 1..=3 {
        service
            .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick)
            .unwrap();
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert_eq!(service.joined_subagent_dependency_count(), 1);
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&child.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
    }
    service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
    let recipient = AgentId::opaque(child.agent_id.clone()).unwrap();
    assert_eq!(
        service
            .message_service()
            .receive_for(&recipient, u64::MAX)
            .iter()
            .filter(|mail| mail.payload == "APPROVED_ANSWER")
            .count(),
        1
    );
    dialogue_child_done(&mut service, child);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert!(service.agent_provider_task_is_pending(&parent.turn_id));
}

/// Real Stop, new conversation and forced pane close must retire independently
/// retained source before dropping the original parent context. Late delivery
/// sweeps cannot revive that parent or recreate its child; accepted spawns stay
/// pending evidence rather than being relabeled successful on cancellation.
#[test]
fn runtime_joined_dialogue_terminal_controls_retire_original_source() {
    for control in ["stop", "new", "close"] {
        let (mut service, parent, children, original, primary) =
            dialogue_fixture_with_actions(1, Vec::new());
        let child = &children[0];
        dialogue_question_and_wait(&mut service, child, &parent);
        dialogue_step(
            &mut service,
            &parent.turn_id,
            dialogue_provider(vec![dialogue_send(
                "control-answer",
                &child.agent_id,
                "CONTROL_ANSWER",
            )]),
        );
        let source = service.retained_joined_sources_for_tests(&parent.turn_id);
        assert_eq!(source.len(), 1);
        assert_eq!(source[0].0.action_results, original.action_results);
        let store = service.persistence.cloned_transcript_store().unwrap();
        let path = store.transcript_path(&parent.conversation_id).unwrap();
        match control {
            "stop" => {
                service
                    .execute_agent_shell_command(&primary, "/stop")
                    .unwrap();
            }
            "new" => {
                let rejected = service
                    .execute_agent_shell_command(&primary, "/new")
                    .unwrap();
                assert!(
                    rejected.contains("/new cannot run while an agent turn is active"),
                    "{rejected}"
                );
                assert_eq!(
                    service
                        .retained_joined_sources_for_tests(&parent.turn_id)
                        .len(),
                    1
                );
                service
                    .execute_agent_shell_command(&primary, "/stop")
                    .unwrap();
                let replaced = service
                    .execute_agent_shell_command(&primary, "/new")
                    .unwrap();
                assert!(replaced.contains("new=true"), "{replaced}");
                assert_ne!(
                    service.agent_shell_store().get("%1").unwrap().session_id,
                    parent.conversation_id
                );
            }
            "close" => {
                service
                    .dispatch_runtime_pane_close(
                        &primary,
                        r#"{"target":{"pane_id":"%1"},"force":true}"#,
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            service
                .retained_joined_sources_for_tests(&parent.turn_id)
                .is_empty(),
            "{control}"
        );
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert_ne!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Completed
        );
        let count = service.agent_turn_ledger().turns().len();
        for tick in 1..=3 {
            service
                .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick)
                .unwrap();
            assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        }
        assert_eq!(service.agent_turn_ledger().turns().len(), count);
        assert!(path.exists());
        let archive = store
            .conversation_transcript_view(
                &parent.conversation_id,
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
                &[],
            )
            .unwrap()
            .committed
            .into_iter()
            .map(|entry| entry.content)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            archive.contains("matrix-spawn-0"),
            "{control} lost original accepted spawn"
        );
        assert!(
            archive.contains("control-answer") && archive.contains("MATRIX_QUESTION"),
            "{control} lost original chronology"
        );
    }
}

/// Checked original archive handoff failure during real Stop retains the old
/// execution and context independently of the latest answer batch. The existing
/// critical admission fence prevents accumulating new work or acknowledging the
/// unfinished child; retry sweeps neither replay effects nor discard evidence.
#[test]
fn runtime_joined_dialogue_failed_cleanup_keeps_original_behind_admission_fence() {
    let (mut service, parent, children, original, primary) =
        dialogue_fixture_with_actions(1, Vec::new());
    dialogue_question_and_wait(&mut service, &children[0], &parent);
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "failure-answer",
            &children[0].agent_id,
            "FAILURE_ANSWER",
        )]),
    );
    service
        .persistence
        .set_bookkeeping_generation_for_tests(u64::MAX);
    let response = service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(response.contains("generation exhausted"), "{response}");
    assert!(
        service
            .ensure_completion_admission_open()
            .unwrap_err()
            .message()
            .contains("generation exhausted")
    );
    let sources = service.retained_joined_sources_for_tests(&parent.turn_id);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].0.action_results, original.action_results);
    assert!(
        sources[0].1,
        "failed original cleanup must retain its own context"
    );
    assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
    let count = service.agent_turn_ledger().turns().len();
    for tick in 1..=3 {
        let _ =
            service.apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick);
        assert_eq!(
            service
                .retained_joined_sources_for_tests(&parent.turn_id)
                .len(),
            1
        );
    }
    assert!(
        service
            .start_agent_prompt_turn("%1", "cannot bypass failed retention")
            .is_err()
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), count);
}

/// A genuine interrupted child is redirected through the supported prompt path.
/// Its earlier committed question belongs to the old task attempt and cannot
/// wake the parent for the replacement. Only a fresh accepted send from that
/// replacement admits coordination; the original spawn still settles once.
#[test]
fn runtime_joined_dialogue_redirection_drops_old_attempt_question() {
    let (mut service, parent, children, original, _) = dialogue_fixture_with_actions(1, Vec::new());
    let old = &children[0];
    dialogue_step(
        &mut service,
        &old.turn_id,
        dialogue_provider(vec![dialogue_send(
            "old-attempt-question",
            &parent.agent_id,
            "OLD_ATTEMPT_QUESTION",
        )]),
    );
    service.stop_agent_turn_for_pane(&old.pane_id).unwrap();
    assert!(service.interrupted_subagent_awaits_redirection(&old.turn_id));
    let started = service
        .start_agent_prompt_turn(
            &old.pane_id,
            "continue the same delegated task with new guidance",
        )
        .unwrap();
    let child = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    assert_eq!(
        service.joined_coordination_receipt_count_for_tests(&parent.turn_id),
        0
    );
    assert_ne!(old.turn_id, child.turn_id);
    assert_eq!(old.agent_id, child.agent_id);
    assert!(service.joined_subagent_dependency(&old.turn_id).is_none());
    assert_eq!(
        service
            .joined_subagent_dependency(&child.turn_id)
            .unwrap()
            .parent_action_id,
        original.action_results[0].action_id
    );
    for tick in 1..=3 {
        service
            .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick)
            .unwrap();
        assert!(
            !service.agent_provider_task_is_pending(&parent.turn_id),
            "old attempt admitted replacement dialogue"
        );
    }
    dialogue_question_and_wait(&mut service, &child, &parent);
    assert!(service.agent_provider_task_is_pending(&parent.turn_id));
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "redirected-answer",
            &child.agent_id,
            "REDIRECTED_ANSWER",
        )]),
    );
    dialogue_child_done(&mut service, &child);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert_eq!(service.agent_turn_ledger().turns().len(), 3);
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

/// Pausing an unissued approved-answer candidate and explicitly resuming must
/// let the parent reevaluate that same question. Resume cancels the old pending
/// approval continuation, not its audit record or the child await. Late approval
/// cannot dispatch the old answer. The actor regression drives the fresh lease
/// and answer through actual completion; this control checks the resume owner.
#[test]
fn runtime_joined_dialogue_resume_queues_unissued_answer_without_losing_question() {
    let (mut service, parent, children, _, primary) = dialogue_fixture_with_actions(1, Vec::new());
    let child = &children[0];
    dialogue_question_and_wait(&mut service, child, &parent);
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::Ask));
    let old = dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "superseded-answer",
            &child.agent_id,
            "DO_NOT_SEND_OLD_ANSWER",
        )]),
    );
    assert_eq!(old.action_results[0].status, ActionStatus::Blocked);
    let approval = service
        .blocked_approvals()
        .pending()
        .into_iter()
        .find(|approval| approval.pane_id == parent.pane_id)
        .unwrap()
        .clone();
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    let generation = service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    assert!(
        service
            .resume_agent_lifecycle_target(&primary, &target, generation)
            .unwrap()
    );
    assert_eq!(service.joined_subagent_dependency_count(), 1);
    assert!(
        service.agent_provider_task_is_pending(&parent.turn_id),
        "explicit resume lost the question after superseding the unissued answer"
    );
    assert!(!service.agent_turn_has_blocked_approval(&parent.turn_id));
    service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
            .unwrap(),
        None
    );
    let recipient = AgentId::opaque(child.agent_id.clone()).unwrap();
    assert!(
        !service
            .message_service()
            .receive_for(&recipient, u64::MAX)
            .iter()
            .any(|message| message.payload == "DO_NOT_SEND_OLD_ANSWER")
    );
    assert!(service.agent_provider_task_is_pending(&parent.turn_id));
    assert_eq!(service.joined_subagent_dependency_count(), 1);
}
