//! Real native sibling settlement and completed-answer capacity release.

use super::*;

/// A child's committed question cannot overtake an issued native shell sibling.
/// Its acknowledged receipt keeps retry scheduled with an empty transport. Once
/// the real dispatcher completes that shell exactly once, the timer admits the
/// parent without a second question; answering preserves both original slots.
#[test]
fn runtime_joined_dialogue_native_sibling_settles_before_question_admission() {
    let shell = mez_agent::AgentAction {
        id: "dialogue-native-sibling".into(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect alongside delegated work".into(),
            command: "printf DIALOGUE_NATIVE_RESULT".into(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let (mut service, parent, children, original, _) =
        dialogue_fixture_with_actions(1, vec![shell]);
    let child = &children[0];
    dialogue_question_and_wait(&mut service, child, &parent);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
    for generation in 1..=3 {
        let transition = service
            .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), generation)
            .unwrap();
        assert!(!transition.side_effects.is_empty());
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
    }
    let mut commands = 0;
    for _ in 0..4 {
        let dispatch = service
            .claim_native_shell_action(&parent.turn_id, "dialogue-native-sibling")
            .unwrap()
            .expect("original native phase remains owned");
        commands += usize::from(!dispatch.capability_probe_only);
        let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
        assert!(service.complete_native_shell_action(outcome).unwrap());
        if service
            .agent_turn_executions()
            .get(&parent.turn_id)
            .unwrap()
            .action_results
            .iter()
            .find(|result| result.action_id == "dialogue-native-sibling")
            .unwrap()
            .is_terminal()
        {
            break;
        }
    }
    assert_eq!(
        commands, 1,
        "capability discovery must not replay the actual command"
    );
    assert_eq!(
        service
            .agent_turn_executions()
            .get(&parent.turn_id)
            .unwrap()
            .action_results
            .iter()
            .find(|result| result.action_id == "dialogue-native-sibling")
            .unwrap()
            .status,
        ActionStatus::Succeeded
    );
    service
        .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), 4)
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running,
        "offer={} gates={} execution={:?}",
        service.joined_coordination_offer_ready(&parent.turn_id),
        service.joined_source_gates_for_tests(&parent.turn_id),
        service.agent_turn_executions().get(&parent.turn_id)
    );
    assert!(service.agent_provider_task_is_pending(&parent.turn_id));
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![dialogue_send(
            "native-answer",
            &child.agent_id,
            "NATIVE_ANSWER",
        )]),
    );
    assert!(
        service
            .claim_native_shell_action(&parent.turn_id, "dialogue-native-sibling")
            .unwrap()
            .is_none()
    );
    dialogue_child_done(&mut service, child);
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
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::ActionResult
                && block.content.contains("DIALOGUE_NATIVE_RESULT"))
    );
}

/// A coordination answer may issue native work under normal permissions. Its
/// real completion must repark the parent and release the only slot immediately
/// to the child, without a new peer letter, a timer sweep or another model call.
/// The original child await remains live until that child actually completes.
#[test]
fn runtime_joined_dialogue_coordination_native_completion_releases_capacity() {
    let (mut service, parent, children, _) = dialogue_fixture(1);
    let child = &children[0];
    dialogue_question_and_wait(&mut service, child, &parent);
    dialogue_step(
        &mut service,
        &parent.turn_id,
        dialogue_provider(vec![
            dialogue_send(
                "native-coordination-answer",
                &child.agent_id,
                "NATIVE_COORDINATION_ANSWER",
            ),
            mez_agent::AgentAction {
                id: "native-coordination-work".into(),
                payload: mez_agent::AgentActionPayload::ShellCommand {
                    summary: "Inspect supporting evidence before returning to await".into(),
                    command: "printf COORDINATION_NATIVE_BODY".into(),
                    interactive: false,
                    stateful: false,
                    timeout_ms: None,
                },
            },
        ]),
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let mut commands = 0;
    for _ in 0..4 {
        let dispatch = service
            .claim_native_shell_action(&parent.turn_id, "native-coordination-work")
            .unwrap()
            .expect("real admitted native coordination phase");
        commands += usize::from(!dispatch.capability_probe_only);
        let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
        assert!(service.complete_native_shell_action(outcome).unwrap());
        if service
            .agent_turn_executions()
            .get(&parent.turn_id)
            .unwrap()
            .action_results
            .iter()
            .find(|result| result.action_id == "native-coordination-work")
            .unwrap()
            .is_terminal()
        {
            break;
        }
    }
    assert_eq!(commands, 1);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked,
        "settled answer work must release capacity without a receive sweep"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
    assert_eq!(service.joined_subagent_dependency_count(), 1);
    dialogue_child_done(&mut service, child);
    assert!(service.agent_provider_task_is_pending(&parent.turn_id));
}
