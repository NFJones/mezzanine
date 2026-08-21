//! Runtime tests for agent scheduling behavior.

use super::*;
use crate::runtime::ActiveTurnSleepInhibition;

/// Verifies that runtime hook diagnostics use the same canonical event label as
/// hook audit records and hook configuration. This matters because blocked
/// action payloads and hook failure events are user-visible protocol surfaces
/// that automation can match exactly.
#[test]
fn runtime_hook_event_name_uses_canonical_agent_turn_stop_label() {
    assert_eq!(
        runtime_hook_event_name(HookEvent::AgentTurnStop),
        "agent_turn_stop"
    );
}

/// Ensures every terminal agent-turn lifecycle state feeds the same turn-end
/// hook. This keeps user stops aligned with provider completion and failure so
/// configured cleanup hooks run regardless of how the turn ended.
#[test]
fn runtime_hook_lifecycle_maps_cancelled_turns_to_agent_turn_end() {
    assert_eq!(
        runtime_hook_event_for_lifecycle(
            EventKind::AgentStatus,
            r#"{"agent_prompt_turn":"turn-1","state":"completed"}"#,
        ),
        Some(HookEvent::AgentTurnStop)
    );
    assert_eq!(
        runtime_hook_event_for_lifecycle(
            EventKind::AgentStatus,
            r#"{"agent_prompt_turn":"turn-2","state":"failed"}"#,
        ),
        Some(HookEvent::AgentTurnStop)
    );
    assert_eq!(
        runtime_hook_event_for_lifecycle(
            EventKind::AgentStatus,
            r#"{"agent_prompt_turn":"turn-3","state":"cancelled"}"#,
        ),
        Some(HookEvent::AgentTurnStop)
    );
}

