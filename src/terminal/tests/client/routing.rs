//! Regression tests for terminal client routing behavior.

use crate::terminal::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, BTreeMap, ClientStatusKind,
    ClientStatusLine, ClientViewRole, CopyModeKeyAction, MouseAction, MousePaneAgentSelectorCell,
    MousePaneAgentStatusCell, MouseWindowActionFrameCell, PaneAgentStatusField, RenderedClientView,
    Size, TerminalClientLoopAction, TerminalClientLoopConfig, TerminalCursorStyle,
    TerminalFdInterest, TerminalScreen, UiTheme, Window, WindowFrameAction,
    draw_window_from_screens, plan_attached_terminal_client_step, route_client_input,
    route_client_input_actions,
};
use mez_mux::copy::CopyPosition;
use mez_mux::input::{
    KeyChord, KeyCode, MouseBorderCell, MousePaneRegion, MouseWindowFrameCell,
    MouseWindowGroupFrameCell, MuxAction, key_chord_input_bytes,
};
use unicode_width::UnicodeWidthStr;

/// Verifies client loop routes input to pane mux and mouse actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_routes_input_to_pane_mux_and_mouse_actions() {
    let mut config = TerminalClientLoopConfig::default();

    assert_eq!(
        route_client_input(b"echo hi", &config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"echo hi".to_vec())
    );
    assert_eq!(
        route_client_input(b"\x01%", &config).unwrap(),
        TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical)
    );
    assert_eq!(
        route_client_input(b"\x01", &config).unwrap(),
        TerminalClientLoopAction::EnterPrefixKeyMode
    );
    config.command_bindings.insert(
        KeyChord::new(KeyCode::Char('x')),
        "split-window -h".to_string(),
    );
    assert_eq!(
        route_client_input(b"\x01x", &config).unwrap(),
        TerminalClientLoopAction::ExecuteCommand("split-window -h".to_string())
    );

    let mut mouse_config = config.clone();
    mouse_config.mouse_policy.over_pane_border = true;
    assert_eq!(
        route_client_input(b"\x1b[<32;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 11, row: 4 })
    );

    let mut border_config = config.clone();
    border_config.mouse_border_cells = vec![MouseBorderCell { column: 11, row: 4 }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &border_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 11, row: 4 })
    );

    let mut frame_config = config.clone();
    frame_config.mouse_window_frame_cells = vec![MouseWindowFrameCell {
        column: 11,
        row: 4,
        window_index: 2,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusWindow { index: 2 })
    );

    let mut group_frame_config = frame_config.clone();
    group_frame_config.mouse_window_group_frame_cells = vec![MouseWindowGroupFrameCell {
        column: 11,
        row: 4,
        group_index: 1,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &group_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusGroup { index: 1 })
    );

    let mut action_frame_config = frame_config.clone();
    action_frame_config.mouse_window_action_frame_cells = vec![MouseWindowActionFrameCell {
        column: 11,
        row: 4,
        action: WindowFrameAction::NewWindow,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &action_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::PressWindowAction {
            action: WindowFrameAction::NewWindow,
        })
    );
    action_frame_config.frame_context.pressed_window_action = Some(WindowFrameAction::NewWindow);
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5m", &action_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ReleaseWindowAction {
            action: WindowFrameAction::NewWindow,
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5m", &action_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::CancelWindowAction)
    );

    let mut pane_status_config = frame_config.clone();
    pane_status_config.mouse_pane_agent_status_cells = vec![MousePaneAgentStatusCell {
        column: 11,
        row: 4,
        pane_index: 0,
        field: PaneAgentStatusField::Model,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &pane_status_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::Ignore)
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5m", &pane_status_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::OpenPaneAgentStatusSelector {
            pane_index: 0,
            field: PaneAgentStatusField::Model,
        })
    );

    let mut pane_selector_config = frame_config.clone();
    pane_selector_config.mouse_pane_agent_selector_cells = vec![MousePaneAgentSelectorCell {
        column: 11,
        row: 4,
        pane_index: 0,
        field: PaneAgentStatusField::Reasoning,
        item_index: 2,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::HoverPaneAgentStatusSelector {
            pane_index: 0,
            field: PaneAgentStatusField::Reasoning,
            item_index: 2,
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5m", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::SelectPaneAgentStatusSelector {
            pane_index: 0,
            field: PaneAgentStatusField::Reasoning,
            item_index: 2,
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5M", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ClosePaneAgentStatusSelector)
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5m", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::Ignore)
    );
    let mut selector_with_window_frame_config = pane_selector_config.clone();
    selector_with_window_frame_config.mouse_window_frame_cells = vec![MouseWindowFrameCell {
        column: 12,
        row: 4,
        window_index: 1,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5M", &selector_with_window_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusWindow { index: 1 })
    );
    let mut selector_with_window_action_config = pane_selector_config.clone();
    selector_with_window_action_config.mouse_window_action_frame_cells =
        vec![MouseWindowActionFrameCell {
            column: 12,
            row: 4,
            action: WindowFrameAction::NewWindow,
        }];
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5M", &selector_with_window_action_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::PressWindowAction {
            action: WindowFrameAction::NewWindow,
        })
    );

    let mut display_overlay_config = frame_config.clone();
    display_overlay_config.primary_display_overlay_active = true;
    assert_eq!(
        route_client_input(b"\x1b[<0;4;3M", &display_overlay_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::BeginDisplayOverlaySelection {
            position: CopyPosition { line: 2, column: 3 },
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<64;4;3M", &display_overlay_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { lines: -3 })
    );

    mouse_config.mouse_policy.over_pane_border = false;
    mouse_config.mouse_pane_regions = vec![MousePaneRegion {
        pane_id: "%1".to_string(),
        column: 0,
        row: 0,
        columns: 40,
        rows: 20,
        application_sgr_mouse_mode: true,
        application_mouse_mode: true,
        copy_mode_active: false,
        active: true,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<0;12;5M".to_vec(),
        }
    );
    assert_eq!(
        route_client_input(b"\x1b[<2;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<2;12;5M".to_vec(),
        }
    );
    assert_eq!(
        route_client_input(b"\x1b[<65;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<65;12;5M".to_vec(),
        }
    );
    mouse_config.mouse_policy.pane_resize_active = true;
    assert_eq!(
        route_client_input(b"\x1b[<32;20;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 19, row: 4 })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;20;5m", &mouse_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FinishResizePane)
    );
}

/// Verifies that a previously entered prefix key state routes the next key
/// through the prefix table instead of opening the command prompt immediately.
///
/// This protects the split between the escape key and the command-prompt
/// binding so callers can keep the prefix state across terminal read frames.
#[test]
fn client_loop_routes_pending_prefix_key_to_prefix_table() {
    let config = TerminalClientLoopConfig {
        prefix_key_pending: true,
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b":", &config).unwrap(),
        TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCommandPrompt)
    );
}

