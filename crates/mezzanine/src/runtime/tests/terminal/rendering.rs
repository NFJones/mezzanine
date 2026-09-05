//! Runtime tests for terminal rendering behavior.

use super::*;

/// Verifies exact-client rendering rejects identities that are not currently
/// attached instead of falling back to the session's compatibility focus.
#[test]
fn runtime_exact_client_render_rejects_stale_identity() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let stale = mez_core::ids::ClientId::new('c', 99_999);
    let error = service
        .render_client_view_for_client_with_resolved_config(
            &stale,
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap_err();

    assert!(error.to_string().contains("attached client"), "{error}");
}

/// Verifies an attached observer follows its exact source primary's
/// caller-local navigation rather than the deciding primary or landing view.
#[test]
fn runtime_observer_render_follows_exact_source_navigation() {
    let mut service = test_runtime_service();
    let source = service
        .attach_primary("source", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let landing_window = service
        .session()
        .active_window_for(&source)
        .unwrap()
        .id
        .clone();
    let source_window = service.session.new_window(&source, "source", true).unwrap();
    let source_pane = service
        .session()
        .active_pane_for(&source)
        .unwrap()
        .id
        .clone();
    let decider = service
        .attach_primary("decider", true, Size::new(80, 24).unwrap(), 121)
        .unwrap();
    service
        .session
        .select_window(&decider, landing_window.as_str())
        .unwrap();
    let decider_window = service
        .session()
        .active_window_for(&decider)
        .unwrap()
        .id
        .clone();
    let landing_pane = service
        .session()
        .active_pane_for(&decider)
        .unwrap()
        .id
        .clone();
    assert_ne!(source_window, decider_window);
    assert_ne!(source_pane, landing_pane);

    let source_size = service
        .session()
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .find(|pane| pane.id == source_pane)
        .unwrap()
        .size;
    let landing_size = service
        .session()
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .find(|pane| pane.id == landing_pane)
        .unwrap()
        .size;
    let mut source_screen = TerminalScreen::new(source_size, 120).unwrap();
    source_screen.feed(b"source-pane");
    service.set_pane_screen(source_pane.to_string(), source_screen);
    let mut landing_screen = TerminalScreen::new(landing_size, 120).unwrap();
    landing_screen.feed(b"decider-pane");
    service.set_pane_screen(landing_pane.to_string(), landing_screen);

    let observer_client = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    service
        .prepare_client_render(&observer_client, ClientViewRole::Observer)
        .unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig {
            window_frames_enabled: false,
            pane_frames_enabled: false,
            ..TerminalClientLoopConfig::default()
        })
        .unwrap();
    let view = service
        .render_client_view_for_client_with_resolved_config(
            &observer_client,
            ClientViewRole::Observer,
            Size::new(80, 24).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();

    assert_eq!(view.role, ClientViewRole::Observer);
    assert_eq!(service.session().active_window().unwrap().id, source_window);
    assert_ne!(
        service.session().active_window().unwrap().id,
        decider_window
    );
    assert_eq!(
        service.session().observer_attachments()[0].view_source_client_id,
        source
    );
}

/// Verifies pane output wakes only primaries projecting its window and the
/// observers attached to those exact primary projections.
#[test]
fn runtime_pane_output_targets_projecting_primary_and_observer_only() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    let source = service.attach_primary("source", true, size, 120).unwrap();
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    let unrelated = service
        .attach_primary("unrelated", true, size, 121)
        .unwrap();
    service
        .session
        .new_window(&unrelated, "background", true)
        .unwrap();

    let transition = service
        .apply_pane_output_transition("%1", b"source output".to_vec())
        .unwrap();
    let rendered_client_ids = transition
        .side_effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::RenderClient { client_id, .. } => Some(client_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(transition.applied);
    assert_eq!(rendered_client_ids, vec![source, observer]);
}

/// Verifies a structural pane mutation wakes every projection of its affected
/// window, including source-bound observers, while unrelated windows remain idle.
#[test]
fn runtime_structural_step_targets_affected_window_projections_only() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    let source = service.attach_primary("source", true, size, 120).unwrap();
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    let same_window = service
        .attach_primary("same-window", true, size, 121)
        .unwrap();
    let unrelated = service
        .attach_primary("unrelated", true, size, 122)
        .unwrap();
    service
        .session
        .new_window(&unrelated, "background", true)
        .unwrap();

    let (_, transition) = service
        .apply_attached_terminal_step_transition(
            &source,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(
                    MuxAction::SplitPaneVertical,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let rendered_client_ids = transition
        .side_effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::RenderClient { client_id, .. } => Some(client_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(transition.applied);
    assert_eq!(rendered_client_ids, vec![source, observer, same_window]);
}

/// Verifies a changed divider is projected only to its drag owner over a blank
/// window body, then commits once to every client projecting the resized
/// window. The owner's rendered divider and mouse hit cells must use the same
/// live geometry throughout the deferred content redraw.
#[test]
fn runtime_divider_commit_targets_projecting_clients_after_debounce() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    let source = service.attach_primary("source", true, size, 120).unwrap();
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    let same_window = service
        .attach_primary("same-window", true, size, 121)
        .unwrap();
    let unrelated = service
        .attach_primary("unrelated", true, size, 122)
        .unwrap();
    service
        .session
        .new_window(&unrelated, "background", true)
        .unwrap();
    service
        .session
        .split_active_pane(&source, SplitDirection::Vertical)
        .unwrap();
    let border = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .mouse_border_cells
        .into_iter()
        .next()
        .expect("vertical split should expose a divider");
    let baseline = service
        .render_client_view(
            ClientViewRole::Primary,
            size,
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .expect("split window should render");
    let presentation_plan = mez_mux::presentation::plan_window_presentation(
        service.session.active_window().unwrap(),
        mez_mux::presentation::WindowPresentationOptions {
            group_frame_visible: service.session.window_groups().len() > 1,
            window_frame_visible: service.window_frames_enabled(),
            window_frame_position: service.window_frame_position(),
            pane_frames_visible: service.pane_frames_enabled(),
            pane_frame_position: service.pane_frame_position(),
        },
    )
    .expect("split window should have a presentation plan");
    let moved_column = border.column.saturating_add(3);
    let drag = |column| AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::ResizePane {
                column,
                row: border.row,
            },
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    for column in [border.column, moved_column] {
        let (_, transition) = service
            .apply_attached_terminal_step_transition(&source, &drag(column))
            .unwrap();
        assert_eq!(
            transition.side_effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: source.clone(),
                reason: crate::runtime::RenderInvalidationReason::ResizeDrag,
            }]
        );
    }

    let live_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(
        live_config
            .mouse_border_cells
            .iter()
            .any(|cell| { cell.column == moved_column && cell.row == border.row })
    );
    assert!(
        !live_config
            .mouse_border_cells
            .iter()
            .any(|cell| { cell.column == border.column && cell.row == border.row })
    );
    let live_border_press = format!(
        "\u{1b}[<0;{};{}M",
        moved_column.saturating_add(1),
        border.row.saturating_add(1),
    );
    assert_eq!(
        crate::host::terminal::route_client_input(live_border_press.as_bytes(), &live_config)
            .unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane {
            column: moved_column,
            row: border.row,
        })
    );
    let provisional = service
        .render_client_view(ClientViewRole::Primary, size, &live_config)
        .unwrap()
        .expect("drag owner should receive a provisional divider view");
    let provisional_row = &provisional.lines[usize::from(border.row)];
    assert_eq!(
        mez_mux::render::line_slice(
            &baseline.lines[usize::from(border.row)],
            usize::from(border.column),
            usize::from(border.column).saturating_add(1),
        ),
        "│"
    );
    assert_eq!(
        mez_mux::render::line_slice(
            provisional_row,
            usize::from(border.column),
            usize::from(border.column).saturating_add(1),
        ),
        " "
    );
    assert_eq!(
        mez_mux::render::line_slice(
            provisional_row,
            usize::from(moved_column),
            usize::from(moved_column).saturating_add(1),
        ),
        "│"
    );
    let body_row_start = usize::from(presentation_plan.body_row_offset);
    let body_row_end = body_row_start.saturating_add(usize::from(presentation_plan.body_size.rows));
    let body_columns = usize::from(presentation_plan.body_size.columns);
    for row in body_row_start..body_row_end {
        for column in 0..body_columns {
            if column == usize::from(moved_column) {
                continue;
            }
            assert_eq!(
                mez_mux::render::line_slice(
                    &provisional.lines[row],
                    column,
                    column.saturating_add(1),
                ),
                " ",
                "provisional body was not blank at row {row}, column {column}"
            );
        }
        assert_eq!(
            provisional.line_style_spans[row]
                .iter()
                .filter(|span| span.start != usize::from(moved_column))
                .count(),
            0,
            "provisional body retained non-divider styles at row {row}"
        );
    }
    for row in 0..provisional.lines.len() {
        if (body_row_start..body_row_end).contains(&row) {
            continue;
        }
        assert_eq!(provisional.lines[row], baseline.lines[row]);
        assert_eq!(
            provisional.line_style_spans[row],
            baseline.line_style_spans[row]
        );
    }
    assert!(!provisional.cursor_visible);
    assert_eq!(provisional.selection, None);

    let mut output_effects = service
        .apply_pane_output_transition("%1", b"output during drag".to_vec())
        .unwrap()
        .side_effects;
    service.reconcile_pending_divider_render_effects(&mut output_effects);
    assert!(
        output_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
    );

    let release = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::FinishResizePane,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    let (_, transition) = service
        .apply_attached_terminal_step_transition(&source, &release)
        .unwrap();
    assert!(
        transition
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
    );

    let commit = service
        .apply_resize_debounce_timer_transition(source.as_str(), true)
        .unwrap();
    let rendered_client_ids = commit
        .side_effects
        .iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::RenderClient {
                client_id,
                reason: crate::runtime::RenderInvalidationReason::FullRedraw,
            } => Some(client_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(commit.applied);
    assert_eq!(rendered_client_ids.len(), 3);
    assert!(rendered_client_ids.contains(&source));
    assert!(rendered_client_ids.contains(&observer));
    assert!(rendered_client_ids.contains(&same_window));
    assert!(!rendered_client_ids.contains(&unrelated));
}

/// Verifies horizontal divider movement also blanks the complete window body
/// except for the live divider projection.
#[test]
fn runtime_horizontal_divider_drag_projects_blank_body() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    let primary = service.attach_primary("primary", true, size, 120).unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Horizontal)
        .unwrap();
    let border = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .mouse_border_cells
        .into_iter()
        .next()
        .expect("horizontal split should expose a divider");
    let presentation_plan = mez_mux::presentation::plan_window_presentation(
        service.session.active_window().unwrap(),
        mez_mux::presentation::WindowPresentationOptions {
            group_frame_visible: service.session.window_groups().len() > 1,
            window_frame_visible: service.window_frames_enabled(),
            window_frame_position: service.window_frame_position(),
            pane_frames_visible: service.pane_frames_enabled(),
            pane_frame_position: service.pane_frame_position(),
        },
    )
    .expect("split window should have a presentation plan");
    let moved_row = border.row.saturating_add(2);
    let drag = |row| AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::ResizePane {
                column: border.column,
                row,
            },
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    for row in [border.row, moved_row] {
        service
            .apply_attached_terminal_step_transition(&primary, &drag(row))
            .unwrap();
    }

    let live_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let provisional = service
        .render_client_view(ClientViewRole::Primary, size, &live_config)
        .unwrap()
        .expect("drag owner should receive a provisional divider view");
    assert_eq!(
        mez_mux::render::line_slice(
            &provisional.lines[usize::from(border.row)],
            usize::from(border.column),
            usize::from(border.column).saturating_add(1),
        ),
        " "
    );
    assert_eq!(
        mez_mux::render::line_slice(
            &provisional.lines[usize::from(moved_row)],
            usize::from(border.column),
            usize::from(border.column).saturating_add(1),
        ),
        "─"
    );
    let body_row_start = usize::from(presentation_plan.body_row_offset);
    let body_row_end = body_row_start.saturating_add(usize::from(presentation_plan.body_size.rows));
    let body_columns = usize::from(presentation_plan.body_size.columns);
    for row in body_row_start..body_row_end {
        for column in 0..body_columns {
            if live_config
                .mouse_border_cells
                .iter()
                .any(|cell| usize::from(cell.row) == row && usize::from(cell.column) == column)
            {
                continue;
            }
            assert_eq!(
                mez_mux::render::line_slice(
                    &provisional.lines[row],
                    column,
                    column.saturating_add(1),
                ),
                " ",
                "provisional body was not blank at row {row}, column {column}"
            );
        }
        if row != usize::from(moved_row) {
            assert!(
                provisional.line_style_spans[row].is_empty(),
                "provisional body retained styles at row {row}"
            );
        }
    }
}

