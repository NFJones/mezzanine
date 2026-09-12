//! Runtime tests for actions failure recovery behavior.

use super::*;

/// Builds a running agent turn in a secondary pane that can be removed without
/// terminating the complete test session.
///
/// The fixture removes provider ownership so reconciliation must either settle
/// the pane-owned turn or identify it as unreachable. Both pane-removal
/// regressions use the same illegal interleaving that previously reached the
/// async pane supervisor.
fn removable_pane_running_turn_fixture() -> (RuntimeSessionService, String, String) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap()
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen(pane_id.clone(), screen);
    let turn_id = service
        .start_agent_prompt_turn(&pane_id, "finish after pane removal")
        .unwrap()
        .turn_id;
    service.remove_pending_agent_provider_task(&turn_id);
    (service, pane_id, turn_id)
}

/// Verifies shell-history bookkeeping never enters durable or prepared model context.
///
/// Exact dispatch history remains controller-owned for loop detection; repeated
/// inspection alone must not create model-facing pressure reminders.
#[test]
fn runtime_shell_history_remains_outside_model_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-shell-history","input":"finish the backlog fixes"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    for index in 0..12 {
        service.record_shell_dispatch_history(
            "turn-1",
            &format!("sed -n '{}p' src/runtime/mod.rs", index + 1),
        );
    }
    let durable = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        durable
            .blocks()
            .iter()
            .all(|block| block.label != "action pressure")
    );

    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let (prepared, _) = service
        .prepare_agent_turn_model_context(
            &turn,
            durable.clone(),
            &mez_agent::McpPromptSummary {
                available_servers: Vec::new(),
                available_tools: Vec::new(),
                unavailable_servers: Vec::new(),
            },
            &runtime_model_profile("openai", "test"),
        )
        .unwrap();
    assert!(
        prepared
            .to_agent_context()
            .blocks()
            .iter()
            .all(|block| block.label != "action pressure")
    );
}

