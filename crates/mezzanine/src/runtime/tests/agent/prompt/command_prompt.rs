//! Runtime tests for agent prompt command prompt behavior.

use super::*;

/// Verifies that the primary command prompt is runtime state rather than a
/// nested prompt loop. Submitted input must be consumed by the actor, clear the
/// prompt immediately, execute the terminal command, and render command output
/// through the primary display overlay without forwarding bytes to the pane.
#[test]
fn runtime_primary_command_prompt_submits_and_clears_through_terminal_step() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let prompt_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(50, 8).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(prompt_view.primary_prompt_active);
    assert_eq!(
        prompt_view.lines.last().map(|line| line.trim_end()),
        Some("▐ :")
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"help\r".to_vec())],
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
    assert!(report.full_redraw_required);
    let display_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(50, 8).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(!display_view.primary_prompt_active);
    assert_eq!(display_view.lines[0].trim_end(), "mezzanine command output");
    assert!(
        display_view
            .lines
            .iter()
            .any(|line| line.contains("Mezzanine command help")),
        "{:?}",
        display_view.lines
    );
    assert!(
        display_view
            .lines
            .iter()
            .any(|line| line.contains("Category") && line.contains("Command")),
        "{:?}",
        display_view.lines
    );
}

/// Verifies Ctrl+L clears the live viewport while keeping the terminal command
/// prompt open and preserving prior visible content in pane history. Escape
/// exits that prompt without forwarding bytes.
#[test]
fn runtime_primary_command_prompt_ctrl_l_clears_and_escape_exits() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(50, 8).unwrap(), 120).unwrap();
    screen.feed(b"old output");
    service.set_pane_screen("%1".to_string(), screen);
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old output")
    );

    service.enter_primary_command_prompt("li").unwrap();
    let clear = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x0c".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(clear.forwarded_bytes, 0);
    assert!(service.primary_prompt_input().is_some());
    assert!(
        !service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("old output")
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old output")
    );

    let escape = service
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

    assert_eq!(escape.forwarded_bytes, 0);
    assert!(service.primary_prompt_input().is_none());
}

/// Verifies that immediate terminal commands submitted through the command
/// prompt take effect without opening a modal display overlay. Commands like
/// `send-prefix` already have an observable pane effect, so users should not
/// have to press Escape after invoking them from the prompt.
#[test]
fn runtime_primary_command_prompt_immediate_command_does_not_open_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"send-prefix\r".to_vec(),
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
    assert!(report.view_refresh_required);
    assert!(service.primary_prompt_input().is_none());
    assert!(service.primary_display_overlay().is_none());
    service.enter_primary_command_prompt("").unwrap();

    let create_buffer = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"create-buffer ack --content hello\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(create_buffer.forwarded_bytes, 0);
    assert!(create_buffer.view_refresh_required);
    assert!(service.primary_prompt_input().is_none());
    assert!(service.primary_display_overlay().is_none());
    assert_eq!(service.paste_buffers().get("ack"), Some("hello"));
    assert!(
        service
            .primary_error_status_overlay()
            .is_some_and(|message| message.contains("buffer: ack")),
        "{:?}",
        service.primary_error_status_overlay()
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("mez: buffer: ack"), "{pane_text}");
    assert!(!pane_text.contains("created=true"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the primary command prompt retains submitted commands across
/// prompt openings and exposes them through the same readline Up/Down and
/// Ctrl+R reverse-search behavior used by agent prompts. The command history
/// must remain prompt-local runtime state rather than being forwarded to the
/// pane shell.
#[test]
fn runtime_primary_command_prompt_uses_readline_history_and_reverse_search() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-primary-command-history-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();

    service.enter_primary_command_prompt("").unwrap();
    let first = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"help\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(first.forwarded_bytes, 0);
    assert!(service.primary_prompt_input().is_none());
    service.clear_primary_display_overlay();

    service.enter_primary_command_prompt("").unwrap();
    let second = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"list-buffers\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(second.forwarded_bytes, 0);
    assert_eq!(
        service.primary_command_prompt_history(),
        vec![String::from("help"), String::from("list-buffers")]
    );
    assert_eq!(
        transcript_store.command_prompt_history().unwrap(),
        vec![String::from("help"), String::from("list-buffers")]
    );
    assert!(transcript_store.command_prompt_history_file().exists());
    service.clear_primary_display_overlay();
    service.push_primary_command_prompt_history_for_tests("show list-buffers".to_string());

    service.enter_primary_command_prompt("li").unwrap();
    let search = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(search.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "show list-buffers");

    let restore_draft = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(restore_draft.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "li");
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies MCP server ids complete in primary `:` commands that address
/// existing MCP configuration. These ids come from the live runtime registry,
/// not the static command table.
#[test]
fn runtime_primary_command_prompt_mcp_status_autocompletes_configured_server_id() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture MCP",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"mcp-status fi".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service.primary_prompt_input().unwrap().prompt.buffer.line(),
        "mcp-status fixture "
    );
}

/// Verifies standalone Escape cancels primary command reverse search without
/// closing the prompt itself.
///
/// Reverse search is an in-prompt editing mode. Escape must restore the draft
/// that was present before search started, while a later standalone Escape from
/// normal prompt mode can still close the prompt.
#[test]
fn runtime_primary_command_prompt_escape_cancels_reverse_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.set_primary_command_prompt_history_for_tests(vec![
        "list-buffers".to_string(),
        "show list-buffers".to_string(),
    ]);

    service.enter_primary_command_prompt("li").unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service
            .primary_prompt_input()
            .unwrap()
            .prompt
            .reverse_search_active()
    );

    let escape = service
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

    assert_eq!(escape.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input().unwrap();
    assert!(!prompt.prompt.reverse_search_active());
    assert_eq!(prompt.prompt.buffer.line(), "li");
}

/// Verifies that the terminal command prompt accepts encoded Ctrl+R from
/// terminals that use CSI-u for modified printable keys.
///
/// The low-level readline decoder already handles the legacy ASCII control
/// byte. This runtime-level regression keeps the active prompt path wired so
/// command history search works with terminal encodings commonly emitted by
/// modern emulators.
#[test]
fn runtime_primary_command_prompt_accepts_encoded_ctrl_r_history_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.set_primary_command_prompt_history_for_tests(vec![
        "list-buffers".to_string(),
        "show list-buffers".to_string(),
    ]);

    service.enter_primary_command_prompt("li").unwrap();
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[114;5u".to_vec(),
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
    let prompt = service.primary_prompt_input().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "show list-buffers");
}