/// Verifies runtime owns agent turn start and finish lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_owns_agent_turn_start_and_finish_lifecycle() {
    let mut service = test_runtime_service();
    service.install_test_active_turn_power_inhibition_backend();
    service.set_active_turn_sleep_inhibition(ActiveTurnSleepInhibition::System);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();

    let started = service
        .start_agent_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            deadline_at_unix_millis: 0,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: mez_agent::AgentTurnState::Queued,

            initial_capability: None,
        })
        .unwrap();
    assert_eq!(started.running_turn_id.as_deref(), Some("turn-1"));
    assert_eq!(
        service.active_turn_power_inhibition_state_for_tests(),
        crate::host::power_inhibition::PowerInhibitionState::System
    );

    let agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(agents.contains(r#""status":"running""#), "{agents}");
    assert!(agents.contains(r#""last_turn_id":"turn-1""#), "{agents}");

    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");

    let session_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"session-tasks","method":"agent/task/list","params":{"target":{"default":true}}}"#,
        &primary,
    );
    assert!(
        session_tasks.contains(r#""id":"turn-1""#),
        "{session_tasks}"
    );

    let conflicting_target_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"conflicting-tasks","method":"agent/task/list","params":{"target":{"agent_id":"agent-%1","pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(
        conflicting_target_tasks.contains(r#""mezzanine_code":"invalid_params""#),
        "{conflicting_target_tasks}"
    );

    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let finished = service
        .finish_agent_turn("%1", "turn-1", mez_agent::AgentTurnState::Completed)
        .unwrap();
    assert_eq!(finished.running_turn_id, None);
    assert_eq!(finished.visibility, AgentShellVisibility::Hidden);
    assert_eq!(
        service.active_turn_power_inhibition_state_for_tests(),
        crate::host::power_inhibition::PowerInhibitionState::Inactive
    );

    let completed_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks2","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(
        completed_tasks.contains(r#""state":"completed""#),
        "{completed_tasks}"
    );
}

/// Verifies forced runtime shutdown releases the daemon-wide power inhibitor
/// even when a canonical agent turn is still running. The deterministic
/// backend ensures this lifecycle regression never changes host power state.
#[test]
fn runtime_shutdown_releases_active_turn_power_inhibition() {
    let mut service = test_runtime_service();
    service.install_test_active_turn_power_inhibition_backend();
    service.set_active_turn_sleep_inhibition(ActiveTurnSleepInhibition::System);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .start_agent_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-shutdown-power".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            deadline_at_unix_millis: 0,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: mez_agent::AgentTurnState::Queued,
            initial_capability: None,
        })
        .unwrap();

    assert_eq!(
        service.active_turn_power_inhibition_state_for_tests(),
        crate::host::power_inhibition::PowerInhibitionState::System
    );
    service.kill_session(&primary, true).unwrap();
    assert_eq!(
        service.active_turn_power_inhibition_state_for_tests(),
        crate::host::power_inhibition::PowerInhibitionState::Inactive
    );
}

/// Verifies background agent completion attention follows visible frame
/// hierarchy and is acknowledged only when the completed pane gains focus.
///
/// The marker must prefer a visible pane title, fall back to the owning window
/// title when pane frames are disabled, remain pending when no title surface is
/// visible, and disappear after an explicit focus command selects the pane.
#[test]
fn runtime_background_completion_attention_projects_and_acknowledges_on_focus() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let background_pane = service.active_pane_id().unwrap();
    let focused_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    service
        .start_agent_turn(mez_agent::AgentTurnRecord {
            turn_id: "attention-turn".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: format!("agent-{background_pane}"),
            pane_id: background_pane.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            deadline_at_unix_millis: 0,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: mez_agent::AgentTurnState::Queued,
            initial_capability: None,
        })
        .unwrap();
    service
        .finish_agent_turn(
            &background_pane,
            "attention-turn",
            mez_agent::AgentTurnState::Completed,
        )
        .unwrap();

    let pane_context = service.terminal_frame_context();
    assert!(
        pane_context
            .panes
            .get(&background_pane)
            .is_some_and(|pane| pane.completion_attention)
    );
    assert!(pane_context.animation_tick_ms > 0);

    service.set_frame_visibility_for_tests(true, false);
    let window_context = service.terminal_frame_context();
    assert!(window_context.windows[0].completion_attention);
    assert!(
        window_context
            .panes
            .get(&background_pane)
            .is_some_and(|pane| !pane.completion_attention)
    );

    service.set_frame_visibility_for_tests(false, false);
    let hidden_context = service.terminal_frame_context();
    assert!(
        hidden_context
            .windows
            .iter()
            .all(|window| !window.completion_attention)
    );
    assert_eq!(hidden_context.animation_tick_ms, 0);

    service.set_frame_visibility_for_tests(true, true);
    service
        .execute_terminal_command(&primary, &format!("select-pane -t {background_pane}"))
        .unwrap();
    assert_eq!(service.active_pane_id().unwrap(), background_pane);
    assert_ne!(service.active_pane_id().unwrap(), focused_pane.to_string());
    let acknowledged_context = service.terminal_frame_context();
    assert!(
        acknowledged_context
            .panes
            .values()
            .all(|pane| !pane.completion_attention)
    );
    assert!(
        acknowledged_context
            .windows
            .iter()
            .all(|window| !window.completion_attention)
    );
}

/// Verifies JSON-RPC hook clients can explicitly set and clear a pane's
/// completion-attention pill without changing pane focus.
///
/// The control accepts standard pane targets, projects attention through the
/// existing presentation hierarchy, rejects malformed boolean state, and
/// leaves the target pane unfocused throughout the request sequence.
#[test]
fn runtime_pane_attention_control_sets_clears_and_validates_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let background_pane = service.active_pane_id().unwrap();
    let focused_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let set_response = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"attention-set","method":"pane/attention","params":{{"target":{{"pane_id":"{background_pane}"}},"attention":true,"idempotency_key":"attention-set"}}}}"#
        ),
        &primary,
    );
    assert!(
        set_response.contains(&format!(
            r#""result":{{"pane_id":"{background_pane}","attention":true}}"#
        )),
        "{set_response}"
    );
    assert_eq!(service.active_pane_id().unwrap(), focused_pane.to_string());
    assert!(
        service
            .terminal_frame_context()
            .panes
            .get(&background_pane)
            .is_some_and(|pane| pane.completion_attention)
    );

    let clear_response = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"attention-clear","method":"pane/attention","params":{{"pane_id":"{background_pane}","attention":false,"idempotency_key":"attention-clear"}}}}"#
        ),
        &primary,
    );
    assert!(
        clear_response.contains(&format!(
            r#""result":{{"pane_id":"{background_pane}","attention":false}}"#
        )),
        "{clear_response}"
    );
    assert!(
        service
            .terminal_frame_context()
            .panes
            .values()
            .all(|pane| !pane.completion_attention)
    );

    let invalid_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"attention-invalid","method":"pane/attention","params":{"attention":"yes","idempotency_key":"attention-invalid"}}"#,
        &primary,
    );
    assert!(
        invalid_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_response}"
    );
    assert!(
        invalid_response.contains("pane/attention requires attention to be a boolean"),
        "{invalid_response}"
    );
}