/// Verifies returning a divider to its starting position and detaching a drag
/// owner both leave no delayed layout redraw behind.
#[test]
fn runtime_divider_noop_and_disconnect_do_not_commit_layout() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    let primary = service.attach_primary("primary", true, size, 120).unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let border = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .mouse_border_cells
        .into_iter()
        .next()
        .expect("vertical split should expose a divider");
    let step = |action| AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(action)],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    for column in [
        border.column,
        border.column.saturating_add(3),
        border.column,
    ] {
        service
            .apply_attached_terminal_step_transition(
                &primary,
                &step(MouseAction::ResizePane {
                    column,
                    row: border.row,
                }),
            )
            .unwrap();
    }
    service
        .apply_attached_terminal_step_transition(&primary, &step(MouseAction::FinishResizePane))
        .unwrap();
    let no_change = service
        .apply_resize_debounce_timer_transition(primary.as_str(), true)
        .unwrap();
    assert!(!no_change.applied);
    assert!(no_change.side_effects.is_empty());

    for column in [border.column, border.column.saturating_add(2)] {
        service
            .apply_attached_terminal_step_transition(
                &primary,
                &step(MouseAction::ResizePane {
                    column,
                    row: border.row,
                }),
            )
            .unwrap();
    }
    service
        .apply_attached_terminal_step_transition(&primary, &step(MouseAction::FinishResizePane))
        .unwrap();
    for column in [
        border.column.saturating_add(2),
        border.column.saturating_add(2),
    ] {
        service
            .apply_attached_terminal_step_transition(
                &primary,
                &step(MouseAction::ResizePane {
                    column,
                    row: border.row,
                }),
            )
            .unwrap();
    }
    service
        .apply_attached_terminal_step_transition(&primary, &step(MouseAction::FinishResizePane))
        .unwrap();
    let chained_commit = service
        .apply_resize_debounce_timer_transition(primary.as_str(), true)
        .unwrap();
    assert!(chained_commit.applied);
    assert!(chained_commit.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            client_id,
            reason: crate::runtime::RenderInvalidationReason::FullRedraw,
        } if client_id == &primary
    )));

    for column in [border.column, border.column.saturating_add(2)] {
        service
            .apply_attached_terminal_step_transition(
                &primary,
                &step(MouseAction::ResizePane {
                    column,
                    row: border.row,
                }),
            )
            .unwrap();
    }
    service
        .apply_attached_terminal_step_transition(&primary, &step(MouseAction::FinishResizePane))
        .unwrap();
    assert!(
        service
            .apply_client_disconnect_event(&primary, "divider owner disconnected")
            .unwrap()
    );
    let detached = service
        .apply_resize_debounce_timer_transition(primary.as_str(), true)
        .unwrap();
    assert!(!detached.applied);
    assert!(detached.side_effects.is_empty());
}

