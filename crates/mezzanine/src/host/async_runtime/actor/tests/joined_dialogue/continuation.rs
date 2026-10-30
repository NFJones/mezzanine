//! Ordinary permissioned work between a joined question and its actual answer.
//!
//! Real actor leases transfer each successful completion group before the next
//! inference. Native callbacks and new helper joins retain their normal owners;
//! no synchronous provider shortcut acknowledges that handoff for the fixture.

use super::*;

/// Executes one genuinely admitted response and accounts for its unique lease.
async fn step(
    actor: &mut DialogueActor,
    turn: &mez_agent::AgentTurnRecord,
    actions: Vec<AgentAction>,
    generations: &mut std::collections::BTreeSet<u64>,
) -> crate::runtime::RuntimeAgentProviderDispatch {
    let dispatch = claim(actor, turn).await;
    assert!(generations.insert(dispatch.claim_generation));
    let execution = execute(&dispatch, &response(actions)).await;
    assert_eq!(complete(actor, &dispatch, execution).await, 1);
    dispatch
}

/// Sends a real child question and parks it with a separate valid Wait batch.
async fn question(
    actor: &mut DialogueActor,
    parent: &mez_agent::AgentTurnRecord,
    child: &mez_agent::AgentTurnRecord,
    generations: &mut std::collections::BTreeSet<u64>,
) {
    step(
        actor,
        child,
        vec![send("needed-question", &parent.agent_id, "NEEDED_QUESTION")],
        generations,
    )
    .await;
    step(
        actor,
        child,
        vec![AgentAction {
            id: "needed-wait".into(),
            payload: AgentActionPayload::Wait {
                for_message: None,
                from_agent: None,
            },
        }],
        generations,
    )
    .await;
}

/// Uses the child's genuine final result before completing the original parent.
async fn finish(
    actor: &mut DialogueActor,
    parent: &mez_agent::AgentTurnRecord,
    child: &mez_agent::AgentTurnRecord,
    generations: &mut std::collections::BTreeSet<u64>,
) {
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked,
        "parent screen: {:?}; ownership: {}",
        actor
            .service
            .agent_pane_screen(&parent.pane_id)
            .map(|screen| screen
                .normal_styled_content_lines()
                .into_iter()
                .map(|line| line.text)
                .collect::<Vec<_>>()),
        actor
            .service
            .joined_coordination_state_for_tests(&parent.turn_id)
    );
    assert!(actor.service.has_joined_subagent_dependency(&child.turn_id));
    let dispatch = step(
        actor,
        child,
        vec![done("needed-child-done", "ACTUAL_NEEDED_RESULT")],
        generations,
    )
    .await;
    assert!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .any(
                |block| block.source == mez_agent::ContextSourceKind::PeerMessage
                    && block.content.contains("NEEDED_ANSWER")
            )
    );
    let dispatch = step(
        actor,
        parent,
        vec![done("needed-parent-done", "used the original child result")],
        generations,
    )
    .await;
    assert_eq!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .filter(
                |block| block.source == mez_agent::ContextSourceKind::ActionResult
                    && block.content.contains("ACTUAL_NEEDED_RESULT")
            )
            .count(),
        1
    );
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert_eq!(actor.service.joined_subagent_dependency_count(), 0);
}