/// Verifies pending approvals project attention through the visible pane,
/// window, and group title hierarchy and disappear after a decision.
///
/// Approval attention is derived from the live pending queue rather than turn
/// completion state, so resolving the request must clear every projected pill.
#[test]
fn runtime_pending_approval_attention_projects_and_clears_on_decision() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let approval_pane = service.active_pane_id().unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: format!("agent-{approval_pane}"),
            pane_id: approval_pane.clone(),
            parent_agent_chain: vec![format!("agent-{approval_pane}")],
            action_kind: "shell_command".to_string(),
            action_summary: "cargo test".to_string(),
            declared_effects: vec!["process_control".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: mez_agent::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();

    let pane_context = service.terminal_frame_context();
    assert!(
        pane_context
            .approval_attention_panes
            .contains(&approval_pane)
    );
    assert!(pane_context.animation_tick_ms > 0);

    service.set_frame_visibility_for_tests(true, false);
    let window_context = service.terminal_frame_context();
    assert!(window_context.approval_attention_panes.is_empty());
    assert_eq!(window_context.approval_attention_windows.len(), 1);

    service.session.new_group(&primary, "other", true).unwrap();
    let group_context = service.terminal_frame_context();
    assert!(group_context.approval_attention_windows.is_empty());
    assert_eq!(group_context.approval_attention_groups.len(), 1);

    service
        .integration
        .blocked_approvals_mut()
        .decide_at(
            &approval_id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            10,
        )
        .unwrap();
    let decided_context = service.terminal_frame_context();
    assert!(decided_context.approval_attention_panes.is_empty());
    assert!(decided_context.approval_attention_windows.is_empty());
    assert!(decided_context.approval_attention_groups.is_empty());
    service.kill_session(&primary, true).unwrap();
}

/// Verifies a background subagent completion does not flash a pane title pill.
///
/// Child turns report their result through their parent action, so highlighting
/// their own pane would create completion noise without a user-owned turn
/// reaching a terminal outcome.
#[test]
fn runtime_background_subagent_completion_does_not_register_attention() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let background_pane = service.active_pane_id().unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    service
        .start_agent_turn(mez_agent::AgentTurnRecord {
            turn_id: "child-attention-turn".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: format!("agent-{background_pane}"),
            pane_id: background_pane.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            deadline_at_unix_millis: 0,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: Some("root-attention-turn".to_string()),
            cooperation_mode: None,
            state: mez_agent::AgentTurnState::Queued,
            initial_capability: None,
        })
        .unwrap();
    service
        .finish_agent_turn(
            &background_pane,
            "child-attention-turn",
            mez_agent::AgentTurnState::Completed,
        )
        .unwrap();

    let pane_context = service.terminal_frame_context();
    assert!(
        pane_context
            .panes
            .get(&background_pane)
            .is_some_and(|pane| !pane.completion_attention)
    );
    assert_eq!(pane_context.animation_tick_ms, 0);
}

/// Verifies that the pane renderer blocks shell prompt repaint bytes while an
/// agent turn is running, even when no shell transaction is currently active.
/// Provider iteration can leave the pane between command result handling and
/// the next model response; default and debug views must not show PS1 content
/// during that gap.
#[test]
fn runtime_running_agent_turn_hides_shell_prompt_repaints_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let rendered = service
        .renderable_pane_output_bytes("%1", b"\x1b[38;2;214;93;14muser@host\x1b[0m ~/repo $ ");

    assert!(rendered.is_empty());
}

/// Verifies that `/log-level verbose` remains the explicit mode where shell
/// output is visible during a running agent turn. The hidden default must not
/// make verbose unusable for users who intentionally opted into command output.
#[test]
fn runtime_running_agent_turn_shell_prompt_is_visible_with_verbose_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let rendered = service.renderable_pane_output_bytes("%1", b"user@host ~/repo $ ");

    assert_eq!(rendered, b"user@host ~/repo $ ");
}

/// Verifies runtime config reload applies agent scheduler limit.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_reload_applies_agent_scheduler_limit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-scheduler-reload");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\nmax_concurrent_agents = 2\n").unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "turn-1".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: Some("%1".to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "turn-2".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: "agent-2".to_string(),
            pane_id: Some("%2".to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })
        .unwrap();
    assert_eq!(
        service.agent_scheduler_mut().start_ready().unwrap().turn_id,
        "turn-1"
    );
    assert_eq!(
        service.agent_scheduler_mut().start_ready().unwrap().turn_id,
        "turn-2"
    );

    fs::write(&path, "[agents]\nmax_concurrent_agents = 1\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-scheduler-limit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    let snapshot = service.agent_scheduler().snapshot();
    assert_eq!(snapshot.max_concurrent_agents, 1);
    assert_eq!(snapshot.running, 2);
    assert!(service.agent_scheduler_mut().start_ready().is_none());
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies terminal turn cleanup feeds settlement into the provider-independent
/// provider retry reducer, so a cancelled turn cannot remain reachable only
/// through a stale actor timer.
#[test]
fn runtime_turn_settlement_clears_provider_retry_scheduler_state() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn = service
        .start_agent_prompt_turn("%1", "inspect retry cleanup")
        .unwrap();
    assert!(matches!(
        service.provider_retry_scheduler_mut().apply(
            mez_agent::ProviderRetryEvent::FailureObserved {
                turn_id: turn.turn_id.clone(),
                retry_class: mez_agent::ProviderErrorRetryClass::RetryableTransport,
            }
        ),
        mez_agent::ProviderRetryTransition::Effect(mez_agent::ProviderRetryEffect::Recover { .. })
    ));
    assert_eq!(service.agent_provider_retry_turn_ids().count(), 1);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Interrupted)
        .unwrap();

    assert_eq!(service.agent_provider_retry_turn_ids().count(), 0);
}