/// Verifies primary and observer frames render the visibility-selected screen
/// without allowing hidden process application modes to affect agent input.
#[test]
fn runtime_render_uses_selected_surface_and_process_protocol_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let size = Size::new(80, 24).unwrap();
    let mut process_screen = TerminalScreen::new(size, 120).unwrap();
    process_screen.feed(
        b"process-only\r\n\x1b[?1h\x1b[?1004h\x1b[?2004h\x1b=\x1b[?1049h\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\nprocess-alternate",
    );
    service.set_process_pane_screen("%1", process_screen);
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let agent_size = Size::new(size.columns, size.rows - 1).unwrap();
    let mut agent_screen = TerminalScreen::new(agent_size, 120).unwrap();
    agent_screen.feed(
        b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\nagent-only",
    );
    service.set_agent_pane_screen("%1", conversation_id, agent_screen);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let agent_config = service.terminal_client_loop_config(config.clone()).unwrap();
    assert!(!agent_config.mouse_policy.pane_application_cursor_mode);
    assert!(!agent_config.mouse_policy.pane_application_keypad_mode);
    assert!(agent_config.pane_bracketed_paste_mode);
    assert_eq!(
        crate::host::terminal::route_client_input(b"\x1b[A", &agent_config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())
    );

    for role in [ClientViewRole::Primary, ClientViewRole::Observer] {
        let view = service
            .render_client_view(role, size, &config)
            .unwrap()
            .unwrap();
        let text = view.lines.join("\n");
        assert!(text.contains("agent-only"), "{role:?}: {text}");
        assert!(!text.contains("process-only"), "{role:?}: {text}");
        assert!(!text.contains("process-alternate"), "{role:?}: {text}");
        assert!(!view.focus_events, "{role:?}");
        assert!(!view.alternate_screen, "{role:?}");
        assert!(!view.application_keypad, "{role:?}");
        assert!(view.bracketed_paste, "{role:?}");
        assert_eq!(
            view.readline_input_active,
            role == ClientViewRole::Primary,
            "{role:?}"
        );
    }

    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let process_config = service.terminal_client_loop_config(config.clone()).unwrap();
    assert!(process_config.mouse_policy.pane_application_cursor_mode);
    assert!(process_config.mouse_policy.pane_application_keypad_mode);
    assert!(process_config.pane_bracketed_paste_mode);
    assert_eq!(
        crate::host::terminal::route_client_input(b"\x1b[A", &process_config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"\x1bOA".to_vec())
    );
    let process_view = service
        .render_client_view(ClientViewRole::Primary, size, &config)
        .unwrap()
        .unwrap();
    assert!(!process_view.readline_input_active);
    let process_text = process_view.lines.join("\n");
    assert!(process_text.contains("process-alternate"), "{process_text}");
    assert!(!process_text.contains("agent-only"), "{process_text}");
    assert!(process_view.focus_events);
    assert!(process_view.alternate_screen);
    assert!(process_view.application_keypad);
    assert!(process_view.bracketed_paste);
}

