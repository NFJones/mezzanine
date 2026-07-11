//! Runtime tests for agent shell behavior.

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
        .pane_screens
        .get_mut(&pane_id)
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
    let enter_input = service.drain_deferred_pane_inputs();
    assert_eq!(enter_input.len(), 1);
    assert_eq!(enter_input[0].pane_id, pane_id);
    assert!(service.agent_subshell_panes.contains(&pane_id));

    let hide = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"hide","method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
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
    service.pane_transaction_osc_screens.remove("%1");
    service.running_shell_transactions.insert(
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
        !service.pane_transaction_osc_screens.contains_key("%1"),
        "hidden agent shell output should not allocate or feed the full terminal parser"
    );
    assert!(!service.pane_transaction_osc_pending.contains_key("%1"));
}

/// Verifies the bounded hidden-output marker scanner still preserves
/// transaction markers split across PTY reads. This keeps the lightweight path
/// compatible with the real-world fragmentation that the full terminal parser
/// handled before hidden agent-shell output was bypassed.
#[test]
fn runtime_hidden_agent_shell_osc_parser_preserves_fragmented_markers() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service.pane_transaction_osc_screens.remove("%1");
    service.running_shell_transactions.insert(
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
    assert!(!service.pane_transaction_osc_pending.contains_key("%1"));
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
    service.subagent_lineage.insert(
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
    assert!(!service.subagent_lineage.contains_key(&child_agent_id));
    assert!(service.agent_shell_store().get(&child_pane_id).is_none());
    service.pane_processes_mut().terminate_all().unwrap();
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
        .apply_attached_terminal_step_plan_deferred_pane_io(
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
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].pane_id, "%1");
    assert_eq!(deferred[0].bytes, b"a");
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

/// Verifies agent-shell record browsers keep their typed browser state after
/// the Markdown display response opens the primary overlay.
///
/// `/show-issues` and `/show-memories` cross a JSON display-response boundary
/// before the terminal UI decides whether to open a modal pager. Retaining the
/// browser beside the rendered overlay is the prerequisite for later key-driven
/// filtering, detail navigation, and save prompts to act on structured browser
/// state instead of reparsing displayed Markdown.
#[test]
fn runtime_agent_shell_record_browser_display_retains_overlay_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let browser = crate::runtime::record_browser::RuntimeRecordBrowser::new(
        "Issues",
        vec![crate::runtime::record_browser::RuntimeRecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: Some("/show-issues issue-1".to_string()),
            title: "First issue".to_string(),
            metadata: vec![("kind".to_string(), "task".to_string())],
            markdown: "Body".to_string(),
        }],
        Vec::new(),
    )
    .unwrap();
    service
        .pending_record_browser_overlays
        .insert((pane_id.clone(), "show-issues".to_string()), browser);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: "# Issues\n\n- [`issue-1`](mez-agent:%2Fshow-issues%20issue-1)".to_string(),
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("record-browser display should open an overlay");
    let record_browser = overlay
        .record_browser
        .as_ref()
        .expect("overlay should retain record-browser state");
    assert_eq!(record_browser.pane_id, pane_id);
    assert_eq!(record_browser.command, "show-issues");
    assert_eq!(record_browser.browser.render_page().title, "Issues");
    assert!(service.pending_record_browser_overlays.is_empty());
}

/// Verifies `/show-issues` overlays expose record-browser footer help and keep
/// Enter routed through the focused Markdown selection.
///
/// The browser intercepts filter and save keys directly, but Select should
/// still fall through to the shared overlay selection path so the focused
/// record opens as a child detail view.
#[test]
fn runtime_agent_shell_record_browser_footer_and_enter_open_detail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 12).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-record-browser-footer-enter");
    let config_root = root.join("config");
    fs::create_dir_all(&config_root).unwrap();
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[issues]
enabled = true
"
            .to_string(),
        }])
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let project = crate::issues::project_key_for_working_directory(
        service
            .pane_current_working_directory(&pane_id)
            .unwrap_or_else(|| config_root.clone()),
    );
    let store = crate::issues::IssueStore::under_config_root(config_root.clone());
    store
        .add_issue(
            project.clone(),
            crate::issues::IssueKind::Defect,
            "Second issue".to_string(),
            Some("Second body".to_string()),
            None,
            1,
        )
        .unwrap();
    store
        .add_issue(
            project,
            crate::issues::IssueKind::Task,
            "First issue".to_string(),
            Some("First body".to_string()),
            None,
            2,
        )
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/show-issues")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    let overlay_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 12).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let footer = overlay_view.lines.last().cloned().unwrap_or_default();
    assert!(footer.contains("esc: back"), "{footer}");
    assert!(footer.contains("enter: open"), "{footer}");
    assert!(footer.contains("k/p/x: filter"), "{footer}");
    assert!(footer.contains("s: save"), "{footer}");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("Enter should keep the detail overlay open");
    let record_browser = overlay
        .record_browser
        .as_ref()
        .expect("detail overlay should retain record-browser state");
    assert_eq!(record_browser.command, "show-issues");
    assert_eq!(record_browser.browser.render_page().title, "First issue");
    assert!(overlay.lines.iter().any(|line| line.contains("First body")));
    let _ = fs::remove_dir_all(root);
}

