//! Runtime tests for terminal copy mode behavior.

use super::*;
use crate::runtime::PaneSurfaceKind;

/// Verifies process and agent copy modes retain independent viewport and
/// selection state when pane visibility switches between retained surfaces.
#[test]
fn runtime_copy_mode_state_is_retained_per_pane_surface() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let pane_id = service.active_pane_id().unwrap().to_string();
    let size = Size::new(20, 4).unwrap();
    let mut process_screen = TerminalScreen::new(size, 20).unwrap();
    process_screen
        .feed(b"process one\r\nprocess two\r\nprocess three\r\nprocess four\r\nprocess five");
    service.set_process_pane_screen(&pane_id, process_screen);

    let process_key = service.copy_mode_key(&pane_id, PaneSurfaceKind::Process);
    let process_state = {
        let copy_mode = service.ensure_active_copy_mode(&pane_id).unwrap();
        copy_mode.scroll_to_top();
        copy_mode
            .select_range(
                CopyPosition { line: 0, column: 0 },
                CopyPosition { line: 0, column: 7 },
            )
            .unwrap();
        (copy_mode.scroll_top(), copy_mode.selection())
    };

    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(size, 20).unwrap();
    agent_screen.feed(b"agent one\r\nagent two\r\nagent three\r\nagent four\r\nagent five");
    service.set_agent_pane_screen(&pane_id, &conversation_id, agent_screen);
    let agent_key = service.copy_mode_key(&pane_id, PaneSurfaceKind::Agent);
    let agent_state = {
        let copy_mode = service.ensure_active_copy_mode(&pane_id).unwrap();
        copy_mode.scroll_to_bottom();
        copy_mode
            .select_range(
                CopyPosition { line: 4, column: 0 },
                CopyPosition { line: 4, column: 5 },
            )
            .unwrap();
        (copy_mode.scroll_top(), copy_mode.selection())
    };

    assert_ne!(process_state, agent_state);
    assert_eq!(
        service
            .active_copy_modes()
            .get(&process_key)
            .map(|copy_mode| (copy_mode.scroll_top(), copy_mode.selection())),
        Some(process_state)
    );
    assert_eq!(
        service
            .active_copy_modes()
            .get(&agent_key)
            .map(|copy_mode| (copy_mode.scroll_top(), copy_mode.selection())),
        Some(agent_state)
    );

    service
        .agent_shell_store_mut()
        .request_exit(&pane_id)
        .unwrap();
    assert_eq!(
        service
            .active_copy_mode_for_presented_surface(&pane_id)
            .map(|copy_mode| (copy_mode.scroll_top(), copy_mode.selection())),
        Some(process_state)
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    assert_eq!(
        service
            .active_copy_mode_for_presented_surface(&pane_id)
            .map(|copy_mode| (copy_mode.scroll_top(), copy_mode.selection())),
        Some(agent_state)
    );
}

/// Verifies destructive history clearing invalidates only the copy snapshot
/// owned by the currently presented surface.
#[test]
fn runtime_clear_history_invalidates_only_presented_surface_copy_state() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let size = Size::new(20, 4).unwrap();
    let mut process_screen = TerminalScreen::new(size, 20).unwrap();
    process_screen
        .feed(b"deleted one\r\ndeleted two\r\ndeleted three\r\ndeleted four\r\nprocess live");
    service.set_process_pane_screen(&pane_id, process_screen);
    service.ensure_active_copy_mode(&pane_id).unwrap();
    let process_key = service.copy_mode_key(&pane_id, PaneSurfaceKind::Process);

    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(size, 20).unwrap();
    agent_screen.feed(b"agent retained one\r\nagent retained two");
    service.set_agent_pane_screen(&pane_id, &conversation_id, agent_screen);
    service.ensure_active_copy_mode(&pane_id).unwrap();
    let agent_key = service.copy_mode_key(&pane_id, PaneSurfaceKind::Agent);

    service
        .agent_shell_store_mut()
        .request_exit(&pane_id)
        .unwrap();
    let response = service
        .execute_terminal_command(&primary, "clear-history --confirm")
        .unwrap();
    assert!(response.contains("cleared=true"), "{response}");
    assert!(!service.active_copy_modes().contains_key(&process_key));
    assert!(service.active_copy_modes().contains_key(&agent_key));
}

/// Verifies retained process and agent copy viewports adopt their resized
/// screen geometry after the screens themselves are synchronized.
#[test]
fn runtime_resize_refreshes_copy_geometry_for_both_surfaces() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let size = Size::new(20, 4).unwrap();
    let content = b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\r\nnine\r\nten";
    let mut process_screen = TerminalScreen::new(size, 20).unwrap();
    process_screen.feed(content);
    service.set_process_pane_screen(&pane_id, process_screen);
    service.ensure_active_copy_mode(&pane_id).unwrap();
    let process_key = service.copy_mode_key(&pane_id, PaneSurfaceKind::Process);

    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(size, 20).unwrap();
    agent_screen.feed(content);
    service.set_agent_pane_screen(&pane_id, &conversation_id, agent_screen);
    service.ensure_active_copy_mode(&pane_id).unwrap();
    let agent_key = service.copy_mode_key(&pane_id, PaneSurfaceKind::Agent);
    service
        .agent_shell_store_mut()
        .request_exit(&pane_id)
        .unwrap();

    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 8).unwrap())
        .unwrap();

    let process_rows = usize::from(service.process_pane_screen(&pane_id).unwrap().size().rows);
    let agent_rows = usize::from(service.agent_pane_screen(&pane_id).unwrap().size().rows);
    assert_eq!(
        service.active_copy_modes()[&process_key]
            .visible_lines()
            .len(),
        process_rows
    );
    assert_eq!(
        service.active_copy_modes()[&agent_key]
            .visible_lines()
            .len(),
        agent_rows
    );
}

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
    assert!(
        service
            .active_copy_mode_for_presented_surface(&pane_id)
            .is_some()
    );
    assert!(service.presented_surface_uses_scrollback_copy_mode(&pane_id));

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

    if let Some((pane_id, _surface, copy_mode, cleanup_at_unix_ms)) =
        service.deferred_word_copy_cleanup().borrow_mut().as_mut()
    {
        *pane_id = "%1".to_string();
        *copy_mode = copy_mode.clone();
        *cleanup_at_unix_ms = 0;
    }

    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    agent_screen.feed(b"agent surface");
    service.set_agent_pane_screen("%1", &conversation_id, agent_screen);

    service
        .render_client_view(ClientViewRole::Primary, Size::new(20, 4).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(service.deferred_word_copy_cleanup().borrow().is_none());
}
