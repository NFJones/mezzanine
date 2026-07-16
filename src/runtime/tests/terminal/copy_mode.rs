//! Runtime tests for terminal copy mode behavior.

use super::*;

/// Verifies mouse-wheel history scrolling updates the pane through a diff
/// refresh. Scrollback movement changes the copy-mode viewport but not the
/// terminal geometry, so preserving the retained output frame avoids visible
/// flicker over slower terminal links.
#[test]
fn runtime_mouse_history_scroll_requests_diff_refresh() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 20).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour\nfive\nsix");
    service.set_pane_screen(pane_id.clone(), screen);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollHistory {
                        lines: -3,
                        position: CopyPosition { line: 1, column: 1 },
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
    assert!(!report.full_redraw_required);
    assert!(service.active_copy_modes().contains_key(&pane_id));
    assert!(
        service
            .scrollback_copy_mode_panes_for_tests()
            .contains(&pane_id)
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(config.scrollback_copy_mode_active);
    assert_eq!(
        crate::host::terminal::route_client_input(b"\x1b[5~", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(mez_mux::copy::CopyModeKeyAction::PageUp)
    );
    assert_eq!(
        crate::host::terminal::route_client_input(b"q", &config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"q".to_vec())
    );
}

/// Verifies a double-click copied-word highlight remains visible across the
/// first render and only clears after its configured 500 ms lifetime expires.
/// This protects the copied-word flash from disappearing immediately on the
/// first rendered client view while still ensuring cleanup happens once the
/// timeout elapses.
#[test]
fn runtime_double_click_highlight_persists_until_cleanup_deadline() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"alpha beta --flag");
    service.set_pane_screen("%1".to_string(), screen);

    for _ in 0..2 {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(
                        MouseAction::FocusPane(CopyPosition { line: 0, column: 7 }),
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

    assert!(service.deferred_word_copy_cleanup().borrow().is_some());
    let config = TerminalClientLoopConfig::default();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(20, 4).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(!view.line_style_spans.iter().all(|spans| spans.is_empty()));
    assert!(service.deferred_word_copy_cleanup().borrow().is_some());

    if let Some((pane_id, copy_mode, cleanup_at_unix_ms)) =
        service.deferred_word_copy_cleanup().borrow_mut().as_mut()
    {
        *pane_id = "%1".to_string();
        *copy_mode = copy_mode.clone();
        *cleanup_at_unix_ms = 0;
    }

    service
        .render_client_view(ClientViewRole::Primary, Size::new(20, 4).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(service.deferred_word_copy_cleanup().borrow().is_none());
}
