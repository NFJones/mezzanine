//! Agent prompt editing navigation tests.

use super::*;

/// Verifies Up/Down move through soft-wrapped prompt rows before history.
///
/// Long single-line drafts can occupy multiple visible rows, but ordinary Up
/// and Down keys still operate on the rendered prompt rows before falling back
/// to the submitted-prompt history contract at the first or last row.
#[test]
fn runtime_agent_prompt_up_moves_within_soft_wrapped_draft_before_history() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state.prompt.buffer.set_history(vec![
            "first saved prompt".to_string(),
            "second saved prompt".to_string(),
        ]);
        prompt_state
            .prompt
            .buffer
            .set_line("alpha beta gamma delta");
    }
    let original_cursor = service
        .agent_prompt_inputs_for_tests()
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
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
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma delta");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);

    service
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
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma delta");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "second saved prompt");
}

/// Verifies pane-local agent prompt navigation uses the rendered pane width
/// after reserving the shared right divider.
///
/// Split-pane agent prompts must wrap and move vertically on the same columns
/// the terminal renderer uses. Otherwise Up can move the cursor sideways on the
/// current visual row instead of to the row above.
#[test]
fn runtime_agent_prompt_navigation_uses_split_pane_render_width() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(30, 8).unwrap(), 120)
        .unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state.prompt.buffer.set_line("abcde fghij klmno");
    }
    let original_cursor = service
        .agent_prompt_inputs_for_tests()
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
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
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "abcde fghij klmno");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
    assert_eq!(prompt_state.prompt.buffer.cursor(), "abcde fghij".len());
}

/// Verifies wrapped agent prompt navigation scrolls the visible prompt window
/// to keep the editing cursor on-screen.
///
/// Agent prompt rendering caps visible input rows at six. Moving upward through
/// a taller multiline draft must shift the rendered prompt window instead of
/// leaving the cursor on an off-screen row that cannot be edited in place.
#[test]
fn runtime_agent_prompt_navigation_scrolls_visible_rows_with_cursor() {
    let mut service = test_runtime_service_with_size(Size::new(24, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state
            .prompt
            .buffer
            .set_line("row1\nrow2\nrow3\nrow4\nrow5\nrow6\nrow7");
    }

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A".to_vec(),
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
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.cursor(), "row1".len());
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(24, 8).unwrap(), &config)
        .unwrap()
        .unwrap();
    let view_text = view.lines.join("\n");
    assert!(view_text.contains("mez> row1"), "{view_text}");
    assert!(!view_text.contains("row7"), "{view_text}");
}

/// Verifies pane-local prompt height changes immediately resize only the owning
/// PTY. Split panes can hold prompts with different wrapped heights, so typing
/// into one pane must not leave that pane at a stale process size or borrow the
/// sibling pane's prompt reservation.
#[test]
fn runtime_agent_prompt_height_resize_is_pane_local() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(30, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();

    let initial_first = service.find_pane_descriptor("%1").unwrap().size;
    let initial_second = service
        .find_pane_descriptor(second_pane.as_str())
        .unwrap()
        .size;
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"alpha beta gamma delta".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    let resized_first = service.find_pane_descriptor("%1").unwrap().size;
    let resized_second = service
        .find_pane_descriptor(second_pane.as_str())
        .unwrap()
        .size;

    assert_eq!(resized_first.columns, initial_first.columns);
    assert!(
        resized_first.rows < initial_first.rows,
        "owning pane PTY should shrink when its prompt wraps: {initial_first:?} -> {resized_first:?}"
    );
    assert_eq!(resized_second, initial_second);

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies application-cursor-mode arrows still drive agent prompt navigation.
///
/// PTY applications can leave the pane in application cursor mode, which causes
/// the attached terminal router to forward SS3 arrow sequences. The
/// Mezzanine-owned agent prompt must normalize those bytes before applying
/// readline navigation.
#[test]
fn runtime_agent_prompt_accepts_application_cursor_arrow_sequences() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state
            .prompt
            .buffer
            .set_line("alpha beta gamma delta");
    }
    let original_cursor = service
        .agent_prompt_inputs_for_tests()
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1bOA".to_vec())],
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
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma delta");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
}

/// Verifies runtime agent prompts keep Up/Down within explicit multiline draft
/// rows before recalling submitted prompt history.
#[test]
fn runtime_agent_prompt_up_moves_within_multiline_draft_before_history() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state.prompt.buffer.set_history(vec![
            "first saved prompt".to_string(),
            "second saved prompt".to_string(),
        ]);
        prompt_state
            .prompt
            .buffer
            .set_line("first line\nsecond line\nthird line");
    }

    let original_cursor = service
        .agent_prompt_inputs_for_tests()
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
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
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.line(),
        "first line\nsecond line\nthird line"
    );
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
}

/// Verifies that pane-local agent mode does not make the primary client modal.
/// Mux navigation can still focus another pane, and ordinary text input after
/// that focus change must go to the newly active shell instead of being
/// captured by the original pane's agent prompt.
#[test]
fn runtime_agent_prompt_allows_navigation_and_other_pane_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let input = b"echo outside agent\n".to_vec();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical),
            TerminalClientLoopAction::ForwardToPane(input.clone()),
        ],
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
    assert_eq!(report.forwarded_bytes, input.len());
    assert_eq!(report.agent_prompt_inputs_applied, 0);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    assert!(!service.agent_prompt_inputs_for_tests().contains_key("%2"));
    service.terminate_all_pane_processes().unwrap();
}