/// Verifies that frame-context animation stays static when no live agent footer
/// is visible in the active window. This keeps idle redraws from paying for
/// animated footer state when agent mode is inactive or quiescent.
#[test]
fn runtime_frame_context_disables_animation_without_live_agent_footer() {
    let service = test_runtime_service();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}

/// Verifies unchanged pane screens reuse immutable styled row projections and
/// any subsequent terminal mutation invalidates the generation-keyed entry.
#[test]
fn runtime_render_reuses_unchanged_pane_styled_rows_by_generation() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let size = Size::new(80, 24).unwrap();
    let mut screen = TerminalScreen::new(size, 120).unwrap();
    screen.feed(b"cached rows");
    service.set_pane_screen(pane_id.clone(), screen);
    let config = TerminalClientLoopConfig::default();

    service
        .render_client_view(ClientViewRole::Primary, size, &config)
        .unwrap();
    assert_eq!(service.pane_styled_row_cache_stats_for_tests(), (0, 1, 1));

    service
        .render_client_view(ClientViewRole::Primary, size, &config)
        .unwrap();
    assert_eq!(service.pane_styled_row_cache_stats_for_tests(), (1, 1, 1));

    service.pane_screen_mut(&pane_id).unwrap().feed(b" changed");
    service
        .render_client_view(ClientViewRole::Primary, size, &config)
        .unwrap();
    assert_eq!(service.pane_styled_row_cache_stats_for_tests(), (1, 2, 1));
}

