//! Agent shell state tests.

use super::*;

/// Verifies runtime control agent shell state persists in service.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_agent_shell_state_persists_in_service() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let show = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let conversation_id = service
        .agent_shell_store
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        show.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{show}"
    );

    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""pane_id":"%1""#), "{list}");
    assert!(list.contains(r#""visible":true"#), "{list}");
    assert!(
        list.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{list}"
    );

    let targeted_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"targeted-list","method":"agent/list","params":{"target":{"default":true}}}"#,
        &primary,
    );
    assert!(
        targeted_list.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{targeted_list}"
    );

    let missing_session_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"missing-list","method":"agent/list","params":{"target":{"session_id":"missing"}}}"#,
        &primary,
    );
    assert!(
        missing_session_list.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session_list}"
    );

    let hide = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"hide","method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");

    let relist = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"relist","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(relist.contains(r#""visible":false"#), "{relist}");
    assert!(
        relist.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{relist}"
    );
}

/// Verifies that the JSON-RPC agent shell visibility endpoints apply the same
/// live pane subshell side effects as the terminal `agent-shell` command. This
/// protects clients that enter agent mode through control APIs from bypassing
/// the parent-shell isolation boundary.
#[test]
fn runtime_control_agent_shell_visibility_enters_and_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .pane_screen_mut(&pane_id)
        .unwrap()
        .feed(b"control show history\ncontrol show visible text");

    let show = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let after_show_screen = service.pane_screen(&pane_id).unwrap();
    assert!(
        !after_show_screen
            .visible_lines()
            .join("\n")
            .contains("control show visible text")
    );
    assert!(
        after_show_screen
            .normal_content_lines()
            .join("\n")
            .contains("control show visible text")
    );
    let enter_input = service.drain_pane_io_transition().side_effects;
    assert_eq!(pane_input_effects(&enter_input).len(), 1);
    assert_eq!(enter_input[0].pane_input_parts().0, pane_id);
    assert!(service.agent_subshell_is_active(&pane_id));

    let hide = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"hide","method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");
    let exit_effects = service.drain_pane_io_transition().side_effects;
    let exit_inputs = pane_input_effects(&exit_effects);
    assert_eq!(exit_inputs.len(), 1);
    assert_eq!(exit_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x04");
    assert!(!service.agent_subshell_is_active(&pane_id));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies that hidden agent-shell output uses a bounded Mezzanine-marker
/// scanner instead of feeding arbitrary command output into a terminal screen.
/// Long shell-command bodies can contain megabytes of plain text or embedded
/// terminal escapes; those bytes are model data and must not monopolize the UI
/// parser while the runtime waits for its own transaction marker.
#[test]
fn runtime_hidden_agent_shell_osc_parser_skips_large_command_bodies() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service
        .pane_transaction_osc_screens_mut_for_tests()
        .remove("%1");
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "read-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "head -c 1048577 -- src/lib.rs".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let mut output = vec![b'x'; 2 * 1024 * 1024];
    output.extend_from_slice(b"\x1b[?1049hignored alternate-screen bytes from file content\n");
    output.extend_from_slice(
        b"\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );

    let (events, alternate_active) = service
        .terminal_osc_events_for_pane_bytes("%1", size, &output)
        .unwrap();

    assert!(!alternate_active);
    assert_eq!(
        events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
    assert!(
        !service
            .pane_transaction_osc_screens_for_tests()
            .contains_key("%1"),
        "hidden agent shell output should not allocate or feed the full terminal parser"
    );
    assert!(
        !service
            .pane_transaction_osc_pending_for_tests()
            .contains_key("%1")
    );
}

/// Verifies the bounded hidden-output marker scanner still preserves
/// transaction markers split across PTY reads. This keeps the lightweight path
/// compatible with the real-world fragmentation that the full terminal parser
/// handled before hidden agent-shell output was bypassed.
#[test]
fn runtime_hidden_agent_shell_osc_parser_preserves_fragmented_markers() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service
        .pane_transaction_osc_screens_mut_for_tests()
        .remove("%1");
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "read-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "head -c 1048577 -- src/lib.rs".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let (first_events, _) = service
        .terminal_osc_events_for_pane_bytes(
            "%1",
            size,
            b"large body\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez",
        )
        .unwrap();
    let (second_events, _) = service
        .terminal_osc_events_for_pane_bytes("%1", size, b"_pane=%1\x1b\\")
        .unwrap();

    assert_eq!(first_events, Vec::<TerminalOscEvent>::new());
    assert_eq!(
        second_events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
    assert!(
        !service
            .pane_transaction_osc_pending_for_tests()
            .contains_key("%1")
    );
}

///
/// Shell-mode inheritance must land on the child pane before the subagent turn
/// begins so model-authored local actions use the same executor path as the
/// Verifies exiting a parent agent shell closes active child subagent panes.
///
/// Subagent panes are owned by the parent delegation tree. Leaving the parent
/// session should not leave child agents, write scopes, or panes behind as
/// orphaned runtime state.
#[test]
fn runtime_parent_agent_shell_exit_closes_child_subagent_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .execute_terminal_command(&primary, "split-window")
        .unwrap();
    let child_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .find(|pane| pane.id.as_str() != "%1")
        .map(|pane| pane.id.to_string())
        .expect("split-window should create a child pane");
    let child_agent_id = format!("agent-{child_pane_id}");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&child_pane_id)
        .unwrap();
    service.set_subagent_lineage(
        child_agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "helper".to_string(),
        },
    );

    service.request_agent_shell_exit_for_pane("%1").unwrap();
    assert!(
        service
            .session()
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .all(|pane| pane.id.as_str() != child_pane_id)
    );
    assert!(!service.has_subagent_lineage(&child_agent_id));
    assert!(service.agent_shell_store().get(&child_pane_id).is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that foreground pane input applied through the async deferred I/O
/// path clears retained agent-shell output filters before the pane process
/// echoes new user-owned bytes. Without this boundary reset, a delayed parent
/// prompt repaint can be reduced to a carriage return while the foreground
/// cursor remains visually placed after the old prompt, causing the next echoed
/// input to render at column zero.
#[test]
fn runtime_deferred_foreground_input_clears_agent_shell_output_filters() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.remember_hidden_shell_render_suppression("%1");
    service.remember_mez_wrapper_filter_command("%1", "MEZ_MARKER_TOKEN='abc'");

    let (report, deferred) = service
        .apply_attached_terminal_step_transition(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"a".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 1);
    let pane_inputs = pane_input_effects(&deferred.side_effects);
    assert_eq!(pane_inputs.len(), 1);
    assert_eq!(pane_inputs[0].pane_input_parts().0, "%1");
    assert_eq!(pane_inputs[0].pane_input_parts().1, b"a");
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let prompt_repaint = service.visible_pane_output_bytes("%1", b"\r$ ");
    assert_eq!(prompt_repaint, b"\r$ ");
}

/// Verifies that a visible pane agent shell publishes the active model profile,
/// reasoning profile, and idle status into pane frame context before any turn
/// has started. The default header relies on these fields for agent mode.
#[test]
fn runtime_frame_context_reports_visible_agent_shell_metadata() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.mode.as_deref(), Some("agent"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_status.as_deref(), Some("idle"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-work"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}
