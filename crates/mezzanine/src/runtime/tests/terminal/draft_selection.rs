//! Production interaction regressions for source-aware draft selection.

use super::*;

/// Routes a complete pointer gesture through the host classifier and runtime,
/// retaining exact client ownership and rebuilding policy between input units.
fn routed_draft_gesture(
    service: &mut RuntimeSessionService,
    client: &mez_core::ids::ClientId,
    units: &[String],
) {
    for bytes in units {
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        let action = crate::host::terminal::route_client_input(bytes.as_bytes(), &config).unwrap();
        service
            .apply_attached_terminal_step_plan(
                client,
                &AttachedTerminalClientStepPlan {
                    actions: vec![action],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }
}

/// Reverse and forward drags include both endpoint graphemes, with authored
/// newlines recovered independently of visual row boundaries.
#[test]
fn runtime_draft_reverse_drag_includes_endpoint_graphemes() {
    for (text, last_row, last_column) in [("abc", 0, 4), ("e\u{301}雪c", 0, 5), ("abc\ndef", 1, 4)]
    {
        let mut service = test_runtime_service_with_size(Size::new(80, 24).unwrap());
        service.set_frame_visibility_for_tests(false, false);
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service.reload_agent_prompt_history_for_pane("%1").unwrap();
        service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap()
            .prompt
            .buffer
            .set_line(text);
        let view = service
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                &TerminalClientLoopConfig::default(),
            )
            .unwrap()
            .unwrap();
        let row = view
            .lines
            .iter()
            .position(|line| line.starts_with("⟩ "))
            .unwrap();
        routed_draft_gesture(
            &mut service,
            &primary,
            &[
                format!("\x1b[<0;{};{}M", last_column + 1, row + last_row + 1),
                format!("\x1b[<32;3;{}M", row + 1),
                format!("\x1b[<0;3;{}m", row + 1),
            ],
        );
        assert_eq!(service.paste_buffers().get("mouse"), Some(text), "{text:?}");
    }
}

/// Releasing over an authored blank row completes the draft gesture and copies
/// its newline. Subsequent wheel input and new gestures are not captured by an
/// abandoned draft drag.
#[test]
fn runtime_draft_release_on_blank_row_ends_gesture() {
    let mut service = test_runtime_service_with_size(Size::new(80, 24).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .set_line("abc\n");
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut history = TerminalScreen::new(Size::new(80, 20).unwrap(), 120).unwrap();
    history.feed(
        (0..60)
            .map(|index| format!("history-{index}\r\n"))
            .collect::<String>()
            .as_bytes(),
    );
    service.set_agent_pane_screen("%1", &conversation, history);
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = view
        .lines
        .iter()
        .position(|line| line.starts_with("⟩ abc"))
        .unwrap();
    routed_draft_gesture(
        &mut service,
        &primary,
        &[
            format!("\x1b[<0;3;{}M", row + 1),
            format!("\x1b[<0;3;{}m", row + 2),
        ],
    );
    assert!(
        !service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap()
            .mouse_selection_active
    );
    assert_eq!(service.paste_buffers().get("mouse"), Some("abc\n"));
    routed_draft_gesture(&mut service, &primary, &["\x1b[<64;3;3M".into()]);
    assert!(
        service
            .active_copy_mode_for_presented_surface("%1")
            .is_some()
    );
    routed_draft_gesture(
        &mut service,
        &primary,
        &[
            format!("\x1b[<0;3;{}M", row + 1),
            format!("\x1b[<0;5;{}m", row + 1),
        ],
    );
    assert!(
        !service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap()
            .mouse_selection_active
    );
}

/// Pointer copying entered draft text must retain the scrolled log, cursor and
/// draft bytes and must never submit a provider request or copy UI decoration.
#[test]
fn runtime_draft_selection_copies_entered_text_without_moving_log() {
    let mut service = test_runtime_service_with_size(Size::new(80, 24).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .set_line("Exact Draft 雪\nsecond line");
    let draft = service.agent_prompt_inputs_for_tests()["%1"].prompt.clone();
    let mut screen = TerminalScreen::new(Size::new(80, 20).unwrap(), 120).unwrap();
    screen.feed(
        (0..60)
            .map(|index| format!("log-{index}\r\n"))
            .collect::<String>()
            .as_bytes(),
    );
    service.set_agent_pane_screen("%1", &conversation, screen);
    service
        .ensure_active_copy_mode("%1")
        .unwrap()
        .scroll_to_top();
    let top = service
        .active_copy_mode_for_presented_surface("%1")
        .unwrap()
        .scroll_top();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = view
        .lines
        .iter()
        .position(|line| line.contains("⟩ Exact Draft 雪"))
        .unwrap();
    for bytes in [
        format!("\x1b[<0;3;{}M", row + 1),
        format!("\x1b[<64;3;{}M", row + 1),
        format!("\x1b[<32;14;{}M", row + 2),
        format!("\x1b[<0;14;{}m", row + 2),
    ] {
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        let action = crate::host::terminal::route_client_input(bytes.as_bytes(), &config).unwrap();
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![action],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }
    assert_eq!(
        service.paste_buffers().get("mouse"),
        Some("Exact Draft 雪\nsecond line")
    );
    assert_eq!(service.agent_prompt_inputs_for_tests()["%1"].prompt, draft);
    assert_eq!(
        service
            .active_copy_mode_for_presented_surface("%1")
            .unwrap()
            .scroll_top(),
        top
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Explicit keyboard draft copy must distinguish visible paste labels from
/// hidden source, preserve selection through resize, and reject changed drafts.
#[test]
fn runtime_draft_selection_keyboard_copy_fences_paste_and_edits() {
    let mut service = test_runtime_service_with_size(Size::new(80, 24).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    let hidden = "Hidden Paste 雪\n".repeat(100);
    let state = service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap();
    state.prompt.buffer.insert_text("Before ");
    state.prompt.buffer.insert_pasted_text(&hidden);
    state.prompt.buffer.insert_text(" After");
    let original = state.prompt.clone();
    let source = original.buffer.expanded_line();
    let rendered = original.buffer.rendered_line();
    service
        .execute_terminal_command(&primary, "copy-mode --draft")
        .unwrap();
    service
        .execute_terminal_command(&primary, "copy-selection --draft -b visible")
        .unwrap();
    assert_eq!(
        service.paste_buffers().get("visible"),
        Some(rendered.as_str())
    );
    assert!(!rendered.contains("Hidden Paste"));
    service
        .execute_terminal_command(&primary, "copy-selection --draft --format source -b exact")
        .unwrap();
    assert_eq!(service.paste_buffers().get("exact"), Some(source.as_str()));
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(matches!(
        crate::host::terminal::route_client_input(b"\r", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(_)
    ));
    service
        .resize_attached_primary_terminal(&primary, Size::new(40, 12).unwrap())
        .unwrap();
    assert_eq!(
        service.copy_draft_selection("%1", true).as_deref(),
        Some(source.as_str())
    );
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"].prompt,
        original
    );
    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .insert_text(" changed");
    assert!(service.copy_draft_selection("%1", true).is_none());
    assert!(
        service
            .execute_terminal_command(&primary, "copy-selection --draft --format source -b exact")
            .is_err()
    );
    assert_eq!(service.paste_buffers().get("exact"), Some(source.as_str()));
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Selection belongs to one primary and source revision. A second primary
/// cannot inherit it; returning restores its selection without sharing drafts.
#[test]
fn runtime_draft_selection_is_client_local_and_word_copy_is_entered_only() {
    let mut service = test_runtime_service_with_size(Size::new(80, 24).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let first = service
        .attach_primary("first", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .set_line("Exact Word 雪");
    service
        .execute_terminal_command(&first, "copy-mode --draft")
        .unwrap();
    let selected = service.copy_draft_selection("%1", true).unwrap();
    let second = service
        .attach_primary("second", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .prepare_client_render(&second, ClientViewRole::Primary)
        .unwrap();
    assert!(service.copy_draft_selection("%1", true).is_none());
    service
        .prepare_client_render(&first, ClientViewRole::Primary)
        .unwrap();
    assert_eq!(
        service.copy_draft_selection("%1", true).as_deref(),
        Some(selected.as_str())
    );
    service.clear_draft_selection("%1");
    let shown = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = shown
        .lines
        .iter()
        .position(|line| line.contains("⟩ Exact Word 雪"))
        .unwrap();
    for _ in 0..2 {
        service
            .apply_attached_terminal_step_plan(
                &first,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(
                        MouseAction::FocusPane(CopyPosition {
                            line: row,
                            column: 9,
                        }),
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }
    assert_eq!(service.paste_buffers().get("mouse"), Some("Word"));
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "Exact Word 雪"
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}