/// Verifies an arbitrarily named provider using the OpenAI Responses wire API
/// does not receive a request-local MCP manifest during context preparation.
#[test]
fn runtime_mcp_context_uses_resolved_api_for_aliased_openai_provider() {
    let mut service = test_runtime_service();
    service
        .integration
        .provider_registry_mut()
        .providers
        .insert(
            "enterprise".to_string(),
            crate::runtime::RuntimeProviderConfig {
                provider_id: "enterprise".to_string(),
                kind: "custom".to_string(),
                api: Some(mez_agent::OPENAI_RESPONSES_API.to_string()),
                auth_profile: "default".to_string(),
                base_url: None,
                models: vec![mez_agent::ProviderModelConfig::named("test")],
                default_model: Some("test".to_string()),
                options: std::collections::BTreeMap::new(),
                unknown_model_policy: "conservative".to_string(),
            },
        );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"aliased-mcp-context","input":"use @fs to read the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let durable = service.agent_turn_contexts().get("turn-1").unwrap().clone();
    let summary = mez_agent::McpPromptSummary {
        available_servers: vec![mez_agent::McpPromptServer {
            server_id: "fs".to_string(),
            display_name: "Filesystem".to_string(),
            purpose: "Read files".to_string(),
            usage_instructions: "Use read_file".to_string(),
            tool_count: 1,
            approval_required_tool_count: 0,
        }],
        available_tools: vec![mez_agent::McpPromptTool {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            description: "Read a file".to_string(),
            approval_required: false,
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        }],
        unavailable_servers: Vec::new(),
    };

    let (prepared, tools) = service
        .prepare_agent_turn_model_context(
            &turn,
            durable,
            &summary,
            &runtime_model_profile("enterprise", "test"),
        )
        .unwrap();
    let context = prepared.to_agent_context();
    assert!(
        context
            .blocks()
            .iter()
            .all(|block| block.label != "mcp integrations")
    );

    assert!(tools.is_empty());
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
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn = runtime_spawn_agent_action("spawn-stale", "missing child");
    service.agent_turn_executions_mut().insert(
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
                    rationale: "test action batch rationale".to_string(),

                    actions: vec![spawn.clone()],
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
    service.remove_pending_agent_provider_task(&parent.turn_id);

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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(
        !service
            .agent_turn_executions()
            .contains_key(&parent.turn_id)
    );
}

/// Verifies removed-pane cleanup terminalizes pane-owned agent work before it
/// deletes the shell session and independent presentation screens.
///
/// Layout rollback and best-effort subagent cleanup can reach this helper after
/// the pane is already absent. Leaving the ledger turn running would make the
/// next actor event attempt to present a recovery diagnostic to that missing
/// pane and fail the pane-process supervisor.
#[test]
fn runtime_removed_pane_cleanup_terminalizes_running_turn() {
    let (mut service, pane_id, turn_id) = removable_pane_running_turn_fixture();
    service
        .session
        .kill_pane_session_owned(Some(&pane_id), true)
        .unwrap();

    service
        .cleanup_removed_pane_runtime_state(&pane_id)
        .unwrap();

    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(service.agent_shell_store().get(&pane_id).is_none());
    assert_eq!(
        service
            .reconcile_agent_runtime_progress_paths_with_actor_progress(
                &std::collections::BTreeSet::new(),
            )
            .unwrap(),
        0
    );
}

/// Verifies reconciliation treats a missing layout pane as terminal even when
/// its provider task still appears to own a live progress path.
///
/// This defensive path covers a pane-removal event that reaches the actor
/// before normal runtime cleanup. Pane liveness must be checked before provider
/// ownership, otherwise the retained provider bit hides the missing pane from
/// unreachable-turn recovery and a later callback can present to that pane.
#[test]
fn runtime_reconciliation_fences_missing_pane_with_provider_progress() {
    let (mut service, pane_id, turn_id) = removable_pane_running_turn_fixture();
    assert!(service.queue_agent_provider_task(turn_id.clone()));
    service
        .session
        .kill_pane_session_owned(Some(&pane_id), true)
        .unwrap();

    assert!(
        service.idle_cleanup_timer_needed_with_actor_progress(&std::collections::BTreeSet::new())
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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(service.agent_shell_store().get(&pane_id).is_none());
}

/// Verifies generic turn settlement cannot recreate or present through an
/// agent shell session after the layout pane has disappeared.
///
/// Provider and routed-workflow completion paths can settle a turn without a
/// running shell binding. A retained conversation is not sufficient evidence
/// of pane ownership, so this path must return the removed session only as a
/// caller snapshot and leave no pane-local runtime state behind.
#[test]
fn runtime_non_shell_settlement_does_not_recreate_missing_pane_session() {
    let (mut service, pane_id, turn_id) = removable_pane_running_turn_fixture();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    service
        .session
        .kill_pane_session_owned(Some(&pane_id), true)
        .unwrap();

    let removed_session = service
        .finish_agent_turn_without_shell_session(&turn, AgentTurnState::Failed)
        .unwrap();

    assert_eq!(
        removed_session
            .as_ref()
            .map(|session| session.pane_id.as_str()),
        Some(pane_id.as_str())
    );
    assert!(service.agent_shell_store().get(&pane_id).is_none());
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
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
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let patch_action = mez_agent::AgentAction {
        id: "patch-fail".to_string(),

        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let read_action = mez_agent::AgentAction {
        id: "read-pending".to_string(),

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
                rationale: "test action batch rationale".to_string(),

                actions: vec![patch_action, read_action],
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
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let normalized_pane_text = normalized_pane_log_text(&pane_text);
    assert!(
        normalized_pane_text.contains("recovery unavailable: action result(s) are still pending"),
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
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-denied".to_string(),

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
                rationale: "test action batch rationale".to_string(),

                actions: vec![action],
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
        .agent_turn_executions_mut()
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
        .agent_turn_ledger()
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
    assert!(
        result
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains(r#""session":"new""#)),
        "{:?}",
        result.structured_content_json
    );
    assert!(service.session().layout_owner_client_id().is_none());
    assert_eq!(service.joined_subagent_dependency_count(), 1);
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
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger()
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
                rationale: "test action batch rationale".to_string(),

                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![denied],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "subagent_spawn_limit_reached",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(service.agent_provider_task_is_pending("turn-1"));
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result spawn-over-capacity spawn_agent denied]")
            && block.content.contains("subagent spawn limit reached")
    }));
    assert!(
        context
            .blocks()
            .iter()
            .all(|block| block.source != ContextSourceKind::RuntimeHint)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an immutable current-agent depth denial enters bounded recovery.
///
/// Unlike transient child capacity, reaching the lineage depth limit requires
/// the current agent to stop delegating and finish with direct actions.
#[test]
fn runtime_spawn_depth_denial_queues_model_recovery() {
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-depth-limit-feedback","input":"delegate at maximum depth"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let action = runtime_spawn_agent_action("spawn-over-depth", "start a nested child");
    let denied = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Denied,
        "forbidden",
        "subagent depth limit reached for agent-%1: depth 2 of 2",
    )
    .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn over depth".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test action batch rationale".to_string(),

                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![denied],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "subagent_depth_limit_reached",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(service.agent_provider_task_is_pending("turn-1"));
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result spawn-over-depth spawn_agent denied]")
            && block.content.contains("subagent depth limit reached")
    }));
    assert!(
        context
            .blocks()
            .iter()
            .all(|block| block.source != ContextSourceKind::RuntimeHint)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the actual maximum-depth spawn path creates no child state and
/// returns explicit direct-execution guidance in the action result itself.
#[test]
fn runtime_spawn_depth_denial_has_guidance_and_no_spawn_side_effects() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "attempt nested delegation")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    assert_eq!(service.max_subagent_depth(), 2);
    service.set_subagent_lineage(
        turn.agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: "agent-parent".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "depth-limited child".to_string(),
            terminal: false,
        },
    );
    let allowed_actions = service
        .agent_provider_request_control_for_turn(&turn)
        .expect("provider control should capture the session action schema")
        .0
        .expect("provider turns should retain their static action set");
    assert!(allowed_actions.contains(mez_agent::AllowedAction::SpawnAgent));

    let action = runtime_spawn_agent_action("spawn-at-depth-limit", "start a child");
    let planned = mez_agent::ActionResult::running(&turn, &action, Vec::new(), None);
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn at depth limit".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "delegate nested work".to_string(),
                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![planned],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    let turn_count = service.agent_turn_ledger().turns().len();
    let window_count = service.session().windows().len();

    assert_eq!(
        service
            .execute_running_spawn_actions_for_turn(&turn, &mut execution)
            .unwrap(),
        1
    );

    let denied = &execution.action_results[0];
    assert_eq!(denied.status, ActionStatus::Denied);
    let diagnostic = "subagent depth limit reached for agent-%1: depth 2 of 2";
    assert_eq!(denied.error.as_ref().unwrap().message, diagnostic);
    assert!(
        denied
            .content
            .iter()
            .any(|block| block.text.contains("no child was created"))
    );
    assert!(
        denied
            .content
            .iter()
            .any(|block| block.text.contains("do not retry spawn_agent"))
    );
    let structured = denied.structured_content_json.as_deref().unwrap();
    assert!(structured.contains(diagnostic), "{structured}");
    assert!(
        structured.contains("maximum delegation depth reached"),
        "{structured}"
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert_eq!(service.session().windows().len(), window_count);

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution,
                "subagent_depth_limit_reached",
            )
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content.contains(diagnostic)
            && block.content.contains("no child was created")
            && block.content.contains("do not retry spawn_agent")
            && block
                .content
                .contains("complete the remaining work directly")
    }));
    assert!(
        context
            .blocks()
            .iter()
            .all(|block| block.source != ContextSourceKind::RuntimeHint)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Builds one spawn action with an explicit role and cooperation mode.
///
/// The shared fixture helper only produces the compact explore-only default, so
/// authority-focused recovery tests override those two fields directly.
fn runtime_spawn_agent_action_with_authority(
    id: &str,
    task_prompt: &str,
    role: &str,
    cooperation_mode: &str,
) -> mez_agent::AgentAction {
    let mut action = runtime_spawn_agent_action(id, task_prompt);
    if let mez_agent::AgentActionPayload::SpawnAgent {
        role: action_role,
        cooperation_mode: action_mode,
        ..
    } = &mut action.payload
    {
        *action_role = role.to_string();
        *action_mode = cooperation_mode.to_string();
    }
    action
}

/// Builds one running execution around a single action batch of spawn actions.
fn runtime_spawn_execution_for_actions(
    turn: &AgentTurnRecord,
    actions: Vec<mez_agent::AgentAction>,
) -> mez_agent::AgentTurnExecution {
    let action_results = actions
        .iter()
        .map(|action| mez_agent::ActionResult::running(turn, action, Vec::new(), None))
        .collect::<Vec<_>>();
    mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "delegate the delegated task".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "delegate the delegated task".to_string(),
                actions: actions.clone(),
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results,
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    }
}