/// Verifies repeated presentation queries reuse one bounded window snapshot,
/// while a geometry mutation replaces that entry instead of returning stale
/// pane regions.
#[test]
fn runtime_window_presentation_plan_cache_tracks_geometry_changes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let initial_window = service.session().active_window().unwrap().clone();

    let initial = service
        .window_presentation_plan_for_tests(&initial_window)
        .unwrap();
    let repeated = service
        .window_presentation_plan_for_tests(&initial_window)
        .unwrap();

    assert!(std::sync::Arc::ptr_eq(&initial, &repeated));
    assert_eq!(
        service.window_presentation_plan_cache_stats_for_tests(),
        (1, 1, 1)
    );

    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    let split_window = service.session().active_window().unwrap().clone();
    let split = service
        .window_presentation_plan_for_tests(&split_window)
        .unwrap();

    assert!(!std::sync::Arc::ptr_eq(&initial, &split));
    assert_eq!(split.panes.len(), 2);
    assert_eq!(
        service.window_presentation_plan_cache_stats_for_tests(),
        (1, 2, 1)
    );
}

/// Verifies zen mode suppresses configured frame rows and their mouse targets.
///
/// Configured frame flags remain available for restoration, while the shared
/// presentation plan must reclaim both standalone rows and expose the full
/// single-pane body without any clickable passive chrome.
#[test]
fn runtime_zen_mode_reclaims_frame_rows_without_changing_configured_flags() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_terminal_zen_mode_for_tests(true);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let window = service.session().active_window().unwrap();
    let plan = service.window_presentation_plan_for_tests(window).unwrap();

    assert!(service.window_frames_enabled());
    assert!(service.pane_frames_enabled());
    assert!(!config.window_frames_enabled);
    assert!(!config.pane_frames_enabled);
    assert_eq!(plan.window_frame_row, None);
    assert_eq!(plan.panes[0].frame_row, None);
    assert_eq!(plan.panes[0].content_region.rows, 24);
    assert!(config.mouse_window_frame_cells.is_empty());
    assert!(config.mouse_window_action_frame_cells.is_empty());
    assert!(config.mouse_window_group_frame_cells.is_empty());
    assert!(config.mouse_pane_agent_status_cells.is_empty());
}

/// Verifies that a live agent footer re-enables animated frame ticks so active
/// agent progress indicators keep their motion while work is still running.
#[test]
fn runtime_frame_context_animates_live_agent_footer() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service.mark_agent_compacting_for_tests(pane_id, 1);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(config.frame_context.animation_tick_ms > 0);
}

/// Verifies that active pane-frame agent status enables animation even when
/// the agent shell footer is not visible. Pane headers and live footers share
/// the same frame tick, so header-only status indicators must not freeze when
/// no prompt overlay is being rendered.
#[test]
fn runtime_frame_context_animates_active_agent_status_without_live_footer() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_turn_ledger_mut()
        .start_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-running".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            deadline_at_unix_millis: 0,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Running,

            initial_capability: None,
        })
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("running"));
    assert!(pane_context.agent_prompt.is_none());
    assert!(config.frame_context.animation_tick_ms > 0);
}

/// Verifies that frame context renders the real normalized exit status when a
/// non-live pane has known exit metadata. This prevents pane frames from
/// collapsing all exited processes into a generic `exited` placeholder.
#[test]
fn runtime_frame_context_uses_known_pane_exit_status() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .session
        .set_pane_live_state(&pane_id, false)
        .unwrap();
    service.set_pane_exit_status_for_tests(
        pane_id.clone(),
        mez_mux::process::PaneExitStatus {
            code: Some(7),
            signal: None,
            success: false,
        },
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.exit_status.as_deref(), Some("exit=7"));
}

/// Verifies that pane-frame runtime context includes the best known current
/// working directory in the compact home-relative form used by the status
/// pill. This keeps the renderer independent from process probing while still
/// giving users location context when shell prompts are hidden or overwritten.
#[test]
fn runtime_frame_context_reports_home_relative_pane_working_directory() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service.set_pane_current_working_directory(pane_id.clone(), path);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(
        pane_context.current_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        config
            .frame_context
            .window_status
            .as_ref()
            .and_then(|status| status.active_pane_working_directory.as_deref()),
        Some(expected.as_str())
    );
}

