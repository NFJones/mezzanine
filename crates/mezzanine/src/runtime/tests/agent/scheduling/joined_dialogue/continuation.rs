//! Normal permissioned harness steps before a joined clarification is answered.
//!
//! Tool output and an admitted peer Wait must reach the next model request.
//! Consuming the question at first inference is not completion of its answer.
//! New joined helpers retain their ordinary result gate independently of the
//! original question child and of nested clarification responses.

use super::*;

/// Completes the actual root only after its original child result is available.
/// No controller fact or final action result is injected into the live state.
fn finish_dialogue_parent(
    service: &mut RuntimeSessionService,
    parent: &mez_agent::AgentTurnRecord,
) {
    dialogue_step(
        service,
        &parent.turn_id,
        RuntimeBatchProvider {
            response: runtime_say_response_for_agent(
                &parent.turn_id,
                &parent.agent_id,
                "used the real joined result after normal coordination",
                true,
            ),
        },
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
}

/// Spawns a real task through its parent's admitted response and validates its
/// native root before fair scheduling. This helper is shared by the ordinary
/// helper-result, nested-question and recursive-parent Wait owners below.
fn spawn_joined_dialogue_child(
    service: &mut RuntimeSessionService,
    parent: &mez_agent::AgentTurnRecord,
    action_id: &str,
) -> mez_agent::AgentTurnRecord {
    let execution = dialogue_step(
        service,
        &parent.turn_id,
        dialogue_provider(vec![runtime_spawn_agent_action(
            action_id,
            "obtain the supporting answer",
        )]),
    );
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let child = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| {
            service
                .joined_subagent_dependency(&turn.turn_id)
                .is_some_and(|dependency| {
                    dependency.parent_turn_id == parent.turn_id
                        && dependency.parent_action_id == action_id
                })
        })
        .unwrap()
        .clone();
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    service.set_pane_approval_policy_override(
        &child.pane_id,
        Some(mez_agent::ApprovalPolicy::AutoAllow),
    );
    service.start_ready_agent_turns().unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    child
}

/// Read-only discovery may be necessary before answering. Its successful result
/// must admit the next ordinary model step while the same child awaits its
/// answer, then one accepted reply must return to that same join exactly once.
#[test]
fn runtime_joined_dialogue_discovery_result_precedes_answer() {
    let (mut service, parent, children, _) = dialogue_fixture(1);
    let child = &children[0];
    dialogue_question_and_wait(&mut service, child, &parent);
    let discovery = dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![mez_agent::AgentAction {
            id: "coordination-discovery".into(),
            payload: mez_agent::AgentActionPayload::ListAgents {
                agent_type: Some("all".into()),
                scope: Some("session".into()),
            },
        }]),
    );
    assert_eq!(discovery.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        discovery.action_results[0]
            .structured_content_json
            .as_ref()
            .unwrap()
            .contains(&child.agent_id)
    );
    assert!(
        service.agent_provider_task_is_pending(&parent.turn_id),
        "the parent must consume discovery before answering its still-pending child"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert_eq!(service.joined_subagent_dependency_count(), 1);
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "discovered-answer",
            &child.agent_id,
            "DISCOVERED_ANSWER",
        )]),
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
    dialogue_child_done(&mut service, child);
    finish_dialogue_parent(&mut service, &parent);
    assert_eq!(service.agent_turn_ledger().turns().len(), 2);
}

