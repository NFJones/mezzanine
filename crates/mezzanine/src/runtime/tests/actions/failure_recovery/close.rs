//! Persistent-child pre-close rejection recovery through real provider cycles.
//!
//! The synchronous test provider supplies model-authored batches, not fabricated
//! close results. Production execution and settlement retain opaque errors and
//! consume the existing stable-signature budget without replaying effects.

use super::*;

/// Builds an unknown-child close batch for the normal provider execution path.
fn provider(id: &str) -> RuntimeBatchProvider {
    RuntimeBatchProvider {
        response: runtime_spawn_execution_for_actions(
            &AgentTurnRecord {
                turn_id: "fixture".into(),
                conversation_id: "fixture".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                trigger: mez_agent::AgentTurnTrigger::UserPrompt,
                started_at_unix_seconds: 1,
                deadline_at_unix_millis: 0,
                policy_profile: "default".into(),
                model_profile: "default".into(),
                parent_turn_id: None,
                state: AgentTurnState::Running,
                cooperation_mode: None,
                initial_capability: None,
            },
            vec![mez_agent::AgentAction {
                id: id.into(),
                payload: mez_agent::AgentActionPayload::CloseAgent {
                    agent_id: "agent-%999".into(),
                },
            }],
        )
        .response,
    }
}

/// A real unavailable close must queue correction once, preserving the rejected
/// result and unchanged pane resources. The next model batch may change course
/// and finish normally; no invented close success or automatic replay occurs.
#[test]
fn runtime_unavailable_close_recovers_through_provider_cycle() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn = service
        .start_agent_prompt_turn("%1", "close or continue locally")
        .unwrap();
    let windows = service.session().windows().len();
    let first = service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &provider("unknown"),
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(
        !first.action_results.is_empty(),
        "{}",
        first.response.raw_text
    );
    assert_eq!(first.action_results[0].status, ActionStatus::Rejected);
    assert_eq!(
        first.action_results[0].error.as_ref().unwrap().code,
        "unavailable"
    );
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert_eq!(service.session().windows().len(), windows);
    assert!(service.agent_provider_task_is_pending(&turn.turn_id));
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::ActionResult
                && block.content.contains("unavailable"))
            .count(),
        1
    );
    let mut next = provider("not-used");
    next.response.action_batch.as_mut().unwrap().actions = vec![mez_agent::AgentAction {
        id: "finish-locally".into(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            content_type: "text/plain".into(),
            text: "continued locally".into(),
        },
    }];
    let completed = service
        .poll_agent_provider_tasks_with_provider(&next, 1)
        .unwrap()
        .remove(0);
    assert_eq!(completed.terminal_state, AgentTurnState::Completed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.session().windows().len(), windows);
    service.terminate_all_pane_processes().unwrap();
}

/// The positive minimum budget allows exactly one correction; the test-only
/// zero injection is clamped to that established minimum (config rejects zero).
/// Changing action IDs cannot reset the stable unavailable-close signature or
/// preserve an indefinitely running parent turn.
#[test]
fn runtime_unavailable_close_exhausts_stable_correction_budget() {
    for limit in [0, 1] {
        let mut service = test_runtime_service();
        service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
        service.set_agent_action_failure_retry_limit(limit);
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service.start_initial_pane_process(Some("cat")).unwrap();
        service.permission_policy_mut().set_approval_bypass(true);
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let turn = service
            .start_agent_prompt_turn("%1", "close child")
            .unwrap();
        let first = service
            .execute_agent_turn_with_provider(
                &turn.turn_id,
                &provider("first"),
                runtime_model_profile("runtime-batch", "test"),
            )
            .unwrap();
        assert_eq!(first.terminal_state, AgentTurnState::Running);
        let last = service
            .poll_agent_provider_tasks_with_provider(&provider("new-id"), 1)
            .unwrap()
            .remove(0);
        assert_eq!(last.terminal_state, AgentTurnState::Failed);
        assert!(service.pending_agent_provider_tasks().is_empty());
        service.terminate_all_pane_processes().unwrap();
    }
}