/// Verifies that a live config reload starts queued agent work when the new
/// scheduler limit makes that work runnable. Updating the limit without
/// draining newly available scheduler capacity would leave prompt turns queued
/// until some unrelated turn completion nudged the scheduler.
#[test]
fn runtime_config_reload_starts_newly_runnable_agent_turns() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-scheduler-reload-start-ready");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\nmax_concurrent_agents = 1\n").unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    for pane_id in ["%1", second_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }

    let first = service.start_agent_prompt_turn("%1", "first").unwrap();
    let second = service
        .start_agent_prompt_turn(second_pane.as_str(), "second")
        .unwrap();
    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(second.state, AgentTurnState::Queued);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);

    fs::write(&path, "[agents]\nmax_concurrent_agents = 2\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-scheduler-start-ready"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(service.agent_scheduler().snapshot().running, 2);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-2")
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-2"),
    );
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies scheduler-delayed work cannot start after `/new` replaces the
/// conversation that originally owned the queued turn.
#[test]
fn runtime_queued_turn_is_settled_after_conversation_rebind() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane_id in ["%1", second_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }

    let first = service.start_agent_prompt_turn("%1", "first").unwrap();
    let second = service
        .start_agent_prompt_turn(second_pane.as_str(), "second")
        .unwrap();
    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(second.state, AgentTurnState::Queued);
    let original_conversation = service
        .agent_shell_store()
        .get(second_pane.as_str())
        .unwrap()
        .session_id
        .clone();

    service
        .session
        .select_pane(&primary, second_pane.as_str())
        .unwrap();
    let rebound = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(rebound.contains("new=true"), "{rebound}");
    let replacement_conversation = service
        .agent_shell_store()
        .get(second_pane.as_str())
        .unwrap()
        .session_id
        .clone();
    assert_ne!(replacement_conversation, original_conversation);

    let first_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == first.turn_id)
        .cloned()
        .unwrap();
    service
        .complete_running_agent_turn_and_start_ready(
            &first_turn,
            AgentTurnState::Completed,
            "test_capacity_released",
        )
        .unwrap();

    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == second.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );
    assert!(!service.agent_provider_task_is_owned(&second.turn_id));
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .map(|session| session.session_id.as_str()),
        Some(replacement_conversation.as_str())
    );
    service.kill_session(&primary, true).unwrap();
}

/// Verifies that stopping a queued pane-local agent turn does not depend on the
/// pane shell store having that queued turn as the active running turn. This
/// covers the queued cleanup path used when global scheduler capacity is full.
#[test]
fn runtime_stop_agent_turn_cleans_up_queued_turn_without_shell_running_marker() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane_id in ["%1", second_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }

    let first = service.start_agent_prompt_turn("%1", "first").unwrap();
    let second = service
        .start_agent_prompt_turn(second_pane.as_str(), "second")
        .unwrap();
    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(second.state, AgentTurnState::Queued);

    let stopped = service
        .stop_agent_turn_for_pane(second_pane.as_str())
        .unwrap();

    assert_eq!(stopped.turn_id, "turn-2");
    assert!(stopped.scheduler_cancelled);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    service.kill_session(&primary, true).unwrap();
}

/// Verifies that hiding a visible agent shell through terminal command routing
/// stops the in-progress turn before returning control to the pane.
#[test]
fn runtime_terminal_command_hides_running_agent_shell_after_task_completion() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-hide-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(!service.agent_turn_is_running("turn-1"));

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies active-turn provider continuations do not run fallback context
/// accounting before request assembly.
///
/// Runtime-owned action results and steering can append context after the turn
/// has started. The continuation path should still send the exact assembled
/// request first and rely on provider context-limit recovery if the provider
/// rejects it.
#[test]
fn runtime_agent_turn_sends_active_context_before_provider_limit_feedback() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-active-turn-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "compact-active-turn-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.compact-active-turn-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 64000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-active-turn-compact","input":"continue with gathered evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    insert_test_context_block(
        service.agent_turn_contexts_mut().get_mut("turn-1").unwrap(),
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synthetic in-turn action result".to_string(),
            content: format!(
                "turn-context-pressure- {}",
                "context-pressure ".repeat(10_000)
            ),
        },
    );
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeRecordingProvider {
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
            &provider,
            service
                .provider_registry()
                .resolve_profile("compact-active-turn-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let request = provider.last_request.borrow().clone().unwrap();
    let request_text = request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_text.contains("[synthetic in-turn action result]"),
        "{request_text}"
    );
    assert!(
        request_text.contains("turn-context-pressure-"),
        "{request_text}"
    );
    assert!(
        !request_text.contains("[context compacted]"),
        "{request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("agent: compacted active turn context"),
        "{pane_text}"
    );
}