/// Verifies Escape restores the parent record-browser list after a selected
/// record opens a child detail view.
///
/// The detail command crosses the agent-shell display response boundary, so
/// the parent browser and pager cursor must survive in the retained view stack
/// instead of being replaced permanently by the child overlay.
#[test]
fn runtime_agent_shell_record_browser_escape_restores_parent_view_stack() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let parent_browser = crate::runtime::record_browser::RuntimeRecordBrowser::new(
        "Issues",
        vec![
            crate::runtime::record_browser::RuntimeRecordBrowserRecord {
                id: "issue-1".to_string(),
                open_command: Some("/show-issues issue-1".to_string()),
                title: "First issue".to_string(),
                metadata: vec![("kind".to_string(), "task".to_string())],
                markdown: "First body".to_string(),
            },
            crate::runtime::record_browser::RuntimeRecordBrowserRecord {
                id: "issue-2".to_string(),
                open_command: Some("/show-issues issue-2".to_string()),
                title: "Second issue".to_string(),
                metadata: vec![("kind".to_string(), "defect".to_string())],
                markdown: "Second body".to_string(),
            },
        ],
        Vec::new(),
    )
    .unwrap();
    let mut child_browser = crate::runtime::record_browser::RuntimeRecordBrowser::new(
        "Issue detail",
        vec![crate::runtime::record_browser::RuntimeRecordBrowserRecord {
            id: "issue-1".to_string(),
            open_command: Some("/show-issues issue-1".to_string()),
            title: "First issue".to_string(),
            metadata: vec![("kind".to_string(), "task".to_string())],
            markdown: "First body".to_string(),
        }],
        Vec::new(),
    )
    .unwrap();
    child_browser.show_first_record_detail();
    let child_page = child_browser.render_page();
    service
        .pending_record_browser_overlays
        .insert((pane_id.clone(), "show-issues".to_string()), child_browser);
    service.pending_record_browser_overlay_stacks.insert(
        (pane_id.clone(), "show-issues".to_string()),
        vec![
            crate::runtime::service_state::RuntimeRecordBrowserOverlayFrame {
                command: "show-issues".to_string(),
                source: None,
                browser: parent_browser,
                scroll_offset: 0,
                active_selection_index: Some(1),
            },
        ],
    );
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        &pane_id,
        "/show-issues issue-1",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: child_page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .map(|record_browser| record_browser.browser.render_page().title),
        Some("First issue".to_string())
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("Escape should keep the restored parent overlay open");
    let record_browser = overlay
        .record_browser
        .as_ref()
        .expect("restored overlay should keep record-browser state");
    assert_eq!(record_browser.browser.render_page().title, "Issues");
    assert!(record_browser.stack.is_empty());
    assert_eq!(overlay.active_selection_index, Some(1));
    assert!(overlay.lines.iter().any(|line| line.contains("issue-2")));
}

/// Verifies runtime attached mux action toggles agent shell state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_attached_mux_action_toggles_agent_shell_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":true"#), "{list}");

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list2","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":false"#), "{list}");
}