/// After an unavailable close, model-authored discovery may identify an actual
/// owned persistent child and correct the target. The executor revalidates that
/// ownership before retiring the child; the earlier rejection never claims an
/// effect and discovery cannot grant authority to close another parent's child.
#[test]
fn runtime_unavailable_close_discovers_and_corrects_owned_child() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root("close-correction")));
    service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover and close owned child")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let mut spawn = runtime_spawn_agent_action("persistent", "");
    if let mez_agent::AgentActionPayload::SpawnAgent {
        lifetime,
        objective,
        ..
    } = &mut spawn.payload
    {
        *lifetime = mez_agent::SubagentLifetime::Persistent;
        *objective = Some("review reusable work".into());
    }
    let spawned = service
        .execute_spawn_action_for_turn(&turn, &spawn)
        .unwrap();
    let spawned: serde_json::Value =
        serde_json::from_str(spawned.structured_content_json.as_deref().unwrap()).unwrap();
    let child = spawned["spawn"]["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let pane = spawned["spawn"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let failed = service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &provider("unknown"),
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(failed.terminal_state, AgentTurnState::Running);
    let mut discovery = provider("unused");
    discovery.response.action_batch.as_mut().unwrap().actions = vec![mez_agent::AgentAction {
        id: "discover-child".into(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".into()),
            scope: None,
        },
    }];
    let listed = service
        .poll_agent_provider_tasks_with_provider(&discovery, 1)
        .unwrap()
        .remove(0);
    assert_eq!(listed.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        listed.action_results[0]
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains(&child))
    );
    let mut corrected = provider("verified-child");
    if let mez_agent::AgentActionPayload::CloseAgent { agent_id } =
        &mut corrected.response.action_batch.as_mut().unwrap().actions[0].payload
    {
        *agent_id = child.clone();
    }
    let closed = service
        .poll_agent_provider_tasks_with_provider(&corrected, 1)
        .unwrap()
        .remove(0);
    assert_eq!(closed.action_results[0].status, ActionStatus::Succeeded);
    assert!(service.persistent_subagent(&child).is_none());
    assert!(!service.pane_processes().contains_pane(&pane));
    service.terminate_all_pane_processes().unwrap();
}

/// The new close exception reuses sibling safety gates: settled success and
/// an inactive unsent shell sibling allow correction, while running subagent
/// or approval-blocked sibling work prevents it. No sibling is redispatched
/// merely to reconstruct a result or obtain another correction opportunity.
#[test]
fn runtime_unavailable_close_preserves_sibling_recovery_gates() {
    for (kind, expected) in [
        ("success", true),
        ("unsent", true),
        ("subagent", false),
        ("approval", false),
    ] {
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "correct close safely")
            .unwrap();
        service.remove_pending_agent_provider_task(&started.turn_id);
        let turn = service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .clone();
        let close = provider("unknown")
            .response
            .action_batch
            .unwrap()
            .actions
            .remove(0);
        let sibling = if kind == "subagent" {
            runtime_spawn_agent_action("sibling", "active work")
        } else {
            mez_agent::AgentAction {
                id: "sibling".into(),
                payload: mez_agent::AgentActionPayload::ShellCommand {
                    summary: "inspect".into(),
                    command: "true".into(),
                    interactive: false,
                    stateful: false,
                    timeout_ms: None,
                },
            }
        };
        let mut execution =
            runtime_spawn_execution_for_actions(&turn, vec![close, sibling.clone()]);
        assert_eq!(
            service
                .execute_running_close_agent_actions_for_turn(&turn, &mut execution)
                .unwrap(),
            1
        );
        if kind == "success" {
            execution.action_results[1] =
                mez_agent::ActionResult::succeeded(&turn, &sibling, vec!["completed".into()], None);
        }
        if kind == "approval" {
            execution.action_results[1].status = ActionStatus::Blocked;
        }
        append_test_execution_assistant_context(&mut service, &turn, &execution);
        assert_eq!(
            service
                .queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "close-recovery-test"
                )
                .unwrap(),
            expected,
            "{kind}"
        );
        assert_eq!(
            service.agent_provider_task_is_pending(&turn.turn_id),
            expected
        );
    }
}