/// A native command must settle once, reach a new actual model request, and
/// supply an answer while the original question child still awaits its result.
/// This exercises normal completion-group transfer rather than directly calling
/// a synchronous provider again while its previous callback group remains live.
#[tokio::test(flavor = "current_thread")]
async fn joined_dialogue_actor_native_result_precedes_answer() {
    let (mut actor, parent, child, mut generations) = actor_fixture(1).await;
    question(&mut actor, &parent, &child, &mut generations).await;
    step(
        &mut actor,
        &parent,
        vec![AgentAction {
            id: "needed-native-read".into(),
            payload: AgentActionPayload::ShellCommand {
                summary: "Read evidence needed for the child answer".into(),
                command: "printf NATIVE_TOOL_FIRST_BODY".into(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }],
        &mut generations,
    )
    .await;
    let mut commands = 0;
    for _ in 0..4 {
        let dispatch = actor
            .service
            .claim_native_shell_action(&parent.turn_id, "needed-native-read")
            .unwrap()
            .unwrap();
        commands += usize::from(!dispatch.capability_probe_only);
        let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
        assert!(actor.service.complete_native_shell_action(outcome).unwrap());
        if actor
            .service
            .agent_turn_executions()
            .get(&parent.turn_id)
            .unwrap()
            .action_results
            .iter()
            .find(|result| result.action_id == "needed-native-read")
            .unwrap()
            .is_terminal()
        {
            break;
        }
    }
    assert_eq!(commands, 1);
    assert!(
        actor
            .service
            .agent_provider_task_is_pending(&parent.turn_id)
    );
    let dispatch = step(
        &mut actor,
        &parent,
        vec![send(
            "native-derived-answer",
            &child.agent_id,
            "NEEDED_ANSWER",
        )],
        &mut generations,
    )
    .await;
    assert!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .any(
                |block| block.source == mez_agent::ContextSourceKind::ActionResult
                    && block.content.contains("NATIVE_TOOL_FIRST_BODY")
            )
    );
    finish(&mut actor, &parent, &child, &mut generations).await;
    assert_eq!(generations.len(), 7);
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 2);
}

/// A newly joined helper must finish before a parent can interpret its result
/// and answer an earlier joined child. Both actual task lifetimes and original
/// result slots remain distinct through the real actor's next lease handoff.
#[tokio::test(flavor = "current_thread")]
async fn joined_dialogue_actor_new_helper_result_precedes_original_answer() {
    let (mut actor, parent, child, mut generations) = actor_fixture(1).await;
    question(&mut actor, &parent, &child, &mut generations).await;
    step(
        &mut actor,
        &parent,
        vec![AgentAction {
            id: "needed-helper-spawn".into(),
            payload: AgentActionPayload::SpawnAgent {
                role: "default".into(),
                placement: "new-window".into(),
                cooperation_mode: "explore-only".into(),
                read_scopes: None,
                write_scopes: None,
                session_mode: None,
                size: None,
                reasoning_effort: None,
                lifetime: mez_agent::SubagentLifetime::Task,
                objective: None,
                task_prompt: "obtain evidence for the pending question".into(),
            },
        }],
        &mut generations,
    )
    .await;
    assert_eq!(actor.service.joined_subagent_dependency_count(), 2);
    assert!(
        !actor
            .service
            .agent_provider_task_is_pending(&parent.turn_id)
    );
    let helper = actor
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id != parent.turn_id && turn.turn_id != child.turn_id)
        .unwrap()
        .clone();
    assert!(
        actor
            .service
            .validate_native_agent_surface_startup(&helper.pane_id)
            .unwrap()
    );
    actor.service.set_pane_approval_policy_override(
        &helper.pane_id,
        Some(mez_agent::ApprovalPolicy::AutoAllow),
    );
    actor.service.start_ready_agent_turns().unwrap();
    step(
        &mut actor,
        &helper,
        vec![done("needed-helper-done", "ACTUAL_HELPER_EVIDENCE")],
        &mut generations,
    )
    .await;
    assert_eq!(actor.service.joined_subagent_dependency_count(), 1);
    assert!(
        actor
            .service
            .agent_provider_task_is_pending(&parent.turn_id)
    );
    let dispatch = step(
        &mut actor,
        &parent,
        vec![send(
            "helper-derived-answer",
            &child.agent_id,
            "NEEDED_ANSWER",
        )],
        &mut generations,
    )
    .await;
    assert_eq!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .filter(
                |block| block.source == mez_agent::ContextSourceKind::ActionResult
                    && block.content.contains("ACTUAL_HELPER_EVIDENCE")
            )
            .count(),
        1
    );
    finish(&mut actor, &parent, &child, &mut generations).await;
    assert_eq!(generations.len(), 8);
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 3);
}