/// Verifies that deep pane working directories collapse to the last three path
/// segments in the default window status. This keeps the footer compact while
/// still surfacing the most actionable cwd context for narrow frame rows.
#[test]
fn runtime_frame_context_compacts_deep_pane_working_directory() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine/src/runtime"))
        .unwrap_or_else(|| PathBuf::from("/tmp/worktrees/mez/src/runtime"));
    service.set_pane_current_working_directory(pane_id.clone(), path);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(
        pane_context.current_working_directory.as_deref(),
        Some("…/mezzanine/src/runtime")
    );
    assert_eq!(
        config
            .frame_context
            .window_status
            .as_ref()
            .and_then(|status| status.active_pane_working_directory.as_deref()),
        Some("…/mezzanine/src/runtime")
    );
}

/// Verifies that frame context leaves unused dynamic right-status fields empty
/// when the configured template only references pane working-directory data.
/// This avoids repeated uptime and datetime formatting work on redraws that do
/// not display those fields.
#[test]
fn runtime_frame_context_skips_unused_dynamic_window_status_fields() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nright_status = \"#{pane.pwd}\"\n".to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service.set_pane_current_working_directory(pane_id, path);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let status = config.frame_context.window_status.as_ref().unwrap();
    assert_eq!(
        status.active_pane_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert!(status.system_uptime.is_empty());
    assert!(status.datetime_local.is_empty());
}

/// Verifies that the pane-frame status reports compaction as its own active
/// running substate. Compaction is provider work, but it is distinct enough
/// from ordinary response generation that users need a direct state label.
#[test]
fn runtime_frame_context_reports_agent_compacting_substate() {
    let mut service = test_runtime_service();
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
    service
        .agent_turn_ledger_mut()
        .start_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-completed".to_string(),
            conversation_id: "conversation-1".to_string(),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            deadline_at_unix_millis: 0,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,

            initial_capability: None,
        })
        .unwrap();
    service
        .agent_turn_ledger_mut()
        .finish_turn("turn-completed", AgentTurnState::Completed)
        .unwrap();
    service.mark_agent_compacting_for_tests(pane_id.clone(), 1);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("compacting"));
    assert_eq!(
        config
            .frame_context
            .window_agent_active_counts
            .get(service.session().active_window().unwrap().id.as_str())
            .copied(),
        Some(1)
    );
}

/// Verifies pane context usage percentages for named OpenAI-compatible
/// providers use live provider-catalog context windows instead of the generic
/// fallback denominator.
#[test]
fn runtime_frame_context_uses_cached_catalog_context_window_for_named_compatible_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"compat\"\ndefault_model_profile = \"work\"\n[providers.compat]\nkind = \"openai-compatible\"\nmodels = [\"baseline-model\"]\ndefault_model = \"baseline-model\"\n[model_profiles.work]\nprovider = \"compat\"\nmodel = \"baseline-model\"\n"
                .to_string(),
        }])
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "compat",
        vec![mez_agent::ProviderModelInfo {
            id: "catalog-only-model".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string()],
            context_window_tokens: Some(2_000_000),
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: Vec::new(),
        }],
        vec!["low".to_string()],
    );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
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
    let model_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compatible-model","method":"agent/shell/command","params":{"idempotency_key":"compatible-model","input":"/model catalog-only-model"}}"#,
        &primary,
    );
    assert!(
        model_response.contains("catalog-only-model"),
        "{model_response}"
    );

    service.record_agent_provider_token_usage(
        &pane_id,
        mez_agent::ModelTokenUsage {
            input_tokens: 500_000,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_context_usage.as_deref(), Some("25%"));
}

/// Verifies mouse drag selection copies the visible alternate-screen grid.
///
/// Full-screen terminal applications are intentionally excluded from normal
/// history and copy-mode buffers, but an explicit mouse drag is a user copy
/// operation over the displayed pane body. This regression protects less/nano
/// style alternate-screen copying without making alternate-screen content part
/// of scrollback or default agent context.
#[test]
fn runtime_mouse_drag_copies_visible_alternate_screen_content() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"normal-only\r\n\x1b[?1049halpha beta\r\nsecond row");
    assert!(screen.alternate_screen_active());
    assert!(
        !screen
            .normal_content_lines()
            .iter()
            .any(|line| line.contains("alpha beta"))
    );
    service.set_pane_screen(pane_id.clone(), screen);

    let application = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionStart(
                        CopyPosition { line: 0, column: 0 },
                    )),
                    TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionFinish(
                        CopyPosition { line: 1, column: 6 },
                    )),
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
        service.paste_buffers().get("mouse"),
        Some("alpha beta\nsecond")
    );
    assert_eq!(
        application
            .client_clipboard_write
            .map(|write| write.into_content()),
        Some("alpha beta\nsecond".to_string())
    );
    assert!(
        service
            .active_copy_mode_for_presented_surface(&pane_id)
            .is_none()
    );
}