/// Verifies that pending prefix state is consumed once and remaining bytes keep
/// their normal pane-forwarding behavior.
///
/// This regression scenario covers attached terminals that deliver the key
/// after the escape and pane text in the same read buffer.
#[test]
fn client_loop_consumes_pending_prefix_before_forwarding_remainder() {
    let config = TerminalClientLoopConfig {
        prefix_key_pending: true,
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input_actions(b"cabc", &config).unwrap(),
        vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::NewWindow),
            TerminalClientLoopAction::ForwardToPane(b"abc".to_vec()),
        ]
    );
}

/// Verifies that pane applications receive mouse input only inside their own
/// rendered content region. A mouse-aware program in one pane must not suppress
/// Mezzanine history scrolling or selection in neighboring panes.
#[test]
fn client_loop_scopes_application_mouse_forwarding_to_pane_regions() {
    let mut config = TerminalClientLoopConfig {
        mouse_pane_regions: vec![
            MousePaneRegion {
                pane_id: "%1".to_string(),
                column: 0,
                row: 1,
                columns: 39,
                rows: 20,
                application_sgr_mouse_mode: true,
                application_mouse_mode: true,
                copy_mode_active: false,
                active: true,
            },
            MousePaneRegion {
                pane_id: "%2".to_string(),
                column: 40,
                row: 1,
                columns: 40,
                rows: 20,
                application_sgr_mouse_mode: false,
                application_mouse_mode: false,
                copy_mode_active: false,
                active: false,
            },
        ],
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b"\x1b[<65;12;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<65;12;4M".to_vec(),
        }
    );
    assert_eq!(
        route_client_input(b"\x1b[<65;50;5M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ScrollHistory {
            lines: 3,
            position: CopyPosition {
                line: 4,
                column: 49,
            },
        })
    );

    config.mouse_border_cells = vec![MouseBorderCell { column: 39, row: 5 }];
    assert_eq!(
        route_client_input(b"\x1b[<0;40;6M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 39, row: 5 })
    );
}

