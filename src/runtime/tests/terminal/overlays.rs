//! Runtime tests for terminal overlays behavior.

use super::*;

/// Verifies that command display output is owned by runtime state instead of a
/// nested terminal loop. The modal overlay must render through the normal
/// primary client view, consume user input while active, and clear on Escape or
/// `q` without forwarding those bytes into the active pane.
#[test]
fn runtime_primary_display_overlay_renders_and_clears_via_terminal_step() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .apply_pane_output_bytes(pane_id, b"prompt$ ".to_vec())
        .unwrap();
    let base_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(base_view.cursor_visible);
    service
        .show_primary_display_overlay(vec![
            "first display line".to_string(),
            "second display line".to_string(),
        ])
        .unwrap();

    let overlay_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(overlay_view.lines[0].trim_end(), "mezzanine command output");
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("first display line")),
        "{:?}",
        overlay_view.lines
    );
    assert!(!overlay_view.cursor_visible);

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
    assert!(report.full_redraw_required);

    let cleared_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        !cleared_view
            .lines
            .iter()
            .any(|line| line.contains("mezzanine command output")),
        "{:?}",
        cleared_view.lines
    );
    assert!(cleared_view.cursor_visible);
    assert_eq!(cleared_view.cursor_row, base_view.cursor_row);
    assert_eq!(cleared_view.cursor_column, base_view.cursor_column);

    service
        .show_primary_display_overlay(vec!["third display line".to_string()])
        .unwrap();
    let quit = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"q".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(quit.forwarded_bytes, 0);
    assert!(quit.view_refresh_required);
    assert!(service.primary_display_overlay.is_none());
}

/// Verifies ordinary input that has no pager binding remains captured by the
/// modal overlay instead of falling through to the active pane.
#[test]
fn runtime_primary_display_overlay_consumes_unbound_pane_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(vec!["modal display line".to_string()])
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(!report.view_refresh_required);
    assert!(service.primary_display_overlay.is_some());
}

/// Verifies plain pager output wraps at the terminal width instead of being
/// truncated by the modal renderer. The pager stores each physical row so its
/// scroll range includes content that would otherwise render off-screen.
#[test]
fn runtime_primary_display_overlay_wraps_plain_content_to_terminal_width() {
    let mut service = test_runtime_service_with_size(Size::new(12, 6).unwrap());
    service
        .show_primary_display_overlay(vec!["alpha beta gamma".to_string()])
        .unwrap();

    let overlay = service.primary_display_overlay.as_ref().unwrap();
    assert_eq!(overlay.lines, vec!["alpha beta", "gamma"]);
    assert!(
        overlay
            .lines
            .iter()
            .all(|line| crate::terminal::terminal_text_width(line) <= 12)
    );
}

/// Verifies keyboard movement inside a primary command-output pager refreshes
/// through the retained-frame diff path.
///
/// Navigating a selectable pager row only changes the active highlight and
/// optional viewport offset. It must not invalidate the whole attached output
/// frame, otherwise remote terminals flicker during routine list navigation.
#[test]
fn runtime_primary_display_overlay_keyboard_navigation_requests_diff_refresh() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.active_selection_index),
        Some(0)
    );
    let initial_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let initial_active_row = initial_view
        .lines
        .iter()
        .position(|line| line.starts_with("> "))
        .expect("overlay should show an active selector gutter");
    assert!(
        initial_view
            .lines
            .iter()
            .enumerate()
            .any(|(index, line)| index != initial_active_row && line.starts_with("  ")),
        "{initial_view:?}"
    );

    let report = service
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

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.active_selection_index),
        Some(1)
    );
    let moved_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let moved_active_row = moved_view
        .lines
        .iter()
        .position(|line| line.starts_with("> "))
        .expect("overlay should keep an active selector gutter after navigation");
    assert_ne!(moved_active_row, initial_active_row, "{moved_view:?}");
    assert!(
        moved_view
            .lines
            .iter()
            .enumerate()
            .any(|(index, line)| index != moved_active_row && line.starts_with("  ")),
        "{moved_view:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies mouse-wheel scrolling inside a primary command-output pager uses a
/// light view refresh instead of a full terminal-frame redraw.
///
/// The overlay renderer already produces a complete next view for the changed
/// rows, so the attach client can keep diffing against the retained frame.
#[test]
fn runtime_primary_display_overlay_mouse_scroll_requests_diff_refresh() {
    let mut service = test_runtime_service_with_size(Size::new(40, 6).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(
            (0..20)
                .map(|index| format!("display line {index:02}"))
                .collect(),
        )
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollDisplayOverlay { lines: 2 },
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
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .map(|overlay| overlay.scroll_offset),
        Some(2)
    );
}

/// Verifies forward text search inside a primary command-output pager, including
/// empty-query repeat and wraparound back to the first matching line.
#[test]
fn runtime_primary_display_overlay_search_repeats_and_wraps() {
    let mut service = test_runtime_service_with_size(Size::new(80, 10).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 10).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(vec![
            "alpha opening".to_string(),
            "needle first".to_string(),
            "middle text".to_string(),
            "needle second".to_string(),
            "closing text".to_string(),
        ])
        .unwrap();

    let initial_search = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"needle".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(initial_search.forwarded_bytes, 0);
    assert!(initial_search.view_refresh_required);
    assert!(!initial_search.full_redraw_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_query.as_deref()),
        Some("needle")
    );
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay
                .search_match
                .map(|search_match| search_match.line_index)),
        Some(1)
    );

    let next_match = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(next_match.forwarded_bytes, 0);
    assert!(next_match.view_refresh_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay
                .search_match
                .map(|search_match| search_match.line_index)),
        Some(3)
    );
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_status.as_deref()),
        None
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay
                .search_match
                .map(|search_match| search_match.line_index)),
        Some(1)
    );
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_status.as_deref()),
        None
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"absent".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let overlay = service.primary_display_overlay.as_ref().unwrap();
    assert_eq!(
        overlay
            .search_match
            .map(|search_match| search_match.line_index),
        Some(1)
    );
    assert_eq!(
        overlay.search_status.as_deref(),
        Some("pattern not found: absent")
    );
}