/// Verifies runtime treats a same-pane prompt submitted mid-turn as steering.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_prompt_during_running_turn_becomes_steering_context() {
    let mut service = test_runtime_service();
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

    let first = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-1","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-1","input":"first prompt"}}"#,
        &primary,
    );
    assert!(first.contains(r#""state":"running""#), "{first}");
    {
        let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
        let group_1 = mez_agent::ContextExecutionGroupId::new("turn-1:test-group-1").unwrap();
        context
            .append_assistant_event("assistant action 1", "inspect action", group_1.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "result 1",
                "inspection evidence",
                group_1,
                None,
                true,
            )
            .unwrap();
        let group_2 = mez_agent::ContextExecutionGroupId::new("turn-1:test-group-2").unwrap();
        context
            .append_assistant_event("assistant action 2", "edit action", group_2.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "result 2",
                "edit evidence",
                group_2,
                None,
                true,
            )
            .unwrap();
        context.validate_durable().unwrap();
    }
    let second = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-2","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-2","input":"second prompt"}}"#,
        &primary,
    );
    assert!(second.contains(r#""kind":"mutated""#), "{second}");
    assert!(second.contains(r#""command":"prompt""#), "{second}");
    assert!(second.contains("injected_user_input=true"), "{second}");
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert!(
        service
            .agent_turn_contexts()
            .get("turn-1")
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::UserInstruction
                && block.label.starts_with("user steering ")
                && block.content == "second prompt")
    );
    {
        let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
        let group_3 = mez_agent::ContextExecutionGroupId::new("turn-1:test-group-3").unwrap();
        context
            .append_assistant_event("assistant action 3", "spec action", group_3.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "result 3",
                "spec evidence",
                group_3,
                None,
                true,
            )
            .unwrap();
        context.validate_durable().unwrap();
    }
    let provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: runtime_say_response("turn-1", "Acknowledged.", true),
        last_request: RefCell::new(None),
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let request = provider.last_request.borrow().clone().unwrap();
    let request_context = request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_context.contains("second prompt"),
        "{request_context}"
    );
    assert!(
        request_context.contains("[user steering 1]\nsecond prompt"),
        "{request_context}"
    );
    assert!(!request_context.contains("submitted_at_unix_seconds"));
    assert_eq!(request_context.matches("first prompt").count(), 1);
    assert_eq!(request_context.matches("second prompt").count(), 1);
    let ordered_markers = [
        "first prompt",
        "inspect action",
        "inspection evidence",
        "edit action",
        "edit evidence",
        "second prompt",
        "spec action",
        "spec evidence",
    ];
    let positions = ordered_markers
        .iter()
        .map(|marker| {
            request
                .messages
                .iter()
                .position(|message| message.content.contains(marker))
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    let steering_index = positions[5];
    assert_eq!(
        request.messages[steering_index].source,
        ContextSourceKind::UserInstruction
    );
    assert_eq!(
        request.messages[steering_index].placement,
        mez_agent::ContextPlacement::ConversationAppend
    );
    assert!(
        !service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-2")
    );
}

/// Verifies that the live runtime scheduler applies the starvation-bound
/// fairness rule after a running turn finishes: a queued runnable turn from a
/// different agent starts before a same-agent follow-up when capacity is one.
#[test]
fn runtime_scheduler_prefers_other_runnable_agent_after_completion() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane2 = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane in ["%1", pane2.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane.to_string(), screen);
    }

    service.start_agent_prompt_turn("%1", "first").unwrap();
    service.start_agent_prompt_turn("%1", "second").unwrap();
    service
        .start_agent_prompt_turn(pane2.as_str(), "third")
        .unwrap();
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 2);

    service.agent_scheduler_mut().complete("turn-1").unwrap();
    service
        .finish_agent_turn("%1", "turn-1", AgentTurnState::Completed)
        .unwrap();

    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-3"]
    );
    assert_eq!(
        service
            .agent_scheduler()
            .queued_turns()
            .map(|queued| queued.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-2"]
    );
}

/// Verifies joined child completion drains the scheduler and fairly resumes
/// the parent when joined work is queued behind a one-slot limit.
///
/// A blocked parent releases its global scheduler slot while it waits for
/// joined subagents. When the first running child finishes, the next queued
/// child must start immediately so the parent is not left waiting for a child
/// turn that is ready but never launched. After the last child completes, the
/// parent must reacquire the slot before its provider continuation is queued.
#[test]
fn runtime_joined_child_completion_starts_next_queued_child() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    let child_one_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let child_two_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Horizontal)
        .unwrap();
    for pane in ["%1", child_one_pane.as_str(), child_two_pane.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane.to_string(), screen);
    }

    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let child_one = service
        .start_agent_prompt_turn(child_one_pane.as_str(), "child one")
        .unwrap();
    let child_two = service
        .start_agent_prompt_turn(child_two_pane.as_str(), "child two")
        .unwrap();
    service.set_subagent_lineage(
        child_one.agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: parent.agent_id.clone(),
            root_agent_id: parent.agent_id.clone(),
            depth: 1,
            display_name: "child one".to_string(),
        },
    );
    service.set_subagent_lineage(
        child_two.agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: parent.agent_id.clone(),
            root_agent_id: parent.agent_id.clone(),
            depth: 1,
            display_name: "child two".to_string(),
        },
    );
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn_one = runtime_spawn_agent_action("spawn-one", "child one");
    let spawn_two = runtime_spawn_agent_action("spawn-two", "child two");
    service.agent_turn_executions_mut().insert(
        parent.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "spawn children".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: parent.turn_id.clone(),
                    agent_id: parent.agent_id.clone(),
                    actions: vec![spawn_one.clone(), spawn_two.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![
                mez_agent::ActionResult::running(
                    &parent_turn,
                    &spawn_one,
                    vec!["waiting for child one".to_string()],
                    None,
                ),
                mez_agent::ActionResult::running(
                    &parent_turn,
                    &spawn_two,
                    vec!["waiting for child two".to_string()],
                    None,
                ),
            ],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    let parent_execution = service
        .agent_turn_executions()
        .get(&parent.turn_id)
        .unwrap()
        .clone();
    service
        .append_agent_execution_chronology(&parent_turn, &parent_execution)
        .unwrap();
    service.insert_joined_subagent_dependency(
        child_one.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-one".to_string(),
            child_turn_id: child_one.turn_id.clone(),
            child_agent_id: child_one.agent_id.clone(),
            child_display_name: Some("child one".to_string()),
        },
    );
    service.insert_joined_subagent_dependency(
        child_two.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-two".to_string(),
            child_turn_id: child_two.turn_id.clone(),
            child_agent_id: child_two.agent_id.clone(),
            child_display_name: Some("child two".to_string()),
        },
    );
    service.remove_pending_agent_provider_task(&parent.turn_id);
    service
        .agent_scheduler_mut()
        .wait_running(&parent.turn_id)
        .unwrap();
    service
        .agent_turn_ledger_mut()
        .finish_turn(&parent.turn_id, AgentTurnState::Blocked)
        .unwrap();
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    assert_eq!(service.agent_scheduler().snapshot().active_capacity_used, 0);
    service.start_ready_agent_turns().unwrap();
    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_one.turn_id.as_str()]
    );
    assert_eq!(
        service
            .agent_scheduler()
            .queued_turns()
            .map(|queued| queued.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_two.turn_id.as_str()]
    );

    let child_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child_one.turn_id,
            &child_one.agent_id,
            "child one done",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &child_one.turn_id,
            &child_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_two.turn_id.as_str()]
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert!(!service.has_joined_subagent_dependency(&child_one.turn_id));
    assert!(service.has_joined_subagent_dependency(&child_two.turn_id));
    let parent_context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert!(
        parent_context
            .blocks()
            .iter()
            .any(|block| block.content.contains("child one done"))
    );
    assert!(
        !parent_context
            .blocks()
            .iter()
            .any(|block| block.content.contains("child two done"))
    );
    assert!(
        service
            .agent_shell_store()
            .get(child_one_pane.as_str())
            .is_none()
    );
    assert!(
        service
            .find_pane_descriptor(child_one_pane.as_str())
            .is_none()
    );
    assert!(!service.has_subagent_authority_state(&child_one.agent_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );

    let child_two_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child_two.turn_id,
            &child_two.agent_id,
            "child two done",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &child_two.turn_id,
            &child_two_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert!(!service.has_joined_subagent_dependency(&child_two.turn_id));
    assert_eq!(service.agent_scheduler().snapshot().waiting, 0);
    assert_eq!(service.agent_scheduler().snapshot().reacquiring, 0);
    assert_eq!(service.agent_scheduler().snapshot().active_capacity_used, 1);
    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![parent.turn_id.as_str()]
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent.turn_id)
    );
    let parent_context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert!(
        parent_context
            .blocks()
            .iter()
            .any(|block| block.content.contains("child two done"))
    );
    assert!(
        service
            .agent_shell_store()
            .get(child_two_pane.as_str())
            .is_none()
    );
    assert!(
        service
            .find_pane_descriptor(child_two_pane.as_str())
            .is_none()
    );
    assert!(!service.has_subagent_authority_state(&child_two.agent_id));
    let child_two_result_blocks = parent_context
        .blocks()
        .iter()
        .filter(|block| block.content.contains(&child_two.turn_id))
        .count();
    let terminal_child_two = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == child_two.turn_id)
        .cloned()
        .unwrap();
    service
        .emit_subagent_task_result_for_state(&terminal_child_two, AgentTurnState::Completed)
        .unwrap();
    assert_eq!(
        service
            .agent_turn_contexts()
            .get(&parent.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .filter(|block| block.content.contains(&child_two.turn_id))
            .count(),
        child_two_result_blocks
    );
}