/// Verifies full-screen pane regions do not capture mouse input unless the pane
/// application explicitly enables mouse tracking. Alternate-screen programs that
/// only draw a full-screen interface still leave wheel scrolling and drag-copy
/// routing owned by Mezzanine.
#[test]
fn client_loop_routes_full_screen_mouse_to_mux_until_application_mouse_is_enabled() {
    let mut config = TerminalClientLoopConfig {
        mouse_pane_regions: vec![MousePaneRegion {
            pane_id: "%1".to_string(),
            column: 4,
            row: 2,
            columns: 40,
            rows: 20,
            application_sgr_mouse_mode: false,
            application_mouse_mode: false,
            copy_mode_active: false,
            active: true,
        }],
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b"\x1b[<65;12;5M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ScrollHistory {
            lines: 3,
            position: CopyPosition {
                line: 4,
                column: 11,
            },
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<32;12;5M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionUpdate(CopyPosition {
            line: 4,
            column: 11,
        }))
    );

    config.mouse_pane_regions[0].application_mouse_mode = true;
    config.mouse_pane_regions[0].application_sgr_mouse_mode = true;
    assert_eq!(
        route_client_input(b"\x1b[<65;12;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<65;8;3M".to_vec(),
        }
    );
    assert_eq!(
        route_client_input(b"\x1b[<32;12;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<32;8;3M".to_vec(),
        }
    );
}

/// Verifies that the first button press in an unfocused mouse-aware pane is a
/// Mezzanine focus action instead of being forwarded to the previously focused
/// pane. After that focus update, later events in the same pane may be forwarded
/// to the pane application.
#[test]
fn client_loop_focuses_unfocused_mouse_region_before_forwarding() {
    let mut config = TerminalClientLoopConfig {
        mouse_pane_regions: vec![
            MousePaneRegion {
                pane_id: "%1".to_string(),
                column: 0,
                row: 1,
                columns: 39,
                rows: 20,
                application_sgr_mouse_mode: false,
                application_mouse_mode: false,
                copy_mode_active: false,
                active: true,
            },
            MousePaneRegion {
                pane_id: "%2".to_string(),
                column: 40,
                row: 1,
                columns: 40,
                rows: 20,
                application_sgr_mouse_mode: true,
                application_mouse_mode: true,
                copy_mode_active: false,
                active: false,
            },
        ],
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b"\x1b[<0;50;5M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusPaneOnly(CopyPosition {
            line: 4,
            column: 49,
        }))
    );

    config.mouse_pane_regions[0].active = false;
    config.mouse_pane_regions[1].active = true;
    assert_eq!(
        route_client_input(b"\x1b[<0;50;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%2".to_string(),
            input: b"\x1b[<0;10;4M".to_vec(),
        }
    );
}