/// A middle agent must ask its own parent and then Wait while retaining the
/// question from its own joined child. The upstream reply is genuine native
/// mail but is not from that awaited child, so it must settle the admitted Wait
/// without manufacturing another child-origin admission. One slot drives all
/// eleven responses and all three original task identities to completion.
#[test]
fn runtime_joined_dialogue_recursive_parent_reply_resumes_admitted_wait() {
    let (mut service, root, children, _) = dialogue_fixture(1);
    let middle = &children[0];
    let leaf = spawn_joined_dialogue_child(&mut service, middle, "recursive-leaf-spawn");
    assert_eq!(service.joined_subagent_dependency_count(), 2);
    dialogue_question_and_wait(&mut service, &leaf, middle);
    dialogue_step(
        &mut service,
        &middle.turn_id,
        dialogue_provider(vec![dialogue_send(
            "recursive-upstream-question",
            &root.agent_id,
            "ROOT_INFORMATION_NEEDED",
        )]),
    );
    assert!(
        service.agent_provider_task_is_pending(&middle.turn_id),
        "the middle agent must be able to emit its separately required Wait"
    );
    dialogue_step(
        &mut service,
        &middle.turn_id,
        dialogue_provider(vec![mez_agent::AgentAction {
            id: "recursive-parent-wait".into(),
            payload: mez_agent::AgentActionPayload::Wait {
                for_message: None,
                from_agent: None,
            },
        }]),
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&root.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    dialogue_step(
        &mut service,
        &root.turn_id,
        dialogue_provider(vec![dialogue_send(
            "recursive-upstream-answer",
            &middle.agent_id,
            "ROOT_INFORMATION",
        )]),
    );
    assert!(
        service.agent_provider_task_is_pending(&middle.turn_id),
        "the existing peer Wait must preserve the middle agent's active coordination: {}",
        service.joined_coordination_state_for_tests(&middle.turn_id)
    );
    assert!(
        service
            .agent_turn_contexts()
            .get(&middle.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::PeerMessage
                && block.content.contains("ROOT_INFORMATION"))
    );
    dialogue_step(
        &mut service,
        &middle.turn_id,
        dialogue_provider(vec![dialogue_send(
            "recursive-leaf-answer",
            &leaf.agent_id,
            "LEAF_INFORMATION_FROM_ROOT",
        )]),
    );
    assert_eq!(service.joined_subagent_dependency_count(), 2);
    assert!(!service.agent_provider_task_is_pending(&middle.turn_id));
    dialogue_child_done(&mut service, &leaf);
    assert!(service.agent_provider_task_is_pending(&middle.turn_id));
    dialogue_child_done(&mut service, middle);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    finish_dialogue_parent(&mut service, &root);
    assert_eq!(service.agent_turn_ledger().turns().len(), 3);
}

/// A newly joined helper may itself ask for clarification. Answering it must
/// return to that helper's pending result before the original question's next
/// inference. Both original groups keep distinct result slots and source pins;
/// no latest-batch success may substitute for the helper's actual completion.
#[test]
fn runtime_joined_dialogue_nested_helper_question_keeps_new_join_gate() {
    let (mut service, parent, children, _) = dialogue_fixture(1);
    let child = &children[0];
    dialogue_question_and_wait(&mut service, child, &parent);
    let helper = spawn_joined_dialogue_child(&mut service, &parent, "nested-helper-spawn");
    dialogue_question_and_wait(&mut service, &helper, &parent);
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "nested-helper-answer",
            &helper.agent_id,
            "HELPER_CLARIFICATION",
        )]),
    );
    assert_eq!(
        service
            .retained_joined_sources_for_tests(&parent.turn_id)
            .len(),
        2
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert!(
        !service.agent_provider_task_is_pending(&parent.turn_id),
        "the new helper still owes its real result after its clarification"
    );
    dialogue_child_done(&mut service, &helper);
    assert_eq!(
        service
            .retained_joined_sources_for_tests(&parent.turn_id)
            .len(),
        1
    );
    assert!(
        service.agent_provider_task_is_pending(&parent.turn_id),
        "the original question remains active after the helper's real result"
    );
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "nested-original-answer",
            &child.agent_id,
            "ANSWER_FROM_NESTED_HELPER",
        )]),
    );
    dialogue_child_done(&mut service, child);
    finish_dialogue_parent(&mut service, &parent);
    assert!(
        service
            .retained_joined_sources_for_tests(&parent.turn_id)
            .is_empty()
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 3);
}

/// An explicit final Say may end the parent's coordination attempt, but it
/// cannot complete an unanswered child's task or generate repeated inference.
/// Bounded delivery sweeps retain the same original join and release the one
/// scheduler slot; fixture teardown owns the intentionally unfinished dialogue.
#[test]
fn runtime_joined_dialogue_final_say_parks_without_completing_unanswered_child() {
    let (mut service, parent, children, _) = dialogue_fixture(1);
    let child = &children[0];
    dialogue_question_and_wait(&mut service, child, &parent);
    let execution = dialogue_step(
        &mut service,
        &parent.turn_id,
        RuntimeBatchProvider {
            response: runtime_say_response_for_agent(
                &parent.turn_id,
                &parent.agent_id,
                "end this coordination attempt while awaiting the actual task",
                true,
            ),
        },
    );
    assert!(!execution.final_turn);
    for tick in 1..=3 {
        service
            .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), tick)
            .unwrap();
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&child.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert!(service.has_joined_subagent_dependency(&child.turn_id));
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert_eq!(
            service
                .retained_joined_sources_for_tests(&parent.turn_id)
                .len(),
            1
        );
        assert_eq!(service.agent_turn_ledger().turns().len(), 2);
    }
}