/// Verifies three runtime-owned nonrouted children release their joined parent.
///
/// This exercises the provider-produced spawn batch rather than installing
/// dependencies by hand. All three generated child turns must settle their
/// corresponding running action result, remove their join records, and queue
/// the blocked parent for provider continuation after the final completion.
#[tokio::test]
async fn runtime_three_nonrouted_subagents_release_waiting_parent() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(4)
        .unwrap();
    let _primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let spawn_one = runtime_spawn_agent_action("spawn-one", "child one");
    let spawn_two = runtime_spawn_agent_action("spawn-two", "child two");
    let spawn_three = runtime_spawn_agent_action("spawn-three", "child three");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn three children".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "delegate three joined tasks".to_string(),
                thought: None,
                turn_id: parent.turn_id.clone(),
                agent_id: parent.agent_id.clone(),
                actions: vec![spawn_one, spawn_two, spawn_three],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(service.joined_subagent_dependency_count(), 3);
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );
    let children = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| turn.turn_id != parent.turn_id)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 3, "{children:#?}");

    for (index, child) in children.iter().rev().enumerate() {
        let response = runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            &format!("child {} done", index + 1),
            true,
        );
        let action = response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.first())
            .cloned()
            .unwrap();
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&child.turn_id, &child.agent_id),
            response,
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::succeeded(
                child,
                &action,
                vec![format!("child {} done", index + 1)],
                None,
            )],
            final_turn: true,
            terminal_state: AgentTurnState::Completed,
        };
        assert!(
            service
                .apply_agent_provider_completed_event(
                    &AgentId::opaque(child.agent_id.clone()).unwrap(),
                    &child.turn_id,
                    execution,
                )
                .await
                .unwrap()
        );
    }

    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert_eq!(service.agent_scheduler().snapshot().waiting, 0);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent.turn_id)
    );
    assert_ne!(
        service
            .terminal_frame_context()
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_status.as_deref()),
        Some("waiting")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a failed nonrouted child remains context for its joined parent.