/// Verifies a drag started on the process surface does not remain active after
/// the pane switches to its retained agent surface.
#[test]
fn runtime_mouse_drag_activation_is_scoped_to_presented_surface() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut process_screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    process_screen.feed(b"process selection text");
    service.set_process_pane_screen(&pane_id, process_screen);

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::CopySelectionStart(CopyPosition { line: 0, column: 0 }),
                )],
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
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap()
            .mouse_selection_active
    );

    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    agent_screen.feed(b"agent selection text");
    service.set_agent_pane_screen(&pane_id, &conversation_id, agent_screen);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(!config.mouse_selection_active);
    assert!(!config.mouse_policy.copy_mode_active);
}

/// Verifies mouse focus uses the same pane-frame row accounting as rendering.
///
/// A top pane frame that is merged into an interior divider does not consume the
/// first content row of the pane below it. Mouse targeting must therefore allow a
/// click on that first rendered content row to focus the lower pane instead of
/// treating the row as an inert frame.
#[test]
fn runtime_mouse_focus_targets_content_below_merged_top_pane_frame() {
    let mut service = test_runtime_service_with_size(Size::new(20, 8).unwrap());
    service.set_frame_visibility_for_tests(false, true);
    service.set_pane_frame_position_for_tests(mez_mux::presentation::TerminalFramePosition::Top);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneHorizontal)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::FocusPaneOnly(CopyPosition { line: 4, column: 0 }),
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
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the pane agent status latency selector opens, populates with
/// the three allowed latency values, applies a selection as a pane-local
/// override, closes after selection, and surfaces the latency value in the
/// pane-frame context for pill rendering.
#[test]
fn runtime_pane_agent_status_selector_applies_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\nlatency_preference = \"default\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
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
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string()],
            context_window_tokens: Some(1_050_000),
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: Vec::new(),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
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
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    let latency_items = service
        .pane_agent_status_selector()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    assert_eq!(
        latency_items,
        vec![
            "slow".to_string(),
            "default".to_string(),
            "fast".to_string()
        ]
    );
    let fast_index = latency_items
        .iter()
        .position(|item| item == "fast")
        .expect("latency selector should include fast");
    let select_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                        item_index: fast_index,
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
    assert!(select_report.view_refresh_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_name, latency_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(latency_profile.model, "gpt-5.5");
    assert_eq!(latency_profile.reasoning_profile.as_deref(), Some("low"));
    assert_eq!(latency_profile.latency_preference.as_deref(), Some("fast"));

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_latency.as_deref(), Some("fast"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("low"));
}

/// Verifies that pane-frame latency controls are hidden for providers that do
/// not support a provider-visible latency preference.
///
/// DeepSeek profiles can still carry `latency_preference` metadata for identity
/// and preset display, but exposing a clickable latency selector would suggest
/// a provider request behavior that DeepSeek does not implement.
#[test]
fn runtime_pane_agent_status_hides_latency_for_unsupported_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "deepseek"
default_model_profile = "deepseek-default"

[providers.deepseek]
kind = "deepseek"
models = ["deepseek-v4-pro"]
default_model = "deepseek-v4-pro"

[model_profiles.deepseek-default]
provider = "deepseek"
model = "deepseek-v4-pro"
reasoning_profile = "high"
latency_preference = "fast"

[model_profiles.deepseek-default.provider_options]
reasoning_effort = "high"
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

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        pane_context.agent_latency, None,
        "unsupported providers should not render a latency status pill"
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
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
    assert!(
        service.pane_agent_status_selector().is_none(),
        "unsupported providers should not expose a latency dropdown"
    );
}

/// Verifies the DeepSeek thinking status pill is an immediate toggle rather
/// than a dropdown selector.
///
/// The pane frame exposes thinking next to reasoning only when the provider
/// supports the capability. Clicking it should reuse the `/thinking toggle`
/// runtime mutation path, update the pane-local generated profile, and refresh
/// the frame context without opening selector state.
#[test]
fn runtime_pane_agent_status_thinking_pill_toggles_deepseek_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[model_profiles.default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
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

    let first_toggle = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Thinking,
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
    assert!(first_toggle.view_refresh_required);
    assert!(!first_toggle.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_off_name, off_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(off_profile.thinking_enabled(), Some(false));
    let off_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        off_config
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_thinking.as_deref()),
        Some("off")
    );

    let second_toggle = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Thinking,
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
    assert!(second_toggle.view_refresh_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_on_name, on_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(on_profile.thinking_enabled(), Some(true));
}