/// Verifies that command chooser output rendered in the primary overlay is not
/// inert text. Rows that advertise an `action=` command must retain selectable
/// metadata so a mouse click can execute the command through the normal
/// terminal command path and then close or replace the overlay.
#[test]
fn runtime_primary_display_overlay_executes_selectable_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-window should open a command display overlay");
    let work_selection = overlay
        .selections
        .iter()
        .find(|selection| selection.command == "select-window -t @2")
        .expect("work window row should advertise a selectable action");
    let clicked_row = work_selection.line_index.saturating_add(1);
    let clicked_column = work_selection.start_column.saturating_add(2);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_row,
                            column: clicked_column,
                        },
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies selectable command rows exposed by the primary display overlay can
/// be chosen from the keyboard. Mouse clicks and keyboard Enter must execute the
/// same stored command metadata so chooser output does not depend on scraping
/// the rendered text.
#[test]
fn runtime_primary_display_overlay_executes_keyboard_selected_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert!(service.primary_display_overlay.is_some());
    assert_eq!(service.session().active_window().unwrap().name, "0");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
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
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies command overlays can expose multiple selectable choices on one row.
/// The user should be able to distinguish routine and destructive choices by
/// color, move between them with selector keys, and execute the active choice
/// without scraping command text out of the rendered row.
#[test]
fn runtime_primary_display_overlay_executes_multiple_action_chips() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.active_paste_buffer = Some("main".to_string());

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-buffer should open a command display overlay");
    let paste = overlay
        .selections
        .iter()
        .position(|selection| selection.command == "paste-buffer -b main")
        .expect("buffer row should expose a paste choice");
    let delete = overlay
        .selections
        .iter()
        .position(|selection| selection.command == "delete-buffer main")
        .expect("buffer row should expose a delete choice");
    assert_eq!(delete, paste.saturating_add(1));

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
        .position(|line| line.contains("[paste]") && line.contains("[delete]"))
        .expect("overlay should render compact action chips");
    assert!(view.lines[row].contains("[paste]"));
    assert!(view.lines[row].contains("[delete]"));
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[paste]".len()
                && !span.rendition.inverse
                && span.rendition.background
                    == Some(service.ui_theme.colors.agent_reasoning.background)
                && span.rendition.foreground
                    == Some(service.ui_theme.colors.agent_reasoning.foreground)
                && span.rendition.bold
                && span.rendition.underline
        }),
        "{view:?}"
    );
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[delete]".len()
                && !span.rendition.inverse
                && span.rendition.background.is_none()
                && span.rendition.foreground
                    == Some(service.ui_theme.colors.agent_status_failed.foreground)
                && span.rendition.bold
                && span.rendition.underline
        }),
        "{view:?}"
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[C".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
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
    assert!(report.view_refresh_required);
    assert!(service.paste_buffers.get("main").is_none());
    assert_eq!(service.active_paste_buffer.as_deref(), None);
    assert!(service.primary_display_overlay.is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies mouse selection resolves the clicked chip when multiple choices are
/// present on the same display row. This keeps multi-action rows from falling
/// back to ambiguous whole-row execution.
#[test]
fn runtime_primary_display_overlay_mouse_selects_action_chip() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.active_paste_buffer = Some("main".to_string());

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    let (clicked_line, clicked_column) = service
        .primary_display_overlay
        .as_ref()
        .and_then(|overlay| {
            overlay
                .selections
                .iter()
                .find(|selection| selection.command == "delete-buffer main")
                .map(|selection| {
                    (
                        selection.line_index.saturating_add(1),
                        selection.start_column.saturating_add(2),
                    )
                })
        })
        .expect("delete-buffer choice should be selectable");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_line,
                            column: clicked_column,
                        },
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.paste_buffers.get("main").is_none());
    assert_eq!(service.active_paste_buffer.as_deref(), None);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that recoverable error overlays render as transient status-bar
/// notices. The next input should clear the notice as presentational state
/// without being forwarded or replayed, so repeating an error-causing action
/// does not immediately trigger the same error while dismissing the overlay.
#[test]
fn runtime_primary_error_overlay_dismisses_on_any_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .show_primary_error_overlay(vec!["error: simulated".to_string()])
        .unwrap();

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("simulated")),
        "{:?}",
        view.lines
    );
    assert!(view.cursor_visible);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
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
    assert!(service.primary_error_status_overlay.is_none());
    assert!(service.primary_display_overlay.is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}