///
/// One failed child must not terminalize the parent or discard sibling work.
/// The parent remains blocked until every joined child settles, then resumes
/// with both the failed task result and successful sibling results as context.
#[tokio::test]
async fn runtime_failed_nonrouted_subagent_preserves_siblings_and_resumes_parent() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(5)
        .unwrap();
    let _primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let actions = (1..=4)
        .map(|index| {
            runtime_spawn_agent_action(&format!("spawn-{index}"), &format!("child {index}"))
        })
        .collect::<Vec<_>>();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn four children".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "delegate four joined tasks".to_string(),
                thought: None,
                turn_id: parent.turn_id.clone(),
                agent_id: parent.agent_id.clone(),
                actions,
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(service.joined_subagent_dependency_count(), 4);
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    let children = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| turn.turn_id != parent.turn_id)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 4, "{children:#?}");
    let failed_child = &children[0];

    service
        .complete_running_agent_turn_and_start_ready(
            failed_child,
            AgentTurnState::Failed,
            "nonrouted_child_failed",
        )
        .unwrap();

    assert_eq!(service.joined_subagent_dependency_count(), 3);
    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .all(|task| task.turn_id != parent.turn_id)
    );

    for (index, child) in children.iter().skip(1).enumerate() {
        let response = runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            &format!("sibling {} done", index + 1),
            true,
        );
        let action = response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.first())
            .cloned()
            .unwrap();
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&child.turn_id, &child.agent_id),
            response,
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::succeeded(
                child,
                &action,
                vec![format!("sibling {} done", index + 1)],
                None,
            )],
            final_turn: true,
            terminal_state: AgentTurnState::Completed,
        };
        assert!(
            service
                .apply_agent_provider_completed_event(
                    &AgentId::opaque(child.agent_id.clone()).unwrap(),
                    &child.turn_id,
                    execution,
                )
                .await
                .unwrap()
        );
    }

    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert_eq!(service.agent_scheduler().snapshot().waiting, 0);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent.turn_id)
    );
    let parent_context = service.agent_turn_contexts().get(&parent.turn_id).unwrap();
    assert!(parent_context.blocks().iter().any(|block| {
        block.content.contains(r#""success":false"#)
            && block.content.contains("subagent task failed")
    }));
    for index in 1..=3 {
        assert!(
            parent_context
                .blocks()
                .iter()
                .any(|block| { block.content.contains(&format!("sibling {index} done")) })
        );
    }
    assert_ne!(
        service
            .terminal_frame_context()
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_status.as_deref()),
        Some("waiting")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Blocks one running parent turn on a joined child result in lifecycle tests.
///
/// The helper installs the same running spawn result, chronology, routing, and
/// scheduler wait state used by runtime-owned joined delegation while allowing
/// nested tests to keep their pane setup compact and explicit.
fn block_turn_on_joined_child(
    service: &mut RuntimeSessionService,
    parent: &mez_agent::AgentTurnRecord,
    child: &mez_agent::AgentTurnRecord,
    action_id: &str,
    child_name: &str,
) {
    let spawn = runtime_spawn_agent_action(action_id, child_name);
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: format!("spawn {child_name}"),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "delegate joined work".to_string(),
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
            parent,
            &spawn,
            vec![format!("waiting for {child_name}")],
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .agent_turn_executions_mut()
        .insert(parent.turn_id.clone(), execution.clone());
    service
        .append_agent_execution_chronology(parent, &execution)
        .unwrap();
    service.insert_joined_subagent_dependency(
        child.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: spawn.id.clone(),
            child_turn_id: child.turn_id.clone(),
            child_agent_id: child.agent_id.clone(),
            child_display_name: Some(child_name.to_string()),
        },
    );
    service.set_subagent_task_parent(child.turn_id.clone(), parent.agent_id.clone());
    service.remove_pending_agent_provider_task(&parent.turn_id);
    service
        .agent_scheduler_mut()
        .wait_running(&parent.turn_id)
        .unwrap();
    service
        .agent_turn_ledger_mut()
        .finish_turn(&parent.turn_id, AgentTurnState::Blocked)
        .unwrap();
}