/// Verifies that pane applications using legacy xterm mouse tracking without
/// SGR mode still receive mouse input. Ncurses programs under `screen-256color`,
/// including htop, commonly request DECSET 1000 without DECSET 1006 and expect
/// `ESC [ M` encoded coordinates local to the pane.
#[test]
fn client_loop_translates_sgr_host_mouse_to_legacy_xterm_pane_mouse() {
    let config = TerminalClientLoopConfig {
        mouse_pane_regions: vec![MousePaneRegion {
            pane_id: "%2".to_string(),
            column: 40,
            row: 1,
            columns: 40,
            rows: 20,
            application_sgr_mouse_mode: false,
            application_mouse_mode: true,
            copy_mode_active: false,
            active: true,
        }],
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b"\x1b[<65;50;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%2".to_string(),
            input: vec![b'\x1b', b'[', b'M', b'a', b'*', b'$'],
        }
    );
}

/// Verifies that a single terminal read containing multiple SGR mouse packets is
/// split into separate mux actions instead of being forwarded as pane input. Drag
/// reporting commonly arrives batched, and forwarding a malformed aggregate
/// sequence would print mouse escape bytes into the active shell.
#[test]
fn attached_terminal_client_step_splits_batched_mouse_sequences() {
    let config = TerminalClientLoopConfig {
        mouse_border_cells: vec![MouseBorderCell { column: 11, row: 4 }],
        ..TerminalClientLoopConfig::default()
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let step = plan_attached_terminal_client_step(
        &readiness,
        Some(b"\x1b[<0;12;5M\x1b[<32;20;5M\x1b[<0;20;5m"),
        None,
        None,
        &config,
    )
    .unwrap();

    assert_eq!(
        step.actions,
        vec![
            TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 11, row: 4 }),
            TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 19, row: 4 }),
            TerminalClientLoopAction::HandleMouse(MouseAction::FinishResizePane),
        ]
    );
}

/// Verifies that a drag selection keeps ownership after it crosses a rendered
/// pane border. Batched mouse reads must classify the border cell as a copy
/// update rather than starting a resize once the initial pane click has armed a
/// selection gesture.
#[test]
fn attached_terminal_client_step_keeps_selection_active_across_borders() {
    let config = TerminalClientLoopConfig {
        mouse_border_cells: vec![MouseBorderCell { column: 11, row: 4 }],
        ..TerminalClientLoopConfig::default()
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let step = plan_attached_terminal_client_step(
        &readiness,
        Some(b"\x1b[<0;2;3M\x1b[<32;12;5M\x1b[<0;12;5m"),
        None,
        None,
        &config,
    )
    .unwrap();

    assert_eq!(
        step.actions,
        vec![
            TerminalClientLoopAction::HandleMouse(MouseAction::FocusPane(CopyPosition {
                line: 2,
                column: 1,
            })),
            TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionUpdate(CopyPosition {
                line: 4,
                column: 11,
            },)),
            TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionFinish(CopyPosition {
                line: 4,
                column: 11,
            },)),
        ]
    );
}

/// Verifies malformed SGR mouse prefixes do not strand later pane input in the
/// immediate forwarding router.
/// Batched attached-terminal reads can contain an unterminated `ESC[<` fragment
/// followed by ordinary pane bytes like `q`, and the router must recover by
/// dropping only the malformed mouse prefix instead of stopping the whole read.
#[test]
fn client_loop_skips_malformed_sgr_mouse_prefix_before_later_pane_input() {
    let config = TerminalClientLoopConfig::default();

    assert_eq!(
        route_client_input_actions(b"\x1b[<0;12;5q", &config).unwrap(),
        vec![
            TerminalClientLoopAction::HandleMouse(MouseAction::Ignore),
            TerminalClientLoopAction::ForwardToPane(b"q".to_vec()),
        ]
    );
}