/// A real child Stop must revoke its unanswered question before queued
/// preparation and during actual inference, with and without an intervening
/// tool batch. Both original and retained spawn sources are covered.
/// Redirection keeps the original join but cannot inherit question authority;
/// only a fresh native question from the replacement can resume coordination.
#[tokio::test(flavor = "current_thread")]
async fn joined_dialogue_actor_tool_continuation_revoked_by_child_stop() {
    for (tool_first, in_flight) in [(false, false), (false, true), (true, false), (true, true)] {
        let (mut actor, parent, child, mut generations, primary) =
            actor_fixture_with_actions(1, Vec::new()).await;
        question(&mut actor, &parent, &child, &mut generations).await;
        if tool_first {
            step(
                &mut actor,
                &parent,
                vec![AgentAction {
                    id: "before-stop-discovery".into(),
                    payload: AgentActionPayload::ListAgents {
                        agent_type: Some("all".into()),
                        scope: Some("session".into()),
                    },
                }],
                &mut generations,
            )
            .await;
        }
        assert!(
            actor
                .service
                .agent_provider_task_is_pending(&parent.turn_id)
        );
        let old = if in_flight {
            let dispatch = claim(&mut actor, &parent).await;
            assert!(generations.insert(dispatch.claim_generation));
            let execution = execute(
                &dispatch,
                &response(vec![send(
                    "stopped-answer",
                    &child.agent_id,
                    "STOPPED_QUESTION_MUST_NOT_AUTHORIZE",
                )]),
            )
            .await;
            Some((dispatch, execution))
        } else {
            None
        };
        actor
            .service
            .execute_agent_shell_command_for_pane(&primary, &child.pane_id, "/stop")
            .unwrap();
        assert!(
            actor
                .service
                .interrupted_subagent_awaits_redirection(&child.turn_id)
        );
        if let Some((dispatch, execution)) = old {
            assert_eq!(complete(&mut actor, &dispatch, execution).await, 1);
            let child_id = AgentId::opaque(child.agent_id.clone()).unwrap();
            assert!(
                actor
                    .service
                    .message_service()
                    .receive_for(&child_id, u64::MAX)
                    .iter()
                    .all(|message| message.payload != "STOPPED_QUESTION_MUST_NOT_AUTHORIZE")
            );
        }
        let (reply, receiver) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
                agent_id: AgentId::opaque(parent.agent_id.clone()).unwrap(),
                turn_id: parent.turn_id.clone(),
                reply,
            })
            .await;
        assert!(
            receiver.await.unwrap().unwrap().is_none(),
            "a stopped question must revoke queued preparation; in_flight={in_flight}"
        );
        assert!(
            !actor
                .service
                .agent_provider_task_is_pending(&parent.turn_id)
        );
        assert_eq!(
            actor
                .service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        let prompt = "continue the original delegated task with new guidance";
        let mut capture = actor.service.prepare_agent_prompt_command_capture(
            &primary,
            &child.pane_id,
            prompt,
            false,
        );
        actor
            .service
            .execute_attached_agent_shell_command(&primary, &child.pane_id, prompt, &mut capture)
            .unwrap();
        let replacement = actor
            .service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.pane_id == child.pane_id && turn.turn_id != child.turn_id)
            .unwrap()
            .clone();
        assert_ne!(replacement.turn_id, child.turn_id);
        actor.service.start_ready_agent_turns().unwrap();
        question(&mut actor, &parent, &replacement, &mut generations).await;
        step(
            &mut actor,
            &parent,
            vec![send(
                "replacement-answer",
                &replacement.agent_id,
                "NEEDED_ANSWER",
            )],
            &mut generations,
        )
        .await;
        finish(&mut actor, &parent, &replacement, &mut generations).await;
        assert_eq!(
            generations.len(),
            8 + usize::from(tool_first) + usize::from(in_flight)
        );
        assert_eq!(actor.service.agent_turn_ledger().turns().len(), 3);
    }
}
