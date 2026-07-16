//! Runtime tests for actions failure recovery behavior.

use super::*;

/// Verifies successive shell dispatches add a gentle action-pressure hint.
///
/// Repeated shell inspection attempts can keep a long turn localizing the same
/// owner instead of implementing the next phase, even when those attempts do
/// not settle successfully. The runtime should nudge the next provider
/// continuation after the configured gentle threshold while keeping the hint
/// volatile and advisory rather than failing the shell action.
#[test]
fn runtime_action_pressure_context_reaches_provider_continuation() {
    let mut service = test_runtime_service();
    service.set_agent_implementation_pressure_after_shell_actions(3);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-implementation-pressure","input":"finish the backlog fixes"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    service.record_shell_dispatch_history("turn-1", "sed -n '1,80p' src/runtime/mod.rs");
    assert!(
        !service
            .agent_turn_contexts
            .get("turn-1")
            .unwrap()
            .blocks
            .iter()
            .any(|block| block.label == "action pressure")
    );

    service.record_shell_dispatch_history("turn-1", "sed -n '80,160p' src/runtime/mod.rs");
    assert!(
        !service
            .agent_turn_contexts
            .get("turn-1")
            .unwrap()
            .blocks
            .iter()
            .any(|block| block.label == "action pressure")
    );

    service.record_shell_dispatch_history("turn-1", "sed -n '160,240p' src/runtime/mod.rs");
    let pressure_block = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .find(|block| block.label == "action pressure")
        .expect("action pressure should be active turn context");
    assert_eq!(
        pressure_block.cache_policy(),
        mez_agent::ContextCachePolicy::Ineligible
    );
    assert!(
        pressure_block
            .content
            .contains("3 consecutive shell_command actions"),
        "{}",
        pressure_block.content
    );
    assert!(
        pressure_block.content.contains("Apply gentle pressure now"),
        "{}",
        pressure_block.content
    );
    assert!(
        pressure_block
            .content
            .contains("Continue following active repository guidance"),
        "{}",
        pressure_block.content
    );
    assert!(
        pressure_block
            .content
            .contains("Do not edit repository instruction or guidance files merely"),
        "{}",
        pressure_block.content
    );

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &second_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::RuntimeHint
            && message.content.contains("[action pressure]")
            && message
                .content
                .contains("Use another shell_command only for one named missing fact")
            && message
                .content
                .contains("does not relax repository rules or permission/capability requirements")
    }));
}

/// Verifies inspection pressure escalates from gentle to medium to strong as
/// repeated shell-command dispatches continue in one turn.
///
/// The runtime-owned hint should become more forceful after prolonged
/// inspection streaks while staying advisory and turn-volatile.
#[test]
fn runtime_action_pressure_escalates_through_stages() {
    let mut service = test_runtime_service();
    service.set_agent_implementation_pressure_after_shell_actions(3);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-implementation-pressure-escalation","input":"fix the owner once you have enough evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    for index in 0..6 {
        service.record_shell_dispatch_history(
            "turn-1",
            &format!("sed -n '{}p' src/runtime/mod.rs", index + 1),
        );
    }
    let medium_block = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .find(|block| block.label == "action pressure")
        .expect("medium action pressure should be active");
    assert!(
        medium_block.content.contains("Apply medium pressure now"),
        "{}",
        medium_block.content
    );
    assert!(
        medium_block
            .content
            .contains("focused regression test, execution-based validation"),
        "{}",
        medium_block.content
    );

    for index in 6..10 {
        service.record_shell_dispatch_history(
            "turn-1",
            &format!("sed -n '{}p' src/runtime/mod.rs", index + 1),
        );
    }
    let strong_block = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .find(|block| block.label == "action pressure")
        .expect("strong action pressure should be active");
    assert!(
        strong_block.content.contains("Apply strong pressure now"),
        "{}",
        strong_block.content
    );
    assert!(
        strong_block
            .content
            .contains("concrete justification from recent evidence"),
        "{}",
        strong_block.content
    );
}