/// Verifies that holding a drag selection beyond a pane edge keeps producing
/// selection-update actions even when the host terminal has no new mouse packet.
/// Runtime uses this synthetic update to keep pane history autoscrolling until
/// the pointer returns inside the pane or the button is released.
#[test]
fn attached_terminal_client_step_continues_selection_autoscroll_without_input() {
    let config = TerminalClientLoopConfig {
        mouse_selection_autoscroll_position: Some(CopyPosition { line: 0, column: 3 }),
        ..TerminalClientLoopConfig::default()
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Output,
        fd: 1,
        interest: TerminalFdInterest::write(),
        readable: false,
        writable: true,
        hangup: false,
        error: false,
    }];

    let step = plan_attached_terminal_client_step(&readiness, None, None, None, &config).unwrap();

    assert_eq!(
        step.actions,
        vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::CopySelectionUpdate(CopyPosition { line: 0, column: 3 })
        )]
    );
}

/// Verifies attached terminal client step routes input and composes output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_step_routes_input_and_composes_output() {
    let config = TerminalClientLoopConfig::default();
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(12, 3).unwrap(),
        client_size: Size::new(12, 3).unwrap(),
        lines: vec![
            "one         ".to_string(),
            "two         ".to_string(),
            "three       ".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 1,
        cursor_column: 2,
        cursor_visible: true,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        focus_events: false,
        alternate_screen: false,
        host_mouse_reporting: true,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };
    let readiness = vec![
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        },
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        },
    ];
    let status = ClientStatusLine {
        kind: ClientStatusKind::Plain,
        text: "ready".to_string(),
    };

    let plan = plan_attached_terminal_client_step(
        &readiness,
        Some(b"\x01%"),
        Some(&view),
        Some(&status),
        &config,
    )
    .unwrap();

    assert_eq!(
        plan.actions,
        vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::SplitPaneVertical
        )]
    );
    assert_eq!(plan.output_lines.len(), 3);
    assert_eq!(plan.output_lines[2], "ready       ");
    assert_eq!(plan.output_line_style_spans.len(), 3);
    assert!(plan.output_line_style_spans[0].is_empty());
    assert!(plan.output_line_style_spans[1].is_empty());
    assert_eq!(plan.output_line_style_spans[2].len(), 1);
    assert_eq!(plan.output_line_style_spans[2][0].start, 0);
    assert_eq!(plan.output_line_style_spans[2][0].length, 12);
    assert!(!plan.input_hangup);
    assert!(plan.error_roles.is_empty());
}

/// Verifies that actor-owned prompt overlays receive raw key bytes before
/// normal mux key classification. This preserves readline semantics for keys
/// such as the configured prefix while the command prompt is active.
#[test]
fn attached_terminal_client_step_forwards_raw_input_when_primary_prompt_is_active() {
    let config = TerminalClientLoopConfig::default();
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(12, 3).unwrap(),
        client_size: Size::new(12, 3).unwrap(),
        lines: vec![
            "one         ".to_string(),
            "two         ".to_string(),
            "▐ :         ".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 2,
        cursor_column: 3,
        cursor_visible: true,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        focus_events: false,
        alternate_screen: false,
        host_mouse_reporting: true,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: true,
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let plan =
        plan_attached_terminal_client_step(&readiness, Some(b"\x01:"), Some(&view), None, &config)
            .unwrap();

    assert_eq!(
        plan.actions,
        vec![TerminalClientLoopAction::ForwardToPane(b"\x01:".to_vec())]
    );
}

/// Verifies attached terminal client step routes batched prefix command prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that batched prefix-command bytes still open the command prompt.
/// Detached control-socket attach can read `Ctrl+A : Enter` in one buffer, and
/// the prefix command trigger must not be reported as an unbound prefix just
/// because a following byte arrived before the prompt loop starts.
fn attached_terminal_client_step_routes_batched_prefix_command_prompt() {
    let config = TerminalClientLoopConfig::default();
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let plan =
        plan_attached_terminal_client_step(&readiness, Some(b"\x01:\r"), None, None, &config)
            .unwrap();

    assert_eq!(
        plan.actions,
        vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::EnterCommandPrompt
        )]
    );
}