/// Verifies an unapproved unrestricted spawn is a nonrecoverable denial.
///
/// Root filesystem bounds cannot authorize unrestricted writes, so the denial
/// must stay Forbidden, carry explicit no-child-created evidence, and remain
/// outside the bounded action-failure correction path instead of being
/// relabelled as a retryable argument problem.
#[test]
fn runtime_unapproved_unrestricted_spawn_denial_is_nonrecoverable() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "escalate my authority")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = runtime_spawn_agent_action_with_authority(
        "spawn-unapproved-unrestricted",
        "take unrestricted authority",
        "worker",
        "unrestricted",
    );
    let mut execution = runtime_spawn_execution_for_actions(&turn, vec![action]);
    let window_count = service.session().windows().len();
    let turn_count = service.agent_turn_ledger().turns().len();

    assert_eq!(
        service
            .execute_running_spawn_actions_for_turn(&turn, &mut execution)
            .unwrap(),
        1
    );

    let denied = &execution.action_results[0];
    assert_eq!(denied.status, ActionStatus::Denied);
    assert_eq!(denied.error.as_ref().unwrap().code, "forbidden");
    assert_eq!(
        denied.error.as_ref().unwrap().message,
        "unrestricted subagent writes require explicit user approval"
    );
    let structured = denied.structured_content_json.as_deref().unwrap();
    assert!(structured.contains("\"spawn\":null"), "{structured}");
    assert!(
        structured.contains("\"child_created\":false"),
        "{structured}"
    );
    assert!(!mez_agent::outcome::runtime_action_result_is_feedback_candidate(denied));
    assert_eq!(service.session().windows().len(), window_count);
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a correctable spawn shape error reports no child and enters the
/// bounded correction path.
///
/// An unsupported profile is a malformed delegation request: the action result
/// must report a null spawn with no child created, and the existing bounded
/// failure feedback must queue model correction.
#[test]
fn runtime_correctable_spawn_shape_error_reports_no_child_and_queues_correction() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "delegate to a missing profile")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = runtime_spawn_agent_action_with_authority(
        "spawn-unsupported-role",
        "delegate to a missing profile",
        "missing-profile",
        "explore-only",
    );
    let mut execution = runtime_spawn_execution_for_actions(&turn, vec![action]);
    let window_count = service.session().windows().len();

    assert_eq!(
        service
            .execute_running_spawn_actions_for_turn(&turn, &mut execution)
            .unwrap(),
        1
    );

    let failed = &execution.action_results[0];
    assert_eq!(failed.status, ActionStatus::Failed);
    assert_eq!(failed.error.as_ref().unwrap().code, "invalid_params");
    let structured = failed.structured_content_json.as_deref().unwrap();
    assert!(structured.contains("\"spawn\":null"), "{structured}");
    assert!(
        structured.contains("\"child_created\":false"),
        "{structured}"
    );
    assert!(mez_agent::outcome::runtime_action_result_is_feedback_candidate(failed));
    assert_eq!(service.session().windows().len(), window_count);
    assert_eq!(service.joined_subagent_dependency_count(), 0);

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution,
                "subagent_spawn_validation_failed",
            )
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content.contains("spawn-unsupported-role")
            && block.content.contains("unsupported subagent role")
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a mixed spawn batch corrects the failed sibling while retaining the
/// child that was actually created.
///
/// One correctable failure must not discard a successful sibling or duplicate
/// its child, and the bounded correction context must carry both results.
#[test]
fn runtime_mixed_spawn_batch_correction_retains_successful_sibling() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.configure_subagent_policy(4, 4, 2, 2, SubagentWaitPolicy::Detach);
    let started = service
        .start_agent_prompt_turn("%1", "delegate two tasks")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let success = runtime_spawn_agent_action_with_authority(
        "spawn-mixed-success",
        "inspect the repository",
        "explorer",
        "explore-only",
    );
    let failure = runtime_spawn_agent_action_with_authority(
        "spawn-mixed-failure",
        "delegate to a missing profile",
        "missing-profile",
        "explore-only",
    );
    let mut execution = runtime_spawn_execution_for_actions(&turn, vec![success, failure]);
    let window_count = service.session().windows().len();

    assert_eq!(
        service
            .execute_running_spawn_actions_for_turn(&turn, &mut execution)
            .unwrap(),
        2
    );

    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let failed = &execution.action_results[1];
    assert_eq!(failed.status, ActionStatus::Failed);
    assert!(
        failed
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"child_created\":false")
    );
    assert_eq!(service.session().windows().len(), window_count + 1);

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution,
                "subagent_spawn_validation_failed",
            )
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content.contains("spawn-mixed-success")
    }));
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content.contains("spawn-mixed-failure")
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies duplicate spawn delivery reconciles to one child.
///
/// Replaying the same mutating control request with the same idempotency key
/// must return the recorded response instead of allocating a second pane, turn,
/// or lineage record for the same delegation.
#[test]
fn runtime_duplicate_spawn_control_delivery_returns_same_child() {
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
    let request = r#"{"jsonrpc":"2.0","id":"spawn-duplicate","method":"agent/spawn","params":{"parent_agent":{"agent_id":"agent-%1"},"placement":{"mode":"new-pane"},"role":"explorer","cooperation_mode":"explore-only","prompt":"inspect the repository","idempotency_key":"duplicate-spawn-key"}}"#;
    let window_count = service.session().windows().len();

    let first = service.dispatch_runtime_control_body(request, &primary);
    let second = service.dispatch_runtime_control_body(request, &primary);

    assert!(first.contains("\"result\""), "{first}");
    assert_eq!(first, second);
    assert_eq!(service.session().windows().len(), window_count + 1);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies spawn-param parsing never mints approval from a requested mode.
///
/// Approval provenance is an authenticated-control fact supplied by the caller,
/// never by the request payload. An unrestricted request must not become
/// approvable merely by naming that mode.
#[test]
fn runtime_spawn_params_require_caller_approval_provenance() {
    let params = r#"{"parent_agent":{"agent_id":"agent-%1"},"placement":"new-pane","role":"worker","cooperation_mode":"unrestricted","prompt":"escalate my authority"}"#;
    let requested = crate::runtime::runtime_subagent_spawn_request(
        params,
        mez_agent::SubagentApprovalProvenance::Requested,
    )
    .unwrap();
    assert!(!requested.explicit_user_approval);
    assert_eq!(
        requested.validate().unwrap_err().kind(),
        mez_agent::SubagentContractErrorKind::Forbidden
    );

    let approved = crate::runtime::runtime_subagent_spawn_request(
        params,
        mez_agent::SubagentApprovalProvenance::ExplicitUserApproval,
    )
    .unwrap();
    assert!(approved.explicit_user_approval);
    approved.validate().unwrap();
}

/// Verifies a failure after the child is allocated reports accurate evidence.
///
/// The MAAP spawn action can allocate the child pane, scope declaration, and
/// turn and still fail during later finalization. The action result must then
/// report the existing child with a reconcile indication instead of the
/// contradicting no-child-created evidence, and it must not allocate a second
/// child.
#[test]
fn runtime_post_allocation_spawn_failure_reports_existing_child() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.configure_subagent_policy(4, 4, 2, 2, SubagentWaitPolicy::Detach);
    let started = service
        .start_agent_prompt_turn("%1", "delegate with a late failure")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = runtime_spawn_agent_action("spawn-post-allocation", "inspect the repository");
    let mut execution = runtime_spawn_execution_for_actions(&turn, vec![action]);
    let window_count = service.session().windows().len();
    let turn_count = service.agent_turn_ledger().turns().len();

    service.fail_next_subagent_spawn_after_allocation_for_tests();
    assert_eq!(
        service
            .execute_running_spawn_actions_for_turn(&turn, &mut execution)
            .unwrap(),
        1
    );

    let failed = &execution.action_results[0];
    assert_eq!(failed.status, ActionStatus::Failed);
    let structured = failed.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""child_created":true"#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""reconcile":"existing_child""#),
        "{structured}"
    );
    assert!(
        !structured.contains(r#""child_created":false"#),
        "{structured}"
    );
    let reported_child =
        serde_json::from_str::<serde_json::Value>(structured).unwrap()["child_agent_id"]
            .as_str()
            .unwrap()
            .to_string();
    // The existing child is reconciled by identity rather than duplicated.
    assert_eq!(service.session().windows().len(), window_count + 1);
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count + 1);
    assert!(
        service
            .subagent_scope_declaration(&reported_child)
            .is_some()
    );
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    service.terminate_all_pane_processes().unwrap();
}