/// Verifies a stale running `spawn_agent` result without a live joined child is
/// not treated as a runtime progress path.
///
/// The recovery loop must be able to fail or repair an orphaned parent turn
/// instead of considering any running `spawn_agent` result sufficient evidence
/// that a child can still complete.
#[test]
fn runtime_stale_joined_spawn_result_is_unreachable_progress() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn = runtime_spawn_agent_action("spawn-stale", "missing child");
    service.agent_turn_executions.insert(
        parent.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "spawn child".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: parent.turn_id.clone(),
                    agent_id: parent.agent_id.clone(),
                    actions: vec![spawn.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &parent_turn,
                &spawn,
                vec!["waiting for missing child".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.pending_agent_provider_tasks.remove(&parent.turn_id);

    assert!(
        service.unreachable_running_agent_turn_timer_needed_with_actor_progress(
            &std::collections::BTreeSet::new()
        )
    );
    assert_eq!(
        service
            .reconcile_agent_runtime_progress_paths_with_actor_progress(
                &std::collections::BTreeSet::new(),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(!service.agent_turn_executions.contains_key(&parent.turn_id));
}

/// Verifies unrecovered failures explain when recovery is unavailable because
/// a sibling action has not settled.
///
/// The runtime cannot feed a partial batch back to the model without risking a
/// correction prompt that ignores still-running or blocked actions. The final
/// failure line should make that blocker explicit instead of using a bare
/// "recovery unavailable" suffix.
#[test]
fn runtime_unrecovered_failure_with_pending_sibling_explains_blocker() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch and inspect")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let patch_action = mez_agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let read_action = mez_agent::AgentAction {
        id: "read-pending".to_string(),
        rationale: "read the target file".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Read the target file".to_string(),
            command: "sed -n '1,120p' src/lib.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &patch_action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "combined_output_preview": "apply_patch: hunk did not match: src/lib.rs",
                "combined_output_bytes": 44,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let pending = mez_agent::ActionResult::running(
        &turn,
        &read_action,
        vec!["local action accepted for pane execution".to_string()],
        None,
    );
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "partial batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![patch_action, read_action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![failed, pending],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert(turn.turn_id.clone(), execution);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("recovery unavailable: action result(s) are still pend"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("read-pending shell_command running no_error_code"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies unrecovered failures explain when the failed result is outside the
/// model-correction path.
///
/// Policy/user-boundary outcomes must not be retried by the model. The final
/// failure line should still identify the non-correctable result so the user
/// can distinguish that boundary from a missing retry loop.
#[test]
fn runtime_unrecovered_non_correctable_failure_explains_boundary() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "write the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-denied".to_string(),
        rationale: "write a source file".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let denied = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Denied,
        "approval_denied",
        "user denied the action",
    )
    .unwrap();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "denied write".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![denied],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert(turn.turn_id.clone(), execution);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("recovery unavailable: no model-correctable"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("patch-denied apply_patch denied"),
        "{pane_text}"
    );
    assert!(pane_text.contains("approval_denied"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies subagent spawn-limit denials are recoverable model feedback.
///
/// Capacity exhaustion is a transient scheduling condition, not a malformed
/// delegation request. The parent model should receive the denial as action
/// result context so it can continue locally or wait for existing children
/// instead of having the turn fail immediately.
#[test]
fn runtime_spawn_agent_action_succeeds_while_primary_is_detached() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "delegate while detached")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();

    let action = runtime_spawn_agent_action("spawn-detached", "inspect detached state");
    let result = service
        .execute_spawn_action_for_turn(&turn, &action)
        .unwrap();

    assert_eq!(result.status, ActionStatus::Running);
    assert!(service.session().primary_client_id().is_none());
    assert_eq!(service.joined_subagent_dependencies.len(), 1);
    assert!(service.session().windows().len() > 1);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies subagent spawn-limit denials are recoverable model feedback.
///
/// Capacity exhaustion is a transient scheduling condition, not a malformed
/// delegation request. The parent model should receive the denial as action
/// result context so it can continue locally or wait for existing children
/// instead of having the turn fail immediately.
#[test]
fn runtime_spawn_limit_denial_queues_model_recovery() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-spawn-limit-feedback","input":"delegate until capacity is full"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let action = runtime_spawn_agent_action("spawn-over-capacity", "start another child");
    let denied = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Denied,
        "forbidden",
        "subagent spawn limit reached for agent-%1: active direct children 4, agents.max_root_subagents 4",
    )
    .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn over capacity".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![denied],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "subagent_spawn_limit_reached",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(service.pending_agent_provider_tasks.contains("turn-1"));
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result spawn-over-capacity spawn_agent denied]")
            && block.content.contains("subagent spawn limit reached")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content.contains("attempt=1 max=5")
            && block.content.contains("Spawn-agent recovery")
    }));
    service.terminate_all_pane_processes().unwrap();
}