/// Verifies attached terminal client step reports hangups and errors without output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_step_reports_hangups_and_errors_without_output() {
    let config = TerminalClientLoopConfig::default();
    let readiness = vec![
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        },
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: false,
            hangup: false,
            error: true,
        },
    ];

    let plan =
        plan_attached_terminal_client_step(&readiness, Some(b"ignored"), None, None, &config)
            .unwrap();

    assert!(plan.actions.is_empty());
    assert!(plan.output_lines.is_empty());
    assert!(plan.input_hangup);
    assert!(!plan.output_hangup);
    assert_eq!(plan.error_roles, vec![AttachedTerminalFdRole::Output]);
}

/// Verifies client loop draws zoomed pane across window body.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_draws_zoomed_pane_across_window_body() {
    let mut ids = crate::ids::IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(20, 4).unwrap());
    window
        .split_active(&mut ids, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    window.toggle_zoom_active();
    let mut screens = BTreeMap::new();
    let mut left = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    left.feed(b"left");
    let mut right = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    right.feed(b"right");
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(window.panes()[1].id.to_string(), right);

    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let rendered = draw_window_from_screens(&window, &screens, &config).unwrap();
    let joined = rendered.join("\n");

    assert_eq!(rendered.len(), 4);
    assert!(joined.contains("right"));
    assert!(!joined.contains("left"));
    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 20);
}

/// Verifies client loop translates plain arrows in application cursor mode.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_translates_plain_arrows_in_application_cursor_mode() {
    let mut config = TerminalClientLoopConfig::default();
    config.mouse_policy.pane_application_cursor_mode = true;

    assert_eq!(
        route_client_input(b"\x1b[A", &config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"\x1bOA".to_vec())
    );
}

/// Verifies client loop forwards application keypad sequences without rewriting digits.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_forwards_application_keypad_sequences_without_rewriting_digits() {
    let mut config = TerminalClientLoopConfig::default();
    config.mouse_policy.pane_application_keypad_mode = true;

    assert_eq!(
        route_client_input(b"\x1bOp", &config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"\x1bOp".to_vec())
    );
    assert_eq!(
        route_client_input(b"0", &config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"0".to_vec())
    );
}

/// Verifies client loop routes copy mode keys without forwarding to pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_routes_copy_mode_keys_without_forwarding_to_pane() {
    let mut config = TerminalClientLoopConfig::default();
    config.mouse_policy.copy_mode_active = true;

    assert_eq!(
        route_client_input(b"\x1b[B", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::MoveDown)
    );
    assert_eq!(
        route_client_input(
            &key_chord_input_bytes(KeyChord::parse("C-Up").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::MoveUpFast)
    );
    assert_eq!(
        route_client_input(
            &key_chord_input_bytes(KeyChord::parse("C-Down").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::MoveDownFast)
    );
    assert_eq!(
        route_client_input(b" ", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::BeginSelection)
    );
    assert_eq!(
        route_client_input(b"\x1b[H", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::LineStart)
    );
    assert_eq!(
        route_client_input(
            &key_chord_input_bytes(KeyChord::parse("C-Home").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::Top)
    );
    assert_eq!(
        route_client_input(b"\x1b[F", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::LineEnd)
    );
    assert_eq!(
        route_client_input(
            &key_chord_input_bytes(KeyChord::parse("C-End").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::Bottom)
    );
    assert_eq!(
        route_client_input(
            &key_chord_input_bytes(KeyChord::parse("C-Left").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::MoveWordLeft)
    );
    assert_eq!(
        route_client_input(
            &key_chord_input_bytes(KeyChord::parse("A-Right").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::MoveWordRight)
    );
    assert_eq!(
        route_client_input(b"\x1b", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::Cancel)
    );
    assert_eq!(
        route_client_input(b"\x03", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::Ignore)
    );
    assert_eq!(
        route_client_input(b"j", &config).unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::Ignore)
    );
}