/// Verifies successful nested joined children close from the leaves upward.
///
/// A grandchild result must resume its immediate child parent and close only
/// the grandchild pane. When that intermediate child later completes, its own
/// result must reach the root and close the child pane without retaining any
/// joined routes, dependencies, or delegation authority for either descendant.
#[test]
fn runtime_nested_joined_children_close_after_each_successful_handoff() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(3)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    let child_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let grandchild_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Horizontal)
        .unwrap();
    for pane in ["%1", child_pane.as_str(), grandchild_pane.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane.to_string(), screen);
    }

    let root = service.start_agent_prompt_turn("%1", "root task").unwrap();
    let child = service
        .start_agent_prompt_turn(child_pane.as_str(), "child task")
        .unwrap();
    let grandchild = service
        .start_agent_prompt_turn(grandchild_pane.as_str(), "grandchild task")
        .unwrap();
    let records = service.agent_turn_ledger().turns();
    let root_record = records
        .iter()
        .find(|turn| turn.turn_id == root.turn_id)
        .cloned()
        .unwrap();
    let child_record = records
        .iter()
        .find(|turn| turn.turn_id == child.turn_id)
        .cloned()
        .unwrap();
    let grandchild_record = records
        .iter()
        .find(|turn| turn.turn_id == grandchild.turn_id)
        .cloned()
        .unwrap();
    service.set_subagent_lineage(
        child.agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: root.agent_id.clone(),
            root_agent_id: root.agent_id.clone(),
            depth: 1,
            display_name: "child".to_string(),
        },
    );
    service.set_subagent_lineage(
        grandchild.agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: child.agent_id.clone(),
            root_agent_id: root.agent_id.clone(),
            depth: 2,
            display_name: "grandchild".to_string(),
        },
    );
    block_turn_on_joined_child(
        &mut service,
        &root_record,
        &child_record,
        "spawn-child",
        "child",
    );
    block_turn_on_joined_child(
        &mut service,
        &child_record,
        &grandchild_record,
        "spawn-grandchild",
        "grandchild",
    );

    let grandchild_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &grandchild.turn_id,
            &grandchild.agent_id,
            "grandchild done",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &grandchild.turn_id,
            &grandchild_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert!(
        service
            .find_pane_descriptor(grandchild_pane.as_str())
            .is_none()
    );
    assert!(service.find_pane_descriptor(child_pane.as_str()).is_some());
    assert!(!service.has_subagent_authority_state(&grandchild.agent_id));
    assert!(service.has_subagent_authority_state(&child.agent_id));
    assert!(service.subagent_task_parent(&grandchild.turn_id).is_none());
    assert!(!service.has_joined_subagent_dependency(&grandchild.turn_id));
    assert!(
        service
            .agent_turn_contexts()
            .get(&child.turn_id)
            .is_some_and(|context| context
                .blocks()
                .iter()
                .any(|block| block.content.contains("grandchild done")))
    );

    let child_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            "child done",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &child_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert!(service.find_pane_descriptor(child_pane.as_str()).is_none());
    assert!(service.find_pane_descriptor("%1").is_some());
    assert!(!service.has_subagent_authority_state(&child.agent_id));
    assert!(service.subagent_task_parent(&child.turn_id).is_none());
    assert!(!service.has_joined_subagent_dependency(&child.turn_id));
    assert!(
        service
            .agent_turn_contexts()
            .get(&root.turn_id)
            .is_some_and(|context| context
                .blocks()
                .iter()
                .any(|block| block.content.contains("child done")))
    );
}
