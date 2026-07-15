//! Runtime tests for agent scheduling behavior.

use super::*;

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
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: mez_agent::AgentTurnState::Queued,

            initial_capability: None,
        })
        .unwrap();
    assert_eq!(started.running_turn_id.as_deref(), Some("turn-1"));

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

    let completed_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks2","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(
        completed_tasks.contains(r#""state":"completed""#),
        "{completed_tasks}"
    );
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
    service.pane_screens.insert(
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
    service.pane_screens.insert(
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
            agent_id: "agent-1".to_string(),
            pane_id: Some("%1".to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "turn-2".to_string(),
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
        service.pane_screens.insert(pane_id.to_string(), screen);
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
        service.pane_screens.insert(pane_id.to_string(), screen);
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
            .agent_turn_ledger
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
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic in-turn action result".to_string(),
            content: format!(
                "turn-context-pressure- {}",
                "context-pressure ".repeat(10_000)
            ),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
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
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let first = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-1","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-1","input":"first prompt"}}"#,
        &primary,
    );
    assert!(first.contains(r#""state":"running""#), "{first}");
    let second = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-2","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-2","input":"second prompt"}}"#,
        &primary,
    );
    assert!(second.contains(r#""kind":"mutated""#), "{second}");
    assert!(second.contains(r#""command":"prompt""#), "{second}");
    assert!(second.contains("injected_user_input=true"), "{second}");
    assert_eq!(service.agent_turn_ledger.turns().len(), 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
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
        request_context.contains("[user steering input during active turn]"),
        "{request_context}"
    );
    assert!(
        !service
            .agent_turn_ledger
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
        service.pane_screens.insert(pane.to_string(), screen);
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

/// Verifies joined child completion drains the scheduler when other joined
/// children are queued behind a low concurrency limit.
///
/// A blocked parent releases its global scheduler slot while it waits for
/// joined subagents. When the first running child finishes, the next queued
/// child must start immediately so the parent is not left waiting for a child
/// turn that is ready but never launched.
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
        service.pane_screens.insert(pane.to_string(), screen);
    }

    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let child_one = service
        .start_agent_prompt_turn(child_one_pane.as_str(), "child one")
        .unwrap();
    let child_two = service
        .start_agent_prompt_turn(child_two_pane.as_str(), "child two")
        .unwrap();
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn_one = runtime_spawn_agent_action("spawn-one", "child one");
    let spawn_two = runtime_spawn_agent_action("spawn-two", "child two");
    service.agent_turn_executions.insert(
        parent.turn_id.clone(),
        crate::agent::AgentTurnExecution {
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
    service.joined_subagent_dependencies.insert(
        child_one.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-one".to_string(),
            child_turn_id: child_one.turn_id.clone(),
            child_agent_id: child_one.agent_id.clone(),
            child_display_name: Some("child one".to_string()),
        },
    );
    service.joined_subagent_dependencies.insert(
        child_two.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-two".to_string(),
            child_turn_id: child_two.turn_id.clone(),
            child_agent_id: child_two.agent_id.clone(),
            child_display_name: Some("child two".to_string()),
        },
    );
    service.pending_agent_provider_tasks.remove(&parent.turn_id);
    service
        .agent_scheduler_mut()
        .complete(&parent.turn_id)
        .unwrap();
    service
        .agent_turn_ledger
        .finish_turn(&parent.turn_id, AgentTurnState::Blocked)
        .unwrap();
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
    assert!(
        !service
            .joined_subagent_dependencies
            .contains_key(&child_one.turn_id)
    );
    assert!(
        service
            .joined_subagent_dependencies
            .contains_key(&child_two.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );
}