/// Verifies the plan status pill is an immediate pane-local toggle rather than
/// a dropdown selector.
///
/// Clicking must reuse `/plan toggle` so the canonical planning state, command
/// response, frame projection, and running-turn safety semantics stay aligned.
#[test]
fn runtime_pane_agent_status_planning_pill_toggles_plan_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let initial = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        initial
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_planning.as_deref()),
        Some("off")
    );

    for expected in ["on", "off"] {
        let report = service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(
                        MouseAction::OpenPaneAgentStatusSelector {
                            pane_index: 0,
                            field: PaneAgentStatusField::Planning,
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
        assert!(service.pane_agent_status_selector().is_none());
        assert_eq!(service.agent_planning_enabled("%1"), expected == "on");
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        assert_eq!(
            config
                .frame_context
                .panes
                .get("%1")
                .and_then(|pane| pane.agent_planning.as_deref()),
            Some(expected)
        );
    }
}

/// Verifies that pane-frame agent selectors remain modal until the user makes
/// an explicit selection or cancels them. Escape must close the selector
/// without leaking the escape byte into the active pane.
#[test]
fn runtime_pane_agent_status_selector_esc_closes_without_forwarding() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
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
    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
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
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_some());

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
    assert!(!report.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_none());
}

/// Verifies pane-frame model and reasoning dropdowns support keyboard
/// navigation. The active row should move with arrow input and Enter should
/// apply the same pane-scoped `/model` mutation as mouse selection.
#[test]
fn runtime_pane_agent_status_selector_accepts_keyboard_navigation() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
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
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
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
    let (active_index, target_index) = service
        .pane_agent_status_selector()
        .map(|selector| {
            (
                selector.active_index,
                selector
                    .items
                    .iter()
                    .position(|item| item == "openai: gpt-5.4")
                    .expect("model selector should include gpt-5.4"),
            )
        })
        .expect("model selector should be open");
    let movement = if target_index < active_index {
        b"\x1b[A".to_vec()
    } else {
        b"\x1b[B".to_vec()
    };

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(movement),
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
    assert!(!report.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_name, model_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(model_profile.model, "gpt-5.4");
}

/// Verifies mouse-wheel input over an open pane agent selector scrolls the
/// selector itself rather than falling through to pane scrollback.
#[test]
fn runtime_pane_agent_status_selector_scrolls_only_dropdown_contents() {
    let mut service = test_runtime_service();
    let models = (0..40)
        .map(|index| format!("\"gpt-test-{index:02}\""))
        .collect::<Vec<_>>()
        .join(", ");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [{models}]\ndefault_model = \"gpt-test-00\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-test-00\"\n"
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
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
    assert_eq!(
        service
            .pane_agent_status_selector()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: 3,
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

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .pane_agent_status_selector()
            .map(|selector| selector.scroll_offset),
        Some(3)
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: -30,
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
    assert_eq!(
        service
            .pane_agent_status_selector()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );
}

/// Verifies fragmented synchronized pane output mutates the live
/// terminal while attached clients retain the pre-transaction frame
/// until the final DEC reset publishes once.
#[test]
fn runtime_synchronized_output_defers_renders_and_publishes_atomically() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service.attach_primary("primary", true, size, 120).unwrap();

    service
        .apply_pane_output_transition("%1", b"before".to_vec())
        .unwrap();

    let deferred_begin = service
        .apply_pane_output_transition("%1", b"\x1b[?2026h\r\x1b[2J\x1b[?25lapproach".to_vec())
        .unwrap();
    assert!(deferred_begin.applied);
    assert!(
        deferred_begin
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
    );

    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let frozen = service
        .render_client_view(ClientViewRole::Primary, size, &config)
        .unwrap()
        .unwrap();
    assert!(frozen.lines.join("\n").contains("before"));
    assert!(!frozen.lines.join("\n").contains("approach"));
    assert!(frozen.cursor_visible);

    let deferred_repaint = service
        .apply_pane_output_transition("%1", b"\x1b[1;1H partial".to_vec())
        .unwrap();
    assert!(
        deferred_repaint
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
    );

    let release = service
        .apply_pane_output_transition("%1", b"\x1b[?2026l".to_vec())
        .unwrap();
    assert!(matches!(
        release.side_effects.as_slice(),
        [RuntimeSideEffect::RenderClient {
            reason: crate::runtime::RenderInvalidationReason::PaneOutput,
            ..
        }]
    ));
    let completed = service
        .render_client_view(ClientViewRole::Primary, size, &config)
        .unwrap()
        .unwrap();
    assert!(completed.lines.join("\n").contains(" partial"));
    assert!(!completed.cursor_visible);
}

/// Verifies an alternate-buffer switch inside synchronized output releases the
/// frozen pane through a full redraw rather than a retained pane diff.
#[test]
fn runtime_synchronized_output_alternate_switch_requires_full_redraw() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service.attach_primary("primary", true, size, 120).unwrap();

    let deferred = service
        .apply_pane_output_transition("%1", b"\x1b[?2026h\x1b[?1049halt".to_vec())
        .unwrap();
    assert!(deferred.applied);
    assert!(
        deferred
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
    );

    let release = service
        .apply_pane_output_transition("%1", b"\x1b[?2026l".to_vec())
        .unwrap();
    assert!(matches!(
        release.side_effects.as_slice(),
        [RuntimeSideEffect::RenderClient {
            reason: crate::runtime::RenderInvalidationReason::FullRedraw,
            ..
        }]
    ));
}