/// Verifies that terminal command execution uses live runtime state for the
/// agent shell toggle instead of falling through to the offline no-op command
/// planner. This covers both show and hide transitions for the active pane and
/// verifies transition clears preserve prior visible content in pane history.
#[test]
fn runtime_terminal_command_toggles_agent_shell_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    screen.feed(b"history line\nvisible before agent");
    service.pane_screens.insert("%1".to_string(), screen);
    let history_before_enter = service.pane_screen("%1").unwrap().history().len();
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains(r#""command":"agent-shell""#), "{show}");
    assert!(show.contains(r#""kind":"display""#), "{show}");
    assert!(show.contains("pane=%1"), "{show}");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        show.contains(&format!("conversation_id={conversation_id}")),
        "{show}"
    );
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let after_enter_screen = service.pane_screen("%1").unwrap();
    assert!(after_enter_screen.history().len() > history_before_enter);
    assert!(
        !after_enter_screen
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
    assert!(
        after_enter_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible before agent")
    );
    let history_before_exit = after_enter_screen.history().len();
    service
        .pane_screens
        .get_mut("%1")
        .unwrap()
        .feed(b"visible inside agent");
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );

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
    let after_exit_screen = service.pane_screen("%1").unwrap();
    assert!(after_exit_screen.history().len() > history_before_exit);
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    let after_reentry_screen = service.pane_screen("%1").unwrap();
    assert!(
        !after_reentry_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent"),
        "agent reentry should start from a clean viewport, not scroll old agent logs back into view"
    );
    assert!(
        after_reentry_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );
}

/// Verifies that showing agent mode starts a pane-local subshell and hiding it
/// exits that subshell instead of sending redraw traffic to the user's original
/// interactive shell. This protects prompt, option, and environment mutations
/// made by agent commands from leaking back to the parent shell, and confirms
/// that retained hidden-render suppression is cleared so the parent prompt
/// repaint can advance the terminal cursor to the end of the prompt line.
#[test]
fn runtime_agent_shell_toggle_enters_and_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
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

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let enter_input = service.drain_deferred_pane_inputs();
    assert_eq!(enter_input.len(), 1);
    assert_eq!(enter_input[0].pane_id, pane_id);
    let enter_text = String::from_utf8_lossy(&enter_input[0].bytes);
    assert!(
        enter_text.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{enter_text}"
    );
    assert!(enter_text.contains("HISTFILE=/dev/null"), "{enter_text}");
    assert!(enter_text.contains("'/bin/sh'"), "{enter_text}");
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service.remember_mez_wrapper_filter_command(&pane_id, "MEZ_MARKER_TOKEN='abc'");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let simple_prompt_repaint = service.visible_pane_output_bytes(&pane_id, b"\r$ ");
    assert_eq!(simple_prompt_repaint, b"\r$ ");
    let prompt_repaint = service.renderable_pane_output_bytes(&pane_id, b"user@host ~/repo $ ");
    assert_eq!(prompt_repaint, b"user@host ~/repo $ ");
    service
        .apply_pane_output_bytes(pane_id.clone(), b"user@host ~/repo $ ".to_vec())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig {
                window_frames_enabled: false,
                pane_frames_enabled: false,
                ..TerminalClientLoopConfig::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.cursor_column, "user@host ~/repo $ ".chars().count());
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies that the live subshell EOF path also restores the parent prompt
/// cursor after agent-authored text has already moved the pane screen. This
/// covers the Ctrl+D path that exits the child agent shell, waits for the parent
/// shell prompt to repaint, and then presents the attached terminal cursor.
#[test]
fn runtime_agent_shell_ctrl_d_after_agent_output_restores_live_parent_cursor() {
    let shell_path = PathBuf::from("/bin/sh");
    let shell_available = fs::metadata(&shell_path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !shell_available {
        eprintln!("skipping live cursor regression because /bin/sh is unavailable");
        return;
    }
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(shell_path.clone(), ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some(
            "/bin/sh -c 'PS1=\"parent$ \"; export PS1; exec /bin/sh -i'",
        ))
        .unwrap();
    let mut initial_screen = String::new();
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        initial_screen = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        if initial_screen.contains("parent$") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        initial_screen.contains("parent$"),
        "parent prompt did not arrive: {initial_screen:?}"
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "done")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x04".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.full_redraw_required);

    let prompt_column = "parent$ ".chars().count();
    let mut cursor_column = None;
    let mut observed_cursor = None;
    let mut observed_screen = String::new();
    for _ in 0..300 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let cursor = service.pane_screen("%1").unwrap().cursor_state();
        let screen_text = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        observed_cursor = Some(cursor);
        observed_screen = screen_text.clone();
        if screen_text.contains("parent$") && cursor.column == prompt_column {
            cursor_column = Some(cursor.column);
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    assert_eq!(
        cursor_column,
        Some(prompt_column),
        "parent prompt cursor should land after the trailing prompt space; observed_cursor={observed_cursor:?}; observed_screen={observed_screen:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/exit` from the pane-scoped agent prompt performs the same
/// subshell exit as the keyboard toggle while preserving pane-visible content in
/// history. This covers the slash-command path used by Escape, Ctrl+C, Ctrl+D
/// on an empty prompt, `/quit`, and direct `/exit` submissions through the
/// control API.
#[test]
fn runtime_agent_shell_slash_exit_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
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
    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(service.drain_deferred_pane_inputs().len(), 1);
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service
        .pane_screens
        .get_mut(&pane_id)
        .unwrap()
        .feed(b"slash exit history\nslash exit visible text");
    assert!(
        service
            .pane_screen(&pane_id)
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit","input":"/exit"}}"#,
        &primary,
    );
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    let after_exit_screen = service.pane_screen(&pane_id).unwrap();
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies `/exit` stops an active pane-local turn before hiding agent mode.
/// This protects the exit paths used by slash commands, keyboard shortcuts, and
/// control clients from leaving provider or shell-action work running unseen.
#[test]
fn runtime_agent_shell_slash_exit_stops_running_turn_before_hiding() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-exit-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-stop","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""command":"exit""#), "{response}");
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    assert!(response.contains("stopped_turn=turn-1"), "{response}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.visibility, AgentShellVisibility::Hidden);
    assert_eq!(session.running_turn_id, None);
    assert!(!service.agent_turn_is_running("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Stopped after"), "{pane_text}");
}

/// Verifies ordinary pane input is consumed while an agent-shell hide request
/// is waiting for the active turn to stop. This prevents user keystrokes from
/// leaking into the parent shell before the `/stop` contract has completed.
#[test]
fn runtime_agent_shell_exit_pending_blocks_foreground_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .request_hide_pending_task_completion("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"leak\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: input blocked while agent shell is stopping"),
        "{pane_text}"
    );
}

/// Verifies that runtime-state failures from agent slash commands are reported
/// through the agent display channel instead of surfacing as JSON-RPC errors.
/// This keeps agent-mode clients alive when a runtime-backed command hits an
/// invalid state, such as stopping when no turn is running.
#[test]
fn runtime_control_reports_invalid_state_agent_shell_errors_as_display() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-invalid-state","method":"agent/shell/command","params":{"idempotency_key":"agent-command-invalid-state","input":"/stop"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains("agent command error: agent shell session has no running turn"),
        "{response}"
    );
    assert!(response.contains("(invalid_state)"), "{response}");
    assert!(!response.contains(r#""error""#), "{response}");
}

/// Verifies that the runtime `agent/shell/command` `/list-mcp` path uses the live
/// MCP registry and exposes unavailable or session-blacklisted details. This
/// protects the spec requirement that agent-shell MCP visibility match control
/// and command surfaces instead of returning a generic runtime placeholder.
#[test]
fn runtime_agent_shell_mcp_command_reports_live_registry_detail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "fs",
            vec![crate::mcp::McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: crate::mcp::McpToolEffects::none(),
                approval: crate::mcp::McpApprovalSetting::Inherit,
                description: "read a file".to_string(),
                input_schema_json: "{}".to_string(),
            }],
        )
        .unwrap();
    service
        .mcp_registry_mut()
        .blacklist_for_session("fs", "failed handshake")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"list-mcp""#), "{response}");
    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("Source: runtime-mcp"), "{response}");
    assert!(response.contains("### `fs` - filesystem"), "{response}");
    assert!(response.contains("- State: blacklisted"), "{response}");
    assert!(
        response.contains("- Session blacklisted: true"),
        "{response}"
    );
    assert!(response.contains("- Retryable: true"), "{response}");
    assert!(
        response.contains("- Reason: failed handshake"),
        "{response}"
    );
    assert!(
        response.contains("| `read_file` | blacklisted |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that `/status` is backed by live runtime state rather than only
/// the shell session fallback. The status view is a user-visible conformance
/// surface, so it must include model selection, policy, identity, writable
/// scope state, current context tracking, and provider token counters in one
/// response.
#[test]
fn runtime_agent_shell_status_reports_live_runtime_state() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-fast\"]\ndefault_model = \"gpt-fast\"\n\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service.session.select_pane(&primary, "%1").unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service.record_agent_provider_token_usage(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
    );
    service.record_agent_provider_token_usage(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 40,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        },
    );
    let deepseek_profile = runtime_model_profile("deepseek", "deepseek-chat");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        Some(&deepseek_profile),
    );
    service.record_agent_provider_token_usage(
        second_pane.as_str(),
        crate::agent::ModelTokenUsage {
            input_tokens: 60,
            output_tokens: 10,
            reasoning_tokens: 4,
            cached_input_tokens: Some(30),
            cache_write_input_tokens: None,
        },
    );
    service.runtime_metrics.record_provider_token_usage(
        crate::agent::ModelTokenUsage {
            input_tokens: 300,
            output_tokens: 75,
            reasoning_tokens: 15,
            cached_input_tokens: Some(120),
            cache_write_input_tokens: None,
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 300,
            output_tokens: 75,
            reasoning_tokens: 15,
            cached_input_tokens: Some(120),
            cache_write_input_tokens: None,
        },
        &crate::agent::ModelTokenUsageKey::new("runtime-metrics", "metrics-only"),
    );
    service
        .subagent_scopes
        .register(
            "agent-%1",
            CooperationMode::OwnedWrite,
            &["src".to_string()],
            None,
        )
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "summarize the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-status","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"status""#), "{response}");
    assert!(
        response.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{response}"
    );
    assert!(response.contains("## Agent Status"), "{response}");
    assert!(response.contains("| Field | Value |"), "{response}");
    assert!(response.contains("| Agent id | agent-%1 |"), "{response}");
    assert!(response.contains("| Window id | @1 |"), "{response}");
    assert!(
        response.contains("| Model | gpt-fast via openai (profile: default"),
        "{response}"
    );
    assert!(
        response.contains("| Prompt profile | default v30 |"),
        "{response}"
    );
    assert!(
        response.contains("| Permissions | preset auto, approval full-access"),
        "{response}"
    );
    assert!(
        response.contains("| src | agent-%1 | owned-write |"),
        "{response}"
    );
    assert!(response.contains("| Context | 6 blocks"), "{response}");
    assert!(
        response.contains("| Pane agent tokens | 2 models; see Pane Agent Token Usage |"),
        "{response}"
    );
    assert!(
        response.contains("### Pane Agent Token Usage"),
        "{response}"
    );
    let session_heading = response
        .find("### Pane Agent Token Usage")
        .expect("session token usage heading should be present");
    let instance_heading = response
        .find("### Mez Session Token Usage")
        .expect("instance token usage heading should be present");
    assert!(session_heading < instance_heading, "{response}");
    assert!(
        response.contains("| openai | gpt-fast | 160 | unknown | 34 | 9 | unknown |"),
        "{response}"
    );
    assert!(
        response.contains("| deepseek | deepseek-chat | 100 | 100 | 50 | 20 | 50.00% |"),
        "{response}"
    );
    assert!(
        response.contains("| openai | gpt-fast | 220 | unknown | 44 | 13 | unknown |"),
        "{response}"
    );
    assert!(
        !response.contains("| runtime-metrics | metrics-only |"),
        "{response}"
    );
    assert!(!response.contains("Provider rate limits"), "{response}");
    assert!(!response.contains("### Quota Usage"), "{response}");
    assert!(
        response.contains("| Latest turn | turn-1 (running) |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that `/diff` reads the active pane's Git repository and includes
/// both modified tracked content and untracked files. This covers the spec
/// requirement that the agent shell diff view expose the working tree rather
/// than returning a generic runtime-required placeholder.
#[test]
fn runtime_agent_shell_diff_reports_git_worktree_and_untracked_files() {
    let root = temp_root("runtime-agent-diff");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    fs::write(root.join("tracked.txt"), "before\n").unwrap();
    git(&["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "before\nafter\n").unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();

    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-diff","method":"agent/shell/command","params":{"idempotency_key":"agent-diff","input":"/diff"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"diff""#), "{response}");
    assert!(response.contains("source=runtime-vcs-diff"), "{response}");
    assert!(response.contains("untracked_files=1"), "{response}");
    assert!(response.contains("tracked.txt"), "{response}");
    assert!(response.contains("+after"), "{response}");
    assert!(response.contains("file=new.txt"), "{response}");
    assert!(response.contains("+untracked"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/init` creates a project instruction scaffold in the active
/// pane's working directory and leaves an existing scaffold intact. This covers
/// the baseline file-mutation slash command without writing to the repository
/// root used by the test harness.
#[test]
fn runtime_agent_shell_init_creates_project_instruction_scaffold() {
    let root = temp_root("runtime-agent-init");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init","method":"agent/shell/command","params":{"idempotency_key":"agent-init","input":"/init"}}"#,
        &primary,
    );

    let scaffold = root.join("AGENTS.md");
    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"init""#), "{response}");
    assert!(response.contains("created=true"), "{response}");
    assert!(response.contains("source=runtime-init"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let text = fs::read_to_string(&scaffold).unwrap();
    assert!(text.contains("# Repository Guidelines"), "{text}");
    assert!(
        text.contains("## Build, Test, and Development Commands"),
        "{text}"
    );

    let existing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init-existing","method":"agent/shell/command","params":{"idempotency_key":"agent-init-existing","input":"/init"}}"#,
        &primary,
    );

    assert!(existing.contains(r#""kind":"display""#), "{existing}");
    assert!(existing.contains(r#""command":"init""#), "{existing}");
    assert!(existing.contains("created=false"), "{existing}");
    assert!(existing.contains("existing=true"), "{existing}");
    assert!(!existing.contains("requires_runtime"), "{existing}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/logout` executes through the runtime auth store and removes
/// stored credentials without exposing a duplicate terminal logout command.
#[test]
fn runtime_agent_shell_logout_uses_attached_auth_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-logout");
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    auth_store
        .login_provider_api_key_with_selected_store(
            "openai",
            "work",
            "sk-runtime-secret",
            Some("file"),
        )
        .unwrap();
    service.set_auth_store(auth_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-logout","method":"agent/shell/command","params":{"idempotency_key":"agent-logout","input":"/logout"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"logout""#), "{response}");
    assert!(response.contains("logged_out=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert!(!response.contains("sk-runtime-secret"), "{response}");
    let status = service
        .execute_terminal_command(&primary, "auth-status")
        .unwrap();
    assert!(status.contains("authenticated=false"), "{status}");
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/approval` arguments are applied through the live runtime
/// approval-mode command path. The no-argument slash command already displays
/// policy state; this covers mutation through the agent shell surface.
#[test]
fn runtime_agent_shell_approval_command_mutates_live_policy() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-permissions","method":"agent/shell/command","params":{"idempotency_key":"agent-permissions","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"approval""#), "{response}");
    assert!(response.contains("field=approval_policy"), "{response}");
    assert!(response.contains("requested=full-access"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
}

/// Verifies terse slash-command display output uses transient status feedback.
///
/// One-line status acknowledgements should stay out of the durable agent pane
/// transcript while still giving brief feedback in the window status bar.
#[test]
fn runtime_agent_shell_single_line_display_uses_transient_status_without_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/approval\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(service.primary_display_overlay.is_none());
    assert!(
        service
            .primary_error_status_overlay
            .as_deref()
            .is_some_and(|message| message.contains("approval policy: ask")),
        "{:?}",
        service.primary_error_status_overlay
    );
    let pane_text = service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(!pane_text.contains("approval policy: ask"), "{pane_text}");
    assert!(!pane_text.contains("source: runtime-policy"), "{pane_text}");
}

/// Verifies an explicit `/approval` choice is stored as a live override and
/// therefore survives unrelated configuration reloads from disk.
///
/// This protects full-access mode from being silently reset when a config
/// reload reapplies an older `permissions.approval_policy` value.
#[test]
fn runtime_agent_shell_approval_command_survives_config_reload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-approval-live-override");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
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
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-approval","method":"agent/shell/command","params":{"idempotency_key":"agent-approval-live","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains("requested=full-access"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );

    fs::write(
        &path,
        "[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let reload = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload-approval","method":"config/reload","params":{"idempotency_key":"reload-approval-live"}}"#,
        &primary,
    );

    assert!(reload.contains(r#""operation":"reload""#), "{reload}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/statusline` mutates the live pane status-line rendering
/// fields. The command should configure existing frame state instead of
/// returning a runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_statusline_configures_pane_frame_fields() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-statusline","method":"agent/shell/command","params":{"idempotency_key":"agent-statusline","input":"/statusline agent.status agent.model pane.mode"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"statusline""#), "{response}");
    assert!(response.contains("enabled=true"), "{response}");
    assert!(response.contains("agent.status"), "{response}");
    assert!(response.contains("agent.model"), "{response}");
    assert!(response.contains("pane.mode"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(response.contains("source=runtime-statusline"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert!(service.pane_frames_enabled);
    assert_eq!(
        service.pane_frame_visible_fields,
        vec![
            "agent.status".to_string(),
            "agent.model".to_string(),
            "pane.mode".to_string()
        ]
    );
    assert_eq!(
        service.pane_frame_template,
        "#{agent.status} #{agent.model} #{pane.mode}"
    );
}

/// Verifies that `/title` reads and mutates the active runtime window title
/// through the live command path. This covers the agent shell title command
/// without allowing the slash surface to target or rename unrelated windows.
#[test]
fn runtime_agent_shell_title_displays_and_renames_active_window() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let display = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-display","method":"agent/shell/command","params":{"idempotency_key":"agent-title-display","input":"/title"}}"#,
        &primary,
    );

    assert!(display.contains(r#""kind":"display""#), "{display}");
    assert!(display.contains(r#""command":"title""#), "{display}");
    assert!(display.contains("source=runtime-title"), "{display}");
    assert!(display.contains("window_id=@1"), "{display}");
    assert!(display.contains("window_title=shell"), "{display}");
    assert!(display.contains("pane=%1"), "{display}");
    assert!(display.contains("pane_title=shell"), "{display}");
    assert!(!display.contains("requires_runtime"), "{display}");

    let rename = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-rename","method":"agent/shell/command","params":{"idempotency_key":"agent-title-rename","input":"/title build shell"}}"#,
        &primary,
    );

    assert!(rename.contains(r#""kind":"mutated""#), "{rename}");
    assert!(rename.contains(r#""command":"title""#), "{rename}");
    assert!(rename.contains("source=runtime-title"), "{rename}");
    assert!(rename.contains("changed=true"), "{rename}");
    assert!(rename.contains("window_title=build shell"), "{rename}");
    assert!(!rename.contains("requires_runtime"), "{rename}");
    assert_eq!(
        service.session().active_window().unwrap().name,
        "build shell"
    );
}

/// Verifies that `/debug-config` reports live effective configuration, layer
/// order, and policy diagnostics from runtime state instead of the generic
/// runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_debug_config_reports_live_runtime_config() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 7\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
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

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"debug-config","method":"agent/shell/command","params":{"idempotency_key":"debug-config","input":"/debug-config history.lines"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(r#""command":"debug-config""#),
        "{response}"
    );
    assert!(response.contains("source=runtime-config"), "{response}");
    assert!(response.contains("layers=1"), "{response}");
    assert!(response.contains("applied_layers=1"), "{response}");
    assert!(response.contains("permission_preset=auto"), "{response}");
    assert!(
        response.contains("approval_policy=full-access"),
        "{response}"
    );
    assert!(response.contains("layer=primary"), "{response}");
    assert!(response.contains("scope=primary"), "{response}");
    assert!(response.contains("format=toml"), "{response}");
    assert!(response.contains("value path=history.lines"), "{response}");
    assert!(response.contains("value=7"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that planning-time shell action failures stay visible without
/// exposing the exact command in the default pane buffer. The user still sees
/// the policy failure, while command details remain reserved for verbose or
/// trace mode.
#[test]
fn runtime_agent_shell_planning_failure_hides_command_by_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().add_rule(
        crate::permissions::CommandRule::new(["ls"], RuleDecision::Forbid, RuleMatch::Prefix)
            .unwrap(),
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failed-command","input":"list files"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "list files".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "List files".to_string(),
                        command: "ls".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Denied);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: List files (shell command denied before execution"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("before execution: ls"), "{pane_text}");
    assert!(!pane_text.contains("$ ls"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}
