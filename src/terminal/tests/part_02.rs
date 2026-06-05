/// Verifies pane-local clear operations keep the live viewport blank across
/// subsequent pane splits and resizes.
///
/// `Ctrl+L` scrolls the visible rows into scrollback and clears the pane
/// without deleting copyable history. Later width-changing and row-only
/// resizes must preserve that empty viewport instead of repopulating it from
/// history.
#[test]
fn terminal_screen_resize_preserves_blank_viewport_after_clear_visible_into_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree");
    screen.clear_visible_into_history();

    screen.resize(Size::new(5, 3).unwrap());
    assert_eq!(screen.visible_lines(), vec!["", "", ""]);
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 0);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["one", "two", "three"]
    );

    screen.resize(Size::new(5, 4).unwrap());
    assert_eq!(screen.visible_lines(), vec!["", "", "", ""]);
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 0);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["one", "two", "three"]
    );
}

/// Verifies terminal erase operations clear hidden copy metadata for rows whose
/// visible cells were rewritten.
///
/// Agent-rendered rows can carry alternate raw copy text. Once a terminal
/// application erases a row, that stale raw text must not remain available to
/// copy-mode or scrollback export for the now-blank row.
#[test]
fn terminal_screen_erase_line_clears_row_copy_text() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();
    screen.line_copy_texts[0] = Some("hidden raw copy".to_string());

    screen.feed(b"\x1b[2K");

    assert_eq!(screen.line_copy_texts[0], None);
}

/// Verifies terminal full-screen clears detach the visible viewport from the
/// adjacent scrollback tail while preserving copyable history.
///
/// Shell `Ctrl+L` commonly emits cursor-home plus `CSI 2 J` instead of calling
/// Mezzanine's pane-local clear helper. After large wrapped output, a
/// subsequent width-changing split must reflow only the prompt/visible rows and
/// must not pull the retained random-output tail back up from history.
#[test]
fn terminal_screen_resize_after_shell_clear_does_not_pull_history_tail_into_view() {
    let mut screen = TerminalScreen::new(Size::new(12, 4).unwrap(), 256).unwrap();
    for index in 0..40 {
        screen.feed(format!("tail-{index:02}-abcdefghijklmnopqrstuvwxyz\r\n").as_bytes());
    }
    let history_before_clear = screen
        .history()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();

    screen.feed(b"\x1b[H\x1b[2J$ ");
    assert_eq!(screen.visible_lines(), vec!["$", "", "", ""]);

    screen.resize(Size::new(8, 4).unwrap());
    assert_eq!(screen.visible_lines(), vec!["$", "", "", ""]);
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 2);
    assert_eq!(
        screen
            .history()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>(),
        history_before_clear
    );
}
/// Verifies shell-cleared panes keep their detached viewport stationary when a
/// neighboring pane closes and restores extra height.
///
/// Closing an over/under split increases only the pane height. After a shell
/// `Ctrl+L`, that growth must leave the visible prompt rows where they already
/// were instead of repopulating the pane from retained scrollback.
#[test]
fn terminal_screen_row_only_expand_after_shell_clear_keeps_stationary_viewport() {
    let mut screen = TerminalScreen::new(Size::new(12, 4).unwrap(), 256).unwrap();
    for index in 0..40 {
        screen.feed(format!("tail-{index:02}-abcdefghijklmnopqrstuvwxyz\r\n").as_bytes());
    }
    let history_before_clear = screen
        .history()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    screen.feed(b"\x1b[H\x1b[2J$ ");
    screen.resize(Size::new(12, 2).unwrap());
    assert_eq!(screen.visible_lines(), vec!["$", ""]);
    screen.resize(Size::new(12, 5).unwrap());
    assert_eq!(screen.visible_lines(), vec!["$", "", "", "", ""]);
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 2);
    assert_eq!(
        screen
            .history()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>(),
        history_before_clear
    );
}

/// Verifies terminal screen handles erase line variants.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_erase_line_variants() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"abcdef\x1b[1;4H\x1b[1K");
    assert_eq!(screen.visible_lines()[0], "    ef");

    screen.feed(b"\rabcdef\x1b[1;4H\x1b[2Kxy");
    assert_eq!(screen.visible_lines()[0], "   xy");
}

/// Verifies terminal screen saves and restores cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_saves_and_restores_cursor() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"ab\x1b7cd\x1b8XY\n12\x1b[s34\x1b[uZZ");

    assert_eq!(screen.visible_lines()[0], "abXY");
    assert_eq!(screen.visible_lines()[1], "12ZZ");
}

/// Verifies terminal screen saves and restores dec private modes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_saves_and_restores_dec_private_modes() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?1;1000;1004;1006;2004h");
    assert!(screen.application_cursor_enabled());
    assert!(screen.focus_events_enabled());
    assert!(screen.application_sgr_mouse_enabled());
    assert!(screen.bracketed_paste_enabled());

    screen.feed(b"\x1b[?1;1000;1004;1006;2004s");
    screen.feed(b"\x1b[?1;1000;1004;1006;2004l");
    assert!(!screen.application_cursor_enabled());
    assert!(!screen.focus_events_enabled());
    assert!(!screen.application_sgr_mouse_enabled());
    assert!(!screen.bracketed_paste_enabled());

    screen.feed(b"\x1b[?1;1000;1004;1006;2004r");
    assert!(screen.application_cursor_enabled());
    assert!(screen.focus_events_enabled());
    assert!(screen.application_sgr_mouse_enabled());
    assert!(screen.bracketed_paste_enabled());
}

/// Verifies terminal screen handles insertion deletion and scroll regions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_insertion_deletion_and_scroll_regions() {
    let mut screen = TerminalScreen::new(Size::new(8, 4).unwrap(), 10).unwrap();

    screen.feed(b"abcd\x1b[1;3H\x1b[2@XY");
    assert_eq!(screen.visible_lines()[0], "abXYcd");

    screen.feed(b"\x1b[1;3H\x1b[2P");
    assert_eq!(screen.visible_lines()[0], "abcd");

    let mut screen = TerminalScreen::new(Size::new(8, 4).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour");
    screen.feed(b"\x1b[2;4r\x1b[2;1H\x1b[L");
    assert_eq!(screen.visible_lines(), vec!["one", "", "two", "three"]);

    screen.feed(b"\x1b[2;1H\x1b[M");
    assert_eq!(screen.visible_lines(), vec!["one", "two", "three", ""]);

    screen.feed(b"\x1b[2;4r\x1b[4;1H\n");
    assert_eq!(screen.visible_lines(), vec!["one", "three", "", ""]);
    assert!(screen.history().is_empty());
}

/// Verifies terminal screen tracks bracketed paste mode.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_bracketed_paste_mode() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?2004h");
    assert!(screen.bracketed_paste_enabled());

    screen.feed(b"\x1b[?2004l");
    assert!(!screen.bracketed_paste_enabled());
}

/// Verifies terminal screen tracks application sgr mouse mode.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_application_sgr_mouse_mode() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?1000h");
    assert!(screen.application_mouse_enabled());
    assert!(!screen.application_sgr_mouse_enabled());

    screen.feed(b"\x1b[?1000;1006h");
    assert!(screen.application_mouse_enabled());
    assert!(screen.application_sgr_mouse_enabled());

    screen.feed(b"\x1b[?1006l");
    assert!(screen.application_mouse_enabled());
    assert!(!screen.application_sgr_mouse_enabled());

    screen.feed(b"\x1b[?1000l");
    assert!(!screen.application_mouse_enabled());
}

/// Verifies terminal screen tracks application cursor keypad and focus modes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_application_cursor_keypad_and_focus_modes() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?1;1004h\x1b=");
    assert!(screen.application_cursor_enabled());
    assert!(screen.focus_events_enabled());
    assert!(screen.application_keypad_enabled());

    screen.feed(b"\x1b[?1;1004l\x1b>");
    assert!(!screen.application_cursor_enabled());
    assert!(!screen.focus_events_enabled());
    assert!(!screen.application_keypad_enabled());
}

/// Verifies that DEC private mode 25 controls the terminal cursor visibility
/// state used by attached-client rendering and snapshot restore.
#[test]
fn terminal_screen_tracks_dec_private_cursor_visibility() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    assert!(screen.cursor_visible());
    screen.feed(b"\x1b[?25lhidden");
    assert!(!screen.cursor_visible());
    assert!(!screen.mode_state().cursor_visible);

    screen.feed(b"\x1b[?25h");
    assert!(screen.cursor_visible());
    assert!(screen.mode_state().cursor_visible);
}

/// Verifies that snapshot resume can restore terminal title and mode flags
/// without replaying the original OSC or DEC private-mode byte stream.
#[test]
fn terminal_screen_restores_terminal_mode_state() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();
    let state = TerminalModeState {
        title: Some("snapshot-title".to_string()),
        cursor_visible: false,
        bracketed_paste_enabled: true,
        mouse_tracking_enabled: true,
        sgr_mouse_enabled: true,
        application_cursor_enabled: true,
        application_keypad_enabled: true,
        focus_events_enabled: true,
    };

    screen.restore_mode_state(&state);

    assert_eq!(screen.mode_state(), state);
    assert_eq!(screen.title(), Some("snapshot-title"));
    assert!(!screen.cursor_visible());
    assert!(screen.bracketed_paste_enabled());
    assert!(screen.application_sgr_mouse_enabled());
    assert!(screen.application_cursor_enabled());
    assert!(screen.application_keypad_enabled());
    assert!(screen.focus_events_enabled());
}

/// Verifies that snapshot resume can restore saved cursor and DEC private-mode
/// state so later restore escape sequences behave as if the PTY stream had run.
#[test]
fn terminal_screen_restores_terminal_saved_state() {
    let mut original = TerminalScreen::new(Size::new(10, 4).unwrap(), 10).unwrap();
    original.feed(b"ab\x1b[s\x1b[?1;1000;1006;2004h\x1b[?1;1000;1006;2004s\x1b[?1;1000;1006;2004l");
    let saved_state = original.saved_state();

    assert_eq!(
        saved_state.saved_cursor,
        Some(TerminalCursorState { row: 0, column: 2 })
    );
    assert!(
        saved_state
            .saved_dec_private_modes
            .iter()
            .any(|mode| mode.mode == 2004 && mode.enabled)
    );

    let mut restored = TerminalScreen::new(Size::new(10, 4).unwrap(), 10).unwrap();
    restored.restore_saved_state(&saved_state);
    restored.feed(b"zz\x1b[uXY\x1b[?1;1000;1006;2004r");

    assert_eq!(restored.visible_lines()[0], "zzXY");
    assert!(restored.application_cursor_enabled());
    assert!(restored.application_sgr_mouse_enabled());
    assert!(restored.bracketed_paste_enabled());
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

/// Verifies attached output frame sets client application keypad mode.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_output_frame_sets_client_application_keypad_mode() {
    let lines = vec!["pane".to_string()];
    assert!(
        encode_attached_terminal_output_frame_with_keypad_transition(&lines, Some(true),)
            .starts_with(b"\x1b=\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[2J\x1b[H")
    );
    assert!(
        encode_attached_terminal_output_frame_with_keypad_transition(&lines, Some(false),)
            .starts_with(b"\x1b>\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[2J\x1b[H")
    );
    assert!(
        encode_attached_terminal_output_frame_with_keypad_transition(&lines, None).starts_with(
            b"\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[2J\x1b[H"
        )
    );
}

/// Verifies attached terminal frames mirror pane bracketed-paste mode into the
/// host terminal. Clipboard paste delimiters are only available when the host
/// terminal has been explicitly placed in bracketed-paste mode.
#[test]
fn attached_output_frame_sets_host_bracketed_paste_mode() {
    let lines = vec!["pane".to_string()];
    let frame = encode_attached_terminal_output_frame_with_styles(
        &lines,
        &[],
        None,
        AttachedTerminalOutputModes {
            bracketed_paste: true,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(rendered.starts_with(
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004h\x1b[2J\x1b[H"
    ));
    assert!(
        String::from_utf8(
            super::client_loop::attached_terminal_restore_presentation_frame().to_vec()
        )
        .unwrap()
        .starts_with("\x1b[?2004l"),
        "restore must always leave host bracketed paste disabled"
    );
}

/// Verifies that attached terminal output encodes rendered SGR spans as ANSI
/// SGR sequences and resets styling before returning to plain text.
#[test]
fn attached_output_frame_encodes_sgr_style_spans() {
    let lines = vec!["ABCD".to_string()];
    let spans = vec![vec![
        TerminalStyleSpan {
            start: 0,
            length: 2,
            rendition: GraphicRendition {
                bold: true,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: false,
                inverse: false,
                foreground: Some(TerminalColor::Indexed(120)),
                background: None,
            },
        },
        TerminalStyleSpan {
            start: 2,
            length: 1,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: true,
                inverse: true,
                foreground: Some(TerminalColor::Rgb(1, 2, 3)),
                background: Some(TerminalColor::Indexed(4)),
            },
        },
    ]];

    let frame = encode_attached_terminal_output_frame_with_styles(
        &lines,
        &spans,
        None,
        AttachedTerminalOutputModes::default(),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert_eq!(
        rendered,
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[2J\x1b[H\x1b[0;1;38;5;120mAB\x1b[0;4;7;38;2;1;2;3;44mC\x1b[0mD\x1b[?25l\x1b[0m"
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
            &super::key_chord_input_bytes(KeyChord::parse("C-Up").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::MoveUpFast)
    );
    assert_eq!(
        route_client_input(
            &super::key_chord_input_bytes(KeyChord::parse("C-Down").unwrap()).unwrap(),
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
            &super::key_chord_input_bytes(KeyChord::parse("C-Home").unwrap()).unwrap(),
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
            &super::key_chord_input_bytes(KeyChord::parse("C-End").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::Bottom)
    );
    assert_eq!(
        route_client_input(
            &super::key_chord_input_bytes(KeyChord::parse("C-Left").unwrap()).unwrap(),
            &config
        )
        .unwrap(),
        TerminalClientLoopAction::HandleCopyMode(CopyModeKeyAction::MoveWordLeft)
    );
    assert_eq!(
        route_client_input(
            &super::key_chord_input_bytes(KeyChord::parse("A-Right").unwrap()).unwrap(),
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

/// Verifies terminal screen tracks osc title with bel and st terminators.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_osc_title_with_bel_and_st_terminators() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before\x1b]0;window title\x07after");

    assert_eq!(screen.title(), Some("window title"));
    assert_eq!(screen.visible_lines()[0], "beforeafter");

    screen.feed(b"\x1b]2;renamed\x1b\\");

    assert_eq!(screen.title(), Some("renamed"));
    assert_eq!(screen.visible_lines()[0], "beforeafter");
}

/// Verifies terminal screen tracks mezzanine shell transaction osc events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_mezzanine_shell_transaction_osc_events() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]133;A\x1b\\");
    screen.feed(b"\x1b]133;B\x1b\\");
    screen.feed(b"\x1b]133;C\x1b\\");
    screen
        .feed(b"\x1b]133;C;mez_marker=abc123;mez_turn=turn-1;mez_agent=agent-1;mez_pane=%1\x1b\\");
    screen
        .feed(b"\x1b]133;D;7;mez_marker=abc123;mez_turn=turn-1;mez_agent=agent-1;mez_pane=%1\x07");

    assert_eq!(
        screen.drain_osc_events(),
        vec![
            TerminalOscEvent::ShellPromptStart,
            TerminalOscEvent::ShellPromptEnd,
            TerminalOscEvent::ShellCommandOutputStart,
            TerminalOscEvent::ShellTransactionStart {
                marker: "abc123".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
            },
            TerminalOscEvent::ShellTransactionEnd {
                marker: "abc123".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "%1".to_string(),
                exit_code: 7,
            },
        ]
    );
    assert_eq!(screen.visible_lines()[0], "");
}

/// Verifies terminal screen handles fragmented and ignored osc strings.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_fragmented_and_ignored_osc_strings() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]52;c;ignored");
    screen.feed(b"\x07text");

    assert_eq!(screen.title(), None);
    assert_eq!(screen.drain_osc_events(), Vec::<TerminalOscEvent>::new());
    assert_eq!(screen.visible_lines()[0], "text");

    screen.feed(b"\x1b]2;split");
    screen.feed(b" title\x1b\\tail");

    assert_eq!(screen.title(), Some("split title"));
    assert_eq!(screen.visible_lines()[0], "texttail");
}

/// Verifies terminal screen parses osc52 clipboard payloads.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_parses_osc52_clipboard_payloads() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b]52;c;aGVsbG8=\x07after");

    assert_eq!(
        screen.drain_osc_events(),
        vec![TerminalOscEvent::ClipboardSet {
            selection: "c".to_string(),
            content: "hello".to_string(),
        }]
    );
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies oversized OSC payloads are dropped instead of dispatched in
/// truncated form.
///
/// OSC 52 clipboard content is base64 encoded, so silently dispatching the
/// bounded prefix can produce a valid but corrupted clipboard event. The parser
/// must consume through the terminator, skip dispatch for that payload, and
/// resume ordinary text parsing afterward.
#[test]
fn terminal_screen_drops_truncated_osc52_clipboard_payloads() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    let oversized_encoded = "A".repeat(4096);
    let sequence = format!("\x1b]52;;{oversized_encoded}\x07after");

    screen.feed(sequence.as_bytes());

    assert_eq!(screen.drain_osc_events(), Vec::<TerminalOscEvent>::new());
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies an OSC payload exactly at the parser byte limit still dispatches.
///
/// The truncation guard must reject only payloads that exceed the bounded OSC
/// buffer. This protects title and clipboard sequences that fit exactly within
/// the parser limit from being treated as overflow cases.
#[test]
fn terminal_screen_dispatches_osc_payload_at_exact_limit() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    let title = "t".repeat(4094);
    let sequence = format!("\x1b]2;{title}\x07after");

    screen.feed(sequence.as_bytes());

    assert_eq!(screen.title(), Some(title.as_str()));
    assert_eq!(
        screen.drain_osc_events(),
        vec![TerminalOscEvent::TitleChanged { title }]
    );
    assert_eq!(screen.visible_lines()[0], "after");
}

/// Verifies terminal screen replaces invalid utf8 without breaking layout.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_replaces_invalid_utf8_without_breaking_layout() {
    let mut screen = TerminalScreen::new(Size::new(12, 2).unwrap(), 10).unwrap();

    screen.feed(b"ok \xff done");

    assert_eq!(screen.visible_lines()[0], "ok \u{fffd} done");
}

/// Verifies terminal screen preserves combining marks in live rows.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_documents_combining_mark_boundary_behavior() {
    let mut screen = TerminalScreen::new(Size::new(12, 2).unwrap(), 10).unwrap();

    screen.feed("e\u{301}x".as_bytes());

    assert_eq!(screen.visible_lines()[0], "e\u{301}x");
    assert_eq!(screen.cursor_state().column, 2);
}

/// Verifies terminal screen nested multiplexer passthrough payload is bounded and ignored.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_nested_multiplexer_passthrough_payload_is_bounded_and_ignored() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before\x1bPtmux;\x1b\x1b[31mnested\x1b\\after");

    assert_eq!(screen.visible_lines()[0], "beforeafter");
    assert_eq!(screen.drain_osc_events(), Vec::<TerminalOscEvent>::new());
}

/// Verifies terminal screen ignores dcs string controls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_ignores_dcs_string_controls() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"before\x1bP1$r q\x1b\\after");

    assert_eq!(screen.visible_lines()[0], "beforeafter");

    screen.feed(b"\x1bPignored");
    screen.feed(b" payload\x1b\\tail");

    assert_eq!(screen.visible_lines()[0], "beforeaftertail");

    screen.feed(b"\x1bPbell\x07still ignored\x1b\\ok");

    assert_eq!(screen.visible_lines()[0], "beforeaftertailok");
    assert_eq!(screen.bell_events(), 0);
}

/// Verifies terminal screen ignores unsupported string controls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_ignores_unsupported_string_controls() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"a\x1bXignored\x1b\\b\x1b^private\x1b\\c\x1b_apc\x1b\\d");

    assert_eq!(screen.visible_lines()[0], "abcd");
}

/// Verifies that SGR parsing stores rendition state on printed cells and that
/// the public styled-line API exposes only non-default visible style runs.
#[test]
fn terminal_screen_stores_sgr_rendition_per_printed_cell() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[1;4;31;48;5;200mX");
    assert_eq!(screen.visible_lines()[0], "X");
    let styled = GraphicRendition {
        bold: true,
        dim: false,
        italic: false,
        strikethrough: false,
        double_underline: false,
        hidden: false,
        underline: true,
        inverse: false,
        foreground: Some(TerminalColor::Indexed(1)),
        background: Some(TerminalColor::Indexed(200)),
    };
    assert_eq!(screen.graphic_rendition, styled);
    assert_eq!(screen.cell_rendition(0, 0), Some(styled));

    screen.feed(b"\x1b[38;2;1;2;3;48;5;42mY");
    assert_eq!(screen.visible_lines()[0], "XY");
    assert_eq!(
        screen.cell_rendition(0, 1),
        Some(GraphicRendition {
            bold: true,
            dim: false,
            italic: false,
            strikethrough: false,
            double_underline: false,
            hidden: false,
            underline: true,
            inverse: false,
            foreground: Some(TerminalColor::Rgb(1, 2, 3)),
            background: Some(TerminalColor::Indexed(42)),
        })
    );

    screen.feed(b"\x1b[22;24;39;49mZ");
    assert_eq!(screen.visible_lines()[0], "XYZ");
    assert_eq!(screen.graphic_rendition, GraphicRendition::default());
    assert_eq!(
        screen.cell_rendition(0, 2),
        Some(GraphicRendition::default())
    );
    assert_eq!(screen.visible_styled_lines()[0].text, "XYZ");
    assert_eq!(
        screen.visible_styled_lines()[0].style_spans,
        vec![
            TerminalStyleSpan {
                start: 0,
                length: 1,
                rendition: styled,
            },
            TerminalStyleSpan {
                start: 1,
                length: 1,
                rendition: GraphicRendition {
                    bold: true,
                    dim: false,
                    italic: false,
                    strikethrough: false,
                    double_underline: false,
                    hidden: false,
                    underline: true,
                    inverse: false,
                    foreground: Some(TerminalColor::Rgb(1, 2, 3)),
                    background: Some(TerminalColor::Indexed(42)),
                },
            },
        ]
    );
}

/// Verifies that styled trailing blank cells remain part of the styled visible
/// line. Full-screen applications often clear or paint a whole row with a
/// background color and spaces, so trimming styled blanks would make
/// row-differential rendering drop the application's background fill.
#[test]
fn terminal_screen_preserves_styled_trailing_blank_cells() {
    let mut screen = TerminalScreen::new(Size::new(5, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[48;5;42m\x1b[2K");
    let styled = screen.visible_styled_lines();

    assert_eq!(styled[0].text, "     ");
    assert_eq!(
        styled[0].style_spans,
        vec![TerminalStyleSpan {
            start: 0,
            length: 5,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: false,
                inverse: false,
                foreground: None,
                background: Some(TerminalColor::Indexed(42)),
            },
        }]
    );
}

/// Verifies copy mode starts at live view and pages through normal history.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn copy_mode_starts_at_live_view_and_pages_through_normal_history() {
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour");

    let mut copy = CopyMode::from_screen(&screen, 2).unwrap();

    assert_eq!(
        copy.visible_lines(),
        &["three".to_string(), "four".to_string()]
    );

    copy.page_up();

    assert_eq!(copy.scroll_top(), 0);
    assert_eq!(
        copy.visible_lines(),
        &["one".to_string(), "two".to_string()]
    );
}

/// Verifies PageUp and PageDown jump directly to the buffer edge when the
/// remaining scroll distance is shorter than one full viewport page. This keeps
/// copy-mode navigation from requiring a small extra paging step near the top
/// or bottom of history.
#[test]
fn copy_mode_page_keys_jump_to_edges_when_less_than_one_page_remains() {
    let mut screen = TerminalScreen::new(Size::new(8, 3).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour\nfive\nsix\nseven");

    let mut copy = CopyMode::from_screen(&screen, 3).unwrap();

    assert_eq!(copy.scroll_top(), 4);

    copy.page_up();
    assert_eq!(copy.scroll_top(), 1);

    copy.page_up();
    assert_eq!(copy.scroll_top(), 0);
    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 0 });

    copy.page_down();
    assert_eq!(copy.scroll_top(), 3);

    copy.page_down();
    assert_eq!(copy.scroll_top(), 4);
    assert_eq!(copy.cursor(), CopyPosition { line: 6, column: 5 });
}

/// Verifies copy mode seeds the rendered keyboard cursor from the pane's live
/// terminal cursor. Entering copy mode should preserve the user's current
/// visual cursor location so arrow keys move the rendered copy cursor from the
/// same screen cell instead of jumping to the first visible history line.
#[test]
fn copy_mode_cursor_starts_at_live_terminal_cursor() {
    let mut screen = TerminalScreen::new(Size::new(20, 3).unwrap(), 10).unwrap();
    screen.feed(b"alpha\nbeta\ngamma");
    screen.feed(b"\x1b[2;3H");

    let mut copy = CopyMode::from_screen(&screen, 3).unwrap();

    assert_eq!(copy.cursor(), CopyPosition { line: 1, column: 2 });

    copy.move_cursor_by(0, 1);

    assert_eq!(copy.cursor(), CopyPosition { line: 1, column: 3 });
}

/// Verifies single-cell copy-mode cursor movement crosses line boundaries.
/// Pressing Right at the end of a line should move to the next line's first
/// cell, and pressing Left at the beginning of a line should return to the
/// previous line's end instead of clamping in place.
#[test]
fn copy_mode_horizontal_cursor_movement_overflows_between_lines() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();
    screen.feed(b"abc\ndef");
    screen.feed(b"\x1b[1;4H");
    let mut copy = CopyMode::from_screen(&screen, 2).unwrap();

    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 3 });

    copy.move_cursor_by(0, 1);
    assert_eq!(copy.cursor(), CopyPosition { line: 1, column: 0 });

    copy.move_cursor_by(0, -1);
    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 3 });

    copy.move_cursor_by(0, -1);
    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 2 });

    copy.move_cursor_to_line_end();
    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 3 });

    copy.move_cursor_by(0, 2);
    assert_eq!(copy.cursor(), CopyPosition { line: 1, column: 1 });
}

/// Verifies copy mode treats modified cursor movement like a readline-style
/// editing cursor. Home and End stay on the current line, Ctrl-Home and
/// Ctrl-End jump to buffer edges, and modified horizontal movement skips a
/// word-like segment instead of moving one cell at a time.
#[test]
fn copy_mode_readline_style_modified_cursor_movement() {
    let mut screen = TerminalScreen::new(Size::new(32, 3).unwrap(), 10).unwrap();
    screen.feed(b"alpha beta  gamma\nomega");
    screen.feed(b"\x1b[1;13H");
    let mut copy = CopyMode::from_screen(&screen, 3).unwrap();

    assert_eq!(
        copy.cursor(),
        CopyPosition {
            line: 0,
            column: 12
        }
    );

    copy.move_cursor_word_left();
    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 6 });

    copy.move_cursor_word_right();
    assert_eq!(
        copy.cursor(),
        CopyPosition {
            line: 0,
            column: 10
        }
    );

    copy.move_cursor_word_right();
    assert_eq!(
        copy.cursor(),
        CopyPosition {
            line: 0,
            column: 17
        }
    );

    copy.move_cursor_to_line_start();
    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 0 });

    copy.move_cursor_to_line_end();
    assert_eq!(
        copy.cursor(),
        CopyPosition {
            line: 0,
            column: 17
        }
    );

    copy.scroll_to_bottom();
    assert_eq!(copy.cursor(), CopyPosition { line: 2, column: 0 });

    copy.scroll_to_top();
    assert_eq!(copy.cursor(), CopyPosition { line: 0, column: 0 });
}

/// Verifies that copy mode keeps the SGR spans recorded in normal-screen
/// history. Pane-local scrollback rendering uses these styled lines directly so
/// scrolling a pane does not flatten colored or attributed terminal output.
#[test]
fn copy_mode_preserves_styled_history_lines() {
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[31mred\x1b[0m\nplain\nlast");

    let mut copy = CopyMode::from_screen(&screen, 2).unwrap();
    copy.page_up();

    let styled = copy.visible_styled_lines();
    assert_eq!(styled[0].text, "red");
    assert_eq!(styled[0].style_spans.len(), 1);
    assert_eq!(
        styled[0].style_spans[0].rendition.foreground,
        Some(TerminalColor::Indexed(1))
    );
}

/// Verifies copy mode excludes active alternate screen content.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn copy_mode_excludes_active_alternate_screen_content() {
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    screen.feed(b"normal\n\x1b[?1049hsecret");

    let copy = CopyMode::from_screen(&screen, 4).unwrap();

    assert!(copy.alternate_screen_was_active());
    assert!(
        !copy
            .visible_lines()
            .iter()
            .any(|line| line.contains("secret"))
    );
}

/// Verifies copy mode search selects and copies text.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn copy_mode_search_selects_and_copies_text() {
    let mut screen = TerminalScreen::new(Size::new(20, 3).unwrap(), 10).unwrap();
    screen.feed(b"alpha\nbeta target\ngamma");
    let mut copy = CopyMode::from_screen(&screen, 3).unwrap();

    let position = copy
        .search("target", SearchDirection::Forward)
        .unwrap()
        .unwrap();

    assert_eq!(position.line, 1);
    assert_eq!(copy.copy_selection().unwrap(), "target");
}

/// Verifies copy mode copies multiline selection.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn copy_mode_copies_multiline_selection() {
    let mut screen = TerminalScreen::new(Size::new(20, 3).unwrap(), 10).unwrap();
    screen.feed(b"alpha\nbeta\ngamma");
    let mut copy = CopyMode::from_screen(&screen, 3).unwrap();

    copy.select_range(
        CopyPosition { line: 0, column: 2 },
        CopyPosition { line: 2, column: 3 },
    )
    .unwrap();

    assert_eq!(copy.copy_selection().unwrap(), "pha\nbeta\ngam");
}

/// Verifies copy mode treats Mezzanine's agent transcript gutter and assistant
/// continuation padding as display-only text. Users often copy assistant
/// markdown out of agent mode, so the paste result should preserve the
/// assistant's content indentation while omitting the visible `agent>` label,
/// continuation alignment, and `▐` indicator characters.
#[test]
fn copy_mode_formats_agent_assistant_output_for_clipboard() {
    let lines = [
        "▐ mez> ## Heading",
        "▐        - item",
        "▐            code",
    ];
    let mut screen = TerminalScreen::new(Size::new(40, 3).unwrap(), 10).unwrap();
    screen.feed(lines.join("\n").as_bytes());
    let mut copy = CopyMode::from_screen(&screen, 3).unwrap();

    copy.select_range(
        CopyPosition { line: 0, column: 0 },
        CopyPosition {
            line: 2,
            column: lines[2].chars().count(),
        },
    )
    .unwrap();

    assert_eq!(
        copy.copy_selection().unwrap(),
        "## Heading\n- item\n    code"
    );
}

/// Verifies copy mode still removes the agent gutter when a user selects only
/// continuation rows from an assistant response. This protects the common
/// mouse-selection case where the first selected row starts after the visible
/// `agent>` label but still contains pane-only alignment padding.
#[test]
fn copy_mode_dedents_orphan_agent_continuation_rows() {
    let lines = ["▐        - item", "▐            code"];
    let mut screen = TerminalScreen::new(Size::new(40, 2).unwrap(), 10).unwrap();
    screen.feed(lines.join("\n").as_bytes());
    let mut copy = CopyMode::from_screen(&screen, 2).unwrap();

    copy.select_range(
        CopyPosition { line: 0, column: 0 },
        CopyPosition {
            line: 1,
            column: lines[1].chars().count(),
        },
    )
    .unwrap();

    assert_eq!(copy.copy_selection().unwrap(), "- item\n    code");
}

/// Verifies copy mode removes only Mezzanine's agent indicator prefix from
/// non-assistant agent status lines. Status, error, and command preview lines
/// keep their text because those labels carry user-visible meaning, but the
/// pane-local gutter should not pollute copied text.
#[test]
fn copy_mode_omits_agent_indicator_prefix_from_status_lines() {
    let line = "▐ agent debug: checking context";
    let mut screen = TerminalScreen::new(Size::new(40, 1).unwrap(), 10).unwrap();
    screen.feed(line.as_bytes());
    let mut copy = CopyMode::from_screen(&screen, 1).unwrap();

    copy.select_range(
        CopyPosition { line: 0, column: 0 },
        CopyPosition {
            line: 0,
            column: line.chars().count(),
        },
    )
    .unwrap();

    assert_eq!(
        copy.copy_selection().unwrap(),
        "agent debug: checking context"
    );
}

/// Verifies copy mode can write selection to bounded paste buffer.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn copy_mode_can_write_selection_to_bounded_paste_buffer() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    screen.feed(b"alpha\nbeta");
    let mut copy = CopyMode::from_screen(&screen, 2).unwrap();
    copy.select_range(
        CopyPosition { line: 0, column: 1 },
        CopyPosition { line: 1, column: 2 },
    )
    .unwrap();
    let mut buffers = PasteBuffers::new(64).unwrap();

    copy.copy_selection_to_buffer(&mut buffers, "main").unwrap();

    assert_eq!(buffers.get("main"), Some("lpha\nbe"));
    let listed = buffers.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "main");
    assert_eq!(listed[0].bytes, 7);
    assert_eq!(listed[0].origin, None);
    assert_eq!(listed[0].preview, "lpha be");
}

/// Verifies copy-mode word selection uses readline delimiter rules.
///
/// Double-click mouse copy and modified copy-mode cursor movement share this
/// helper, so punctuation runs such as command flags must select separately
/// from adjacent identifier text rather than as one whitespace-delimited word.
#[test]
fn copy_mode_selects_readline_word_at_position() {
    let mut screen = TerminalScreen::new(Size::new(30, 1).unwrap(), 10).unwrap();
    screen.feed(b"run --flag=value");
    let mut copy = CopyMode::from_screen(&screen, 1).unwrap();

    copy.select_word_at(CopyPosition { line: 0, column: 5 })
        .unwrap();
    assert_eq!(copy.copy_selection().unwrap(), "--");

    copy.select_word_at(CopyPosition { line: 0, column: 7 })
        .unwrap();
    assert_eq!(copy.copy_selection().unwrap(), "flag");
}

/// Verifies paste buffers reject invalid names and oversized content.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn paste_buffers_reject_invalid_names_and_oversized_content() {
    let mut buffers = PasteBuffers::new(4).unwrap();

    assert_eq!(
        buffers.set("bad/name", "x").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert_eq!(
        buffers.set("main", "12345").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    buffers.set("main", "1234").unwrap();
    assert!(buffers.delete("main"));
}

/// Verifies paste buffer creation preserves existing content by default.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn paste_buffers_create_without_overwriting_existing_content() {
    let mut buffers = PasteBuffers::new(16).unwrap();

    assert!(
        buffers
            .create_with_origin("main", "seed", Some("test:create".to_string()))
            .unwrap()
    );
    assert!(!buffers.create_with_origin("main", "new", None).unwrap());

    assert_eq!(buffers.get("main"), Some("seed"));
    let listed = buffers.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].origin.as_deref(), Some("test:create"));
}

/// Verifies render window composes vertical split side by side.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_window_composes_vertical_split_side_by_side() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(rendered.len(), 3);
    assert_eq!(rendered[0], "pane\u{2502}pane1");
}

/// Verifies wide glyphs in pane content do not shift divider placement.
///
/// Pane composition is cell based. A double-width glyph immediately before a
/// divider must occupy its own cells without causing the final rendered string
/// to carry an extra filler cell that would push the border right.
#[test]
fn render_window_keeps_divider_fixed_after_wide_glyph() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["ab✅".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "ab✅\u{2502}right");
}

/// Verifies a wide glyph cannot overlap a divider and shift the pane to the
/// right of it.
///
/// If the continuation half of a wide glyph is overwritten by a divider, the
/// leading glyph cell must be cleared too. Otherwise the collected output
/// string still advances the terminal by two cells and pushes the neighboring
/// pane one column right on that row.
#[test]
fn render_window_clips_wide_glyph_that_overlaps_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["abc✅".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "abc \u{2502}right");
}

/// Verifies emoji-presentation warning signs keep their two-cell width before a
/// pane divider.
///
/// This protects the render path for grapheme clusters such as `⚠️`, where the
/// leading scalar alone is one cell but the rendered grapheme is two cells.
/// Dropping the variation selector during pane composition makes the divider
/// appear one column too far left on affected rows.
#[test]
fn render_window_keeps_divider_fixed_after_warning_sign_grapheme() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["ab⚠️".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "ab⚠️\u{2502}right");
}

/// Verifies a warning-sign grapheme clipped by a pane divider clears the full
/// wide-cell footprint.
///
/// When the divider overwrites the continuation half of `⚠️`, the leading
/// scalar must be cleared too. Otherwise only rows containing that grapheme
/// report a mismatched terminal width and the adjacent pane appears shifted.
#[test]
fn render_window_clips_warning_sign_grapheme_that_overlaps_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let inputs = vec![
        PaneRenderInput {
            pane_id: pane_ids[0].clone(),
            lines: vec!["abc⚠️".to_string()],
        },
        PaneRenderInput {
            pane_id: pane_ids[1].clone(),
            lines: vec!["right".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 10);
    assert_eq!(rendered[0], "abc \u{2502}right");
}

/// Verifies render window composes horizontal split stacked.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_window_composes_horizontal_split_stacked() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(12, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();

    let rendered = render_window(&window, &inputs, true).unwrap();

    assert_eq!(rendered.len(), 4);
    assert!(
        rendered[0].contains("0 shell") || rendered[0].starts_with("0 shell"),
        "unexpected pane frame: {}",
        rendered[0]
    );
    assert!(
        rendered[1].contains("1 shell"),
        "unexpected pane frame: {}",
        rendered[1]
    );
    assert_eq!(rendered[2], "pane1       ");
    assert!(rendered[3].trim().is_empty());
}

/// Verifies that horizontal split dividers remain visible when pane frame rows
/// are enabled and that pane body content is clipped to the rows left after the
/// frame and divider reservations.
#[test]
fn render_window_reserves_horizontal_divider_above_next_pane_header() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(12, 6).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = vec![
        PaneRenderInput {
            pane_id: window.panes()[0].id.to_string(),
            lines: vec![
                "old-top".to_string(),
                "visible-top".to_string(),
                "overflow-top".to_string(),
            ],
        },
        PaneRenderInput {
            pane_id: window.panes()[1].id.to_string(),
            lines: vec!["bottom".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, true).unwrap();

    assert_eq!(rendered.len(), 6);
    assert!(
        rendered[0].contains("0 shell") || rendered[0].starts_with("0 shell"),
        "unexpected pane frame: {}",
        rendered[0]
    );
    assert_eq!(rendered[1], "overflow-top");
    assert!(
        rendered[2].contains("1 shell") || rendered[2].starts_with("1 shell"),
        "unexpected pane frame: {}",
        rendered[2]
    );
    assert_eq!(rendered[2], " 1 shell ───");
    assert_eq!(rendered[3], "bottom      ");
}

/// Verifies that rendering uses the window's stored pane rectangles instead of
/// reducing layout to a side-by-side-or-stacked choice. The right pane is split
/// horizontally, so the left pane must remain visible across the full height
/// while the two right panes occupy only their stored upper and lower halves and
/// the adjacent divider junction is rendered as a connected tee.
#[test]
fn render_window_composes_irregular_layout_from_stored_geometry() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = vec![
        PaneRenderInput {
            pane_id: window.panes()[0].id.to_string(),
            lines: vec![
                "L0".to_string(),
                "L1".to_string(),
                "L2".to_string(),
                "L3".to_string(),
            ],
        },
        PaneRenderInput {
            pane_id: window.panes()[1].id.to_string(),
            lines: vec!["T0".to_string(), "T1".to_string()],
        },
        PaneRenderInput {
            pane_id: window.panes()[2].id.to_string(),
            lines: vec!["B0".to_string(), "B1".to_string()],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(
        rendered,
        vec![
            "L0  \u{2502}T1   ".to_string(),
            "L1  \u{251c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}".to_string(),
            "L2  \u{2502}B0   ".to_string(),
            "L3  \u{2502}B1   ".to_string(),
        ],
    );
}

/// Verifies that a horizontal split ending at the vertical divider from a
/// neighboring side-by-side pane uses a connected box-drawing tee rather than an
/// ASCII fallback. This is the overlapping junction shape that previously
/// produced `+` when the left pane was split horizontally.
#[test]
fn render_window_connects_overlapped_mixed_split_divider_junction() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window.select_pane("0").unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let inputs = vec![
        PaneRenderInput {
            pane_id: window.panes()[0].id.to_string(),
            lines: vec!["TL0".to_string()],
        },
        PaneRenderInput {
            pane_id: window.panes()[1].id.to_string(),
            lines: vec!["BL0".to_string(), "BL1".to_string()],
        },
        PaneRenderInput {
            pane_id: window.panes()[2].id.to_string(),
            lines: vec![
                "R0".to_string(),
                "R1".to_string(),
                "R2".to_string(),
                "R3".to_string(),
            ],
        },
    ];

    let rendered = render_window(&window, &inputs, false).unwrap();

    assert_eq!(
        rendered,
        vec![
            "TL0 \u{2502}R0   ".to_string(),
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2524}R1   ".to_string(),
            "BL0 \u{2502}R2   ".to_string(),
            "BL1 \u{2502}R3   ".to_string(),
        ],
    );
}

/// Builds a test window from explicit rendered pane rectangles.
///
/// # Parameters
/// - `size`: The target terminal size for the rendered window.
/// - `geometries`: The complete replacement pane geometry set.
fn window_from_test_geometries(size: Size, geometries: Vec<PaneGeometry>) -> Window {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", size);
    while window.panes().len() < geometries.len() {
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
    }
    window.replace_pane_geometries(geometries).unwrap();
    window
}

/// Returns blank render inputs for every pane in a test window.
///
/// # Parameters
/// - `window`: The window whose pane IDs should be covered.
fn blank_inputs_for_window(window: &Window) -> Vec<PaneRenderInput> {
    window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![String::new()],
        })
        .collect()
}

/// Verifies every mux-managed divider connection mask maps to the expected
/// thin Unicode box-drawing glyph.
#[test]
fn pane_divider_connection_masks_use_correct_box_drawing_glyphs() {
    let cases = [
        ((true, true, false, false), '\u{2502}'),
        ((false, false, true, true), '\u{2500}'),
        ((false, true, false, true), '\u{250c}'),
        ((false, true, true, false), '\u{2510}'),
        ((true, false, false, true), '\u{2514}'),
        ((true, false, true, false), '\u{2518}'),
        ((false, true, true, true), '\u{252c}'),
        ((true, false, true, true), '\u{2534}'),
        ((true, true, false, true), '\u{251c}'),
        ((true, true, true, false), '\u{2524}'),
        ((true, true, true, true), '\u{253c}'),
    ];

    for ((up, down, left, right), expected) in cases {
        assert_eq!(
            pane_divider_glyph_for_test(up, down, left, right),
            expected,
            "unexpected glyph for up={up} down={down} left={left} right={right}"
        );
    }
}

/// Verifies rendered irregular pane layouts compose every mixed split junction
/// as connected Unicode box drawing rather than ASCII fallback characters.
#[test]
fn render_window_connects_all_mixed_split_junction_shapes() {
    let size = Size::new(24, 12).unwrap();
    let cases = [
        (
            '\u{253c}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 0,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 3,
                    column: 12,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{252c}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 24,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 0,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 12,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{2534}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 0,
                    row: 6,
                    columns: 24,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{251c}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 12,
                },
                PaneGeometry {
                    index: 1,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 12,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
            ],
        ),
        (
            '\u{2524}',
            vec![
                PaneGeometry {
                    index: 0,
                    column: 0,
                    row: 0,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 1,
                    column: 0,
                    row: 6,
                    columns: 12,
                    rows: 6,
                },
                PaneGeometry {
                    index: 2,
                    column: 12,
                    row: 0,
                    columns: 12,
                    rows: 12,
                },
            ],
        ),
    ];

    for (expected, geometries) in cases {
        let window = window_from_test_geometries(size, geometries);
        let inputs = blank_inputs_for_window(&window);
        let rendered = render_window(&window, &inputs, false).unwrap();

        assert_eq!(
            rendered[5].chars().nth(11),
            Some(expected),
            "unexpected junction in layout:\n{}",
            rendered.join("\n")
        );
    }
}

/// Verifies render pane frame uses named template fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_pane_frame_uses_named_template_fields() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(18, 2).unwrap());
    window.panes_mut()[0].title = "shell\u{1b}[31m".to_string();
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            "#{pane.index}|#{pane.title}|#{pane.id}|#{missing.field}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), format!("0|shell[31m|{pane_id}|"));
    assert_eq!(rendered[1], "body              ");
}

/// Verifies render pane frame template fits narrow panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_pane_frame_template_fits_narrow_panes() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(8, 2).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            "#{pane.index}:#{pane.title}:#{pane.size}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0], "0:shell:");
}

/// Verifies that window frame templates render named fields, sanitize control
/// characters, and reserve one row from the rendered window body.
#[test]
fn render_window_frame_uses_named_template_fields() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 7, "main\u{1b}[31m", Size::new(18, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(
            true,
            "#{window.index}|#{window.name}|#{window.pane_count}|#{layout.name}",
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered.len(), 3);
    assert_eq!(rendered[0], "7|main[31m|2|tiled");
    assert_eq!(rendered[1], "pane0   \u{2502}pane1    ");
}

/// Verifies that runtime-supplied frame context values are available through
/// the required named window and pane frame fields without leaking control
/// characters into the rendered terminal frame text.
#[test]
fn render_frame_templates_use_runtime_context_fields() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(120, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext {
        session_id: Some("$1".to_string()),
        policy_mode: Some("full-access".to_string()),
        pending_observer_count: 1,
        ..TerminalFrameContext::default()
    };
    frame_context
        .window_agent_active_counts
        .insert(window.id.to_string(), 2);
    frame_context
        .window_unread_message_counts
        .insert(window.id.to_string(), 3);
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            primary_pid: Some(4242),
            process_name: Some("bash\u{1b}[31m".to_string()),
            current_working_directory: Some("~/repo\u{1b}[31m".to_string()),
            mode: Some("copy".to_string()),
            agent_id: Some(format!("agent-{pane_id}")),
            agent_name: Some("manager".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("default".to_string()),
            history_position: Some("scroll:4".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(
            true,
            "#{session.id}|#{agent.active_count}|#{message.unread_count}",
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(
            true,
            "#{session.id}|#{pane.primary_pid}|#{pane.process_name}|#{pane.pwd}|#{pane.mode}|#{agent.id}|#{agent.name}|#{agent.status}|#{agent.model}|#{policy.mode}|#{observer.pending_count}|#{history.position}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), "$1|2|3");
    assert_eq!(
        rendered[1].trim_end(),
        format!(
            "$1|4242|bash[31m|~/repo[31m|copy|agent-{pane_id}|manager|running|default|full-access|1|scroll:4"
        )
    );
}

/// Verifies that the built-in default pane frame follows the spec guidance by
/// rendering pane identity without an idle or running agent marker. Agent
/// fields remain available only when users explicitly put them in a template.
#[test]
fn render_default_pane_frame_omits_agent_info() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 2).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            agent_status: Some("running".to_string()),
            agent_model: Some("default".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0], format!("{}{}", " 0 shell ", " ".repeat(23)));
    assert!(!rendered[0].contains("running"), "{}", rendered[0]);
    assert!(!rendered[0].contains("default"), "{}", rendered[0]);
}

/// Verifies render explicit pane frame template can show agent info.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn render_explicit_pane_frame_template_can_show_agent_info() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 2).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            agent_status: Some("running".to_string()),
            agent_model: Some("default".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            "#{pane.index}: #{pane.title} #{agent.status} #{agent.model}",
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), "0: shell running default");
}

/// Verifies that the built-in pane frame leaves working-directory display to
/// the window status area outside agent mode.
#[test]
fn render_default_pane_frame_omits_pwd_in_normal_mode() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(40, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            current_working_directory: Some("~/repo".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0], format!("{}{}", " 0 shell ", " ".repeat(31)));
}

/// Verifies that the built-in pane frame shows agent model, reasoning, and
/// state status on the right side only while the pane is in agent mode.
#[test]
fn render_default_pane_frame_right_aligns_agent_status_in_agent_mode() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(56, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(
        rendered[0],
        " 0 shell                      gpt-5.5   high   running  "
    );
}

/// Verifies that overlong pane-frame agent status text cannot consume the
/// rightmost horizontal border cell. This protects split-pane divider rows
/// where the pane frame merges into the horizontal boundary between stacked
/// panes and the status pills need to sit one cell left of the visible border.
#[test]
fn render_default_pane_frame_keeps_right_border_for_overlong_agent_status() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(36, 6).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let bottom_pane_id = window.panes()[1].id.to_string();
    let inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: vec![format!("pane{}", pane.index)],
        })
        .collect::<Vec<_>>();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        bottom_pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5-with-an-intentionally-long-name".to_string()),
            agent_reasoning: Some("extra-high".to_string()),
            agent_context_usage: Some("100%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(
        rendered[2].chars().last(),
        Some('\u{2500}'),
        "merged pane frame should leave a right-edge border cell: {:?}",
        rendered[2]
    );
}

/// Verifies the default pane-frame agent status group includes a context usage
/// pill immediately before the live state pill.
///
/// Context pressure is what drives automatic compaction, so agent mode exposes
/// the percentage alongside model and reasoning metadata without making it a
/// selectable model/reasoning control.
#[test]
fn render_default_pane_frame_right_aligns_context_usage_before_agent_status() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            agent_routing: Some("auto:on".to_string()),
            agent_preset: Some("openai".to_string()),
            agent_context_usage: Some("87%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert!(rendered[0].contains("gpt-5.5"), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" high "), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" route "), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" 87% "), "{:?}", rendered[0]);
    assert!(rendered[0].contains(" running "), "{:?}", rendered[0]);
    assert!(
        !rendered[0].contains("openai"),
        "default pane frame should not render the preset pill: {:?}",
        rendered[0]
    );
}

/// Verifies context usage has its own derived scale instead of borrowing the
/// agent-state blocked color. Context pressure is related to compaction, not the
/// current scheduler state, so the two pills should not collapse visually.
#[test]
fn render_context_usage_uses_distinct_pill_background() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            agent_context_usage: Some("87%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let context_start = display_column_for_fragment(&view.lines[0], "87%");
    let context_background = view.line_style_spans[0]
        .iter()
        .find(|span| {
            span.start <= context_start && span.start.saturating_add(span.length) > context_start
        })
        .and_then(|span| span.rendition.background)
        .unwrap();

    assert_ne!(
        context_background,
        config.ui_theme.colors.agent_status_blocked.background
    );
    assert_ne!(
        context_background,
        config.ui_theme.colors.agent_status_running.background
    );
}

/// Verifies that the built-in pane frame keeps agent status on the right side
/// without duplicating the working-directory pill now owned by the window
/// status area.
#[test]
fn render_default_pane_frame_right_aligns_agent_status_without_pwd() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(72, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            current_working_directory: Some("~/repos/mezzanine".to_string()),
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert!(rendered[0].contains("gpt-5.5   high   running"));
    assert!(!rendered[0].contains("~/repos/mezzanine"));
}

/// Verifies that the built-in pane frame styles each right-aligned agent status
/// field with a separate themed span and animates active work status. This keeps
/// model, reasoning, and state changes visually distinct while pane titles carry
/// subagent names.
#[test]
fn render_default_pane_frame_agent_status_uses_separate_themed_pills_without_name() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(84, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            current_working_directory: Some("~/repos/mezzanine".to_string()),
            mode: Some("agent".to_string()),
            agent_name: Some("Nova".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            agent_thinking: Some("on".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.animation_tick_ms = 720;
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        view.lines[0],
        " 0 shell                                       gpt-5.5   high   thinking   running  "
    );
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 0
            && span.length == " 0 shell  ".len()
            && span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
            && span.length == " gpt-5.5 ".len()
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
            && span.length == " high ".len()
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
            && span.length == " thinking ".len()
    }));
    assert!(!view.lines[0].contains("Nova"));
    assert!(!view.lines[0].contains("~/repos/mezzanine"));
    let status_start = display_column_for_fragment(&view.lines[0], "running");
    let status_end = status_start + "running".len();
    let status_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter_map(|span| span.rendition.background)
        .collect::<Vec<_>>();
    assert!(
        status_backgrounds.len() > 1,
        "{:?}",
        view.line_style_spans[0]
    );
    assert!(
        status_backgrounds
            .iter()
            .any(|color| *color != TerminalColor::Rgb(0x7e, 0x9c, 0xd8)),
        "{status_backgrounds:?}"
    );
    assert!(
        !status_backgrounds.contains(&TerminalColor::Rgb(0xe6, 0xc3, 0x84)),
        "running scan should derive a harmonious range from the running color instead of reusing the reasoning accent: {status_backgrounds:?}"
    );
}

/// Verifies active agent status animation uses a wider theme-relative color
/// range across all built-in palettes.
///
/// The scan is derived from the running-status background with neighboring
/// hues, so each theme should produce multiple related true-color backgrounds
/// with visible separation from the base color without borrowing an unrelated
/// pill accent.
#[test]
fn render_active_agent_status_gradient_uses_theme_relative_harmony() {
    fn rgb_distance(left: TerminalColor, right: TerminalColor) -> i32 {
        let TerminalColor::Rgb(left_red, left_green, left_blue) = left else {
            panic!("expected true-color left background: {left:?}");
        };
        let TerminalColor::Rgb(right_red, right_green, right_blue) = right else {
            panic!("expected true-color right background: {right:?}");
        };
        (i32::from(left_red) - i32::from(right_red)).abs()
            + (i32::from(left_green) - i32::from(right_green)).abs()
            + (i32::from(left_blue) - i32::from(right_blue)).abs()
    }

    for name in BUILTIN_UI_THEME_NAMES {
        let definition =
            builtin_ui_theme_definition(name).unwrap_or_else(|| panic!("missing theme {name}"));
        let theme = resolve_ui_theme(name, definition).expect("built-in theme must resolve");
        let mut ids = IdFactory::default();
        let window = Window::new(&mut ids, 0, "main", Size::new(62, 3).unwrap());
        let pane_id = window.panes()[0].id.to_string();
        let mut frame_context = TerminalFrameContext::default();
        frame_context.panes.insert(
            pane_id,
            TerminalPaneFrameContext {
                mode: Some("agent".to_string()),
                agent_name: Some("manager".to_string()),
                agent_status: Some("running".to_string()),
                agent_model: Some("gpt-5.5".to_string()),
                agent_reasoning: Some("high".to_string()),
                ..TerminalPaneFrameContext::default()
            },
        );
        frame_context.animation_tick_ms = 1440;
        let config = TerminalClientLoopConfig {
            frame_context,
            window_frames_enabled: false,
            pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
            ui_theme: theme.clone(),
            ..TerminalClientLoopConfig::default()
        };

        let view = render_attached_client_view(
            ClientViewRole::Primary,
            &window,
            &BTreeMap::new(),
            &config,
            window.size,
        )
        .unwrap()
        .unwrap();
        let status_start = display_column_for_fragment(&view.lines[0], "running");
        let status_end = status_start + "running".len();
        let mut unique_backgrounds = Vec::<TerminalColor>::new();
        for background in view.line_style_spans[0]
            .iter()
            .filter(|span| {
                span.start < status_end && span.start.saturating_add(span.length) > status_start
            })
            .filter_map(|span| span.rendition.background)
        {
            if !unique_backgrounds.contains(&background) {
                unique_backgrounds.push(background);
            }
        }

        assert!(
            unique_backgrounds.len() >= 3,
            "{name} should animate with a multi-stop gradient: {unique_backgrounds:?}"
        );
        assert!(
            unique_backgrounds.iter().any(|color| rgb_distance(
                *color,
                theme.colors.agent_status_running.background
            ) >= 30),
            "{name} should visibly widen the running-status range from its base color: {unique_backgrounds:?}"
        );
        assert!(
            !unique_backgrounds.contains(&theme.colors.agent_reasoning.background),
            "{name} should not reuse the reasoning pill accent as the running scan highlight"
        );
    }
}

/// Verifies reduced-motion mode keeps active agent statuses static while
/// preserving the ordinary running-status color category.
///
/// Users on slow terminals or who prefer no animation should still see the
/// active status pill, but its style should not vary per cell or per frame
/// tick.
#[test]
fn render_reduced_motion_agent_status_uses_static_running_style() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(62, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        reduced_motion: true,
        animation_tick_ms: 1440,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let status_start = display_column_for_fragment(&view.lines[0], "running");
    let status_end = status_start + "running".len();
    let unique_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter(|span| span.length < usize::from(window.size.columns))
        .filter_map(|span| span.rendition.background)
        .fold(Vec::<TerminalColor>::new(), |mut colors, background| {
            if !colors.contains(&background) {
                colors.push(background);
            }
            colors
        });

    assert_eq!(
        unique_backgrounds,
        vec![config.ui_theme.colors.agent_status_running.background]
    );
}

/// Verifies that a parent agent waiting on joined child agents renders an
/// explicit `waiting` status with the same animated running-status treatment.
///
/// The status text should distinguish subagent joins from approval blocks, and
/// the animation should continue to communicate that work is still active.
#[test]
fn render_default_pane_frame_agent_status_waiting_uses_running_scan() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(56, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("waiting".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.animation_tick_ms = 720;
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        view.lines[0],
        " 0 shell                      gpt-5.5   high   waiting  "
    );
    let status_start = display_column_for_fragment(&view.lines[0], "waiting");
    let status_end = status_start + "waiting".len();
    let status_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter_map(|span| span.rendition.background)
        .collect::<Vec<_>>();
    assert!(
        status_backgrounds.len() > 1,
        "{:?}",
        view.line_style_spans[0]
    );
    assert!(
        status_backgrounds
            .iter()
            .any(|color| *color != TerminalColor::Rgb(0x7e, 0x9c, 0xd8)),
        "{status_backgrounds:?}"
    );
}
/// Verifies that the routing substate reuses the animated running treatment so
/// auto-sizing stays visibly active while the router chooses a model.
#[test]
fn render_default_pane_frame_agent_status_routing_uses_running_scan() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(56, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("routing".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.animation_tick_ms = 720;
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        view.lines[0],
        " 0 shell                      gpt-5.5   high   routing  "
    );
    let status_start = display_column_for_fragment(&view.lines[0], "routing");
    let status_end = status_start + "routing".len();
    let status_backgrounds = view.line_style_spans[0]
        .iter()
        .filter(|span| {
            span.start < status_end && span.start.saturating_add(span.length) > status_start
        })
        .filter_map(|span| span.rendition.background)
        .collect::<Vec<_>>();
    assert!(
        status_backgrounds.len() > 1,
        "{:?}",
        view.line_style_spans[0]
    );
    assert!(
        status_backgrounds
            .iter()
            .any(|color| *color != TerminalColor::Rgb(0x7e, 0x9c, 0xd8)),
        "{status_backgrounds:?}"
    );
}

/// Verifies stopped agent turns use a muted status treatment instead of the
/// failed/error colors.
///
/// Stopping a turn is often user-directed control flow, so it should remain
/// distinguishable from a failed action without competing visually with real
/// errors in the pane frame.
#[test]
fn render_default_pane_frame_agent_status_stopped_is_muted() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(48, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("stopped".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let status_start = display_column_for_fragment(&view.lines[0], "stopped");
    let status_background = view.line_style_spans[0]
        .iter()
        .rev()
        .find(|span| {
            span.start <= status_start && span.start.saturating_add(span.length) > status_start
        })
        .and_then(|span| span.rendition.background)
        .unwrap();

    assert_eq!(
        status_background,
        config.ui_theme.colors.agent_status_idle.background
    );
    assert_ne!(
        status_background,
        config.ui_theme.colors.agent_status_failed.background
    );
}

/// Verifies that the default pane-frame agent pills expose mouse hit cells
/// across their padded pill surfaces. The picker and toggle paths rely on
/// these cells rather than text parsing, so this protects both visual spacing
/// and click targeting as one contract.
#[test]
fn render_default_pane_frame_agent_model_and_reasoning_pills_are_clickable() {
    fn cells_for_field(
        cells: &[crate::terminal::MousePaneAgentStatusCell],
        field: PaneAgentStatusField,
    ) -> Vec<u16> {
        cells
            .iter()
            .filter(|cell| cell.field == field)
            .map(|cell| cell.column)
            .collect::<Vec<_>>()
    }

    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(80, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_name: Some("manager".to_string()),
            agent_status: Some("running".to_string()),
            agent_model: Some("gpt-5.5".to_string()),
            agent_reasoning: Some("high".to_string()),
            agent_thinking: Some("on".to_string()),
            agent_routing: Some("auto:on".to_string()),
            agent_context_usage: Some("42%".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    frame_context.policy_mode = Some("full-access".to_string());
    let geometries = rendered_pane_geometries(&window, false).unwrap();

    let cells = pane_frame_agent_status_pillbox_cells(
        &window,
        &frame_context,
        DEFAULT_PANE_FRAME_TEMPLATE,
        TerminalFramePosition::Top,
        0,
        &geometries,
    );

    for field in [
        PaneAgentStatusField::Model,
        PaneAgentStatusField::Reasoning,
        PaneAgentStatusField::Thinking,
        PaneAgentStatusField::Routing,
        PaneAgentStatusField::ApprovalPolicy,
    ] {
        assert!(
            !cells_for_field(&cells, field).is_empty(),
            "{field:?} should expose clickable pane-frame cells: {cells:?}"
        );
    }
    let approval_columns = cells_for_field(&cells, PaneAgentStatusField::ApprovalPolicy);
    let reasoning_columns = cells_for_field(&cells, PaneAgentStatusField::Reasoning);
    let thinking_columns = cells_for_field(&cells, PaneAgentStatusField::Thinking);
    let routing_columns = cells_for_field(&cells, PaneAgentStatusField::Routing);
    assert!(
        approval_columns.iter().max() > routing_columns.iter().min(),
        "approval and routing pills should occupy distinct cells: {cells:?}"
    );
    assert!(
        reasoning_columns.iter().max() < thinking_columns.iter().min()
            && thinking_columns.iter().max() < routing_columns.iter().min(),
        "thinking should sit between reasoning and routing pills: {cells:?}"
    );
}

/// Verifies that entering agent mode reserves a persistent prompt row at the
/// bottom of the active pane and exposes that pane content region to clients.
#[test]
fn render_attached_client_view_reserves_agent_prompt_row() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(30, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut screen = TerminalScreen::new(Size::new(30, 3).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree");
    let mut screens = BTreeMap::new();
    screens.insert(pane_id.clone(), screen);
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_status: Some("idle".to_string()),
            agent_model: Some("default".to_string()),
            agent_reasoning: Some("medium".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.lines[3], format!("{:<30}", "▐ mez> "));
    assert_eq!(
        view.agent_prompt_region,
        Some(ReadlinePromptRegion {
            row: 1,
            column: 0,
            columns: 30,
            rows: 3,
        })
    );
}

/// Verifies that copy mode keeps the pane-local agent prompt reservation while
/// making the prompt itself invisible. Mouse selection uses copy mode for text
/// selection, and retaining the reserved row prevents the terminal buffer from
/// visually shifting when selection starts inside an agent pane.
#[test]
fn render_attached_client_view_keeps_agent_prompt_space_transparent_in_copy_mode() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(30, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut screen = TerminalScreen::new(Size::new(30, 4).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour");
    let mut screens = BTreeMap::new();
    screens.insert(pane_id.clone(), screen);
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("copy this");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            mode: Some("copy".to_string()),
            agent_prompt: Some(prompt),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.lines[2].contains("four"), "{:?}", view.lines);
    assert_eq!(view.lines[3], " ".repeat(30));
    assert!(
        view.lines.iter().all(|line| !line.contains("mez>")),
        "{:?}",
        view.lines
    );
}

/// Verifies that pane rendering uses the pane's retained agent prompt buffer
/// and progress rows directly, instead of relying on a modal full-window prompt
/// overlay. This keeps agent mode local to the pane while the rest of the mux
/// remains interactive.
#[test]
fn render_attached_client_view_draws_agent_prompt_state_in_pane() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(30, 5).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("first\nsecond");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            agent_display_lines: vec!["agent: turn turn-1 running".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let mut screens = BTreeMap::new();
    let mut screen = TerminalScreen::new(Size::new(30, 4).unwrap(), 10).unwrap();
    screen.feed(b"\n\n\npane output");
    screens.insert(pane_id, screen);
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.lines.iter().any(|line| line.contains("pane output")));
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("agent: turn turn-1 running")),
        "{:?}",
        view.lines
    );
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("▐ mez> first"))
    );
    assert!(view.lines.iter().any(|line| line.contains("second")));
    assert!(view.cursor_visible);
}

/// Verifies that active-pane footer reconciliation places live status in the
/// prompt row without leaving a stale pane-rendered copy behind.
///
/// The pane renderer may initially place transient display text on a blank
/// content row to avoid covering terminal output. The active prompt-region now
/// owns the live footer in the empty input line. Without clearing the first
/// copy, agent mode can show duplicate working status rows.
#[test]
fn render_attached_client_view_draws_one_agent_live_footer_at_prompt_edge() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 6).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let mut screens = BTreeMap::new();
    let mut screen = TerminalScreen::new(Size::new(64, 5).unwrap(), 10).unwrap();
    screen.feed(b"line00\nline01\n\nline03\nline04");
    screens.insert(pane_id, screen);
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    let prompt_row = view
        .lines
        .iter()
        .position(|line| line.contains("mez> running"))
        .unwrap();
    let footer_rows = view
        .lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| line.contains("esc to interrupt").then_some(row))
        .collect::<Vec<_>>();
    assert_eq!(footer_rows, vec![prompt_row], "{view:?}");
}

/// Verifies stale live-footer cleanup uses terminal cells rather than chars.
///
/// Wide glyphs in a neighboring split can make byte/char offsets differ from
/// terminal columns. The cleanup pass must still recognize and remove stale
/// agent footer text in the active pane so a new prompt-edge footer does not
/// leave behind a blank gutterless row or duplicate status line.
#[test]
fn render_agent_live_footer_cleanup_handles_wide_neighbor_glyphs() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(96, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let pane_id = window.panes()[1].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let mut screens = BTreeMap::new();
    let mut left = TerminalScreen::new(window.panes()[0].size, 10).unwrap();
    left.feed("✅ left".as_bytes());
    let mut right = TerminalScreen::new(window.panes()[1].size, 10).unwrap();
    right.feed("running (5m 39s • esc to interrupt)".as_bytes());
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(pane_id, right);
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let footer_rows = view
        .lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| line.contains("esc to interrupt").then_some(row))
        .collect::<Vec<_>>();

    assert_eq!(footer_rows.len(), 1, "{:?}", view.lines);
    assert!(
        view.lines
            .iter()
            .all(|line| !line.trim_end().is_empty() || !line.contains("▐")),
        "{:?}",
        view.lines
    );
}

/// Verifies typed agent prompt input hides the live footer until the prompt is
/// cleared again.
///
/// The live status is placeholder feedback for an empty agent prompt row. Once
/// the user starts composing a request, the row must prioritize editable input
/// and avoid competing status text.
#[test]
fn render_attached_client_view_hides_agent_live_footer_while_prompt_has_input() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(48, 5).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("write tests");
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("mez> write tests")),
        "{view:?}"
    );
    assert!(
        view.lines
            .iter()
            .all(|line| !line.contains("esc to interrupt")),
        "{view:?}"
    );
}

/// Verifies that the live agent footer renders the active state label with
/// grayscale scan-band motion over the prompt-bar background.
///
/// The state label uses the active grayscale scan while the timer and stop hint
/// remain readable as a muted static parenthetical.
#[test]
fn render_agent_working_footer_uses_prompt_background_grayscale_gradient() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let footer_row = view
        .lines
        .iter()
        .position(|line| line.contains("running (5m 40s • esc to interrupt)"))
        .expect("working footer should be visible");
    let footer_spans = &view.line_style_spans[footer_row];
    assert!(!footer_spans.is_empty());
    let footer_text = &view.lines[footer_row];
    let state_start_byte = footer_text.find("running").unwrap();
    let state_start = UnicodeWidthStr::width(&footer_text[..state_start_byte]);
    let prompt_background = config.ui_theme.colors.agent_prompt.background;
    assert!(footer_spans.iter().any(|span| span.start >= state_start
        && span.rendition.background == Some(prompt_background)
        && span.rendition.foreground.is_some()));
    let parenthetical_start_byte = footer_text.find(" (").unwrap();
    let parenthetical_start = UnicodeWidthStr::width(&footer_text[..parenthetical_start_byte]);
    let parenthetical = " (5m 40s • esc to interrupt)";
    let parenthetical_end = parenthetical_start + UnicodeWidthStr::width(parenthetical);
    let state_spans = footer_spans
        .iter()
        .filter(|span| {
            span.start >= state_start
                && span.start.saturating_add(span.length) <= parenthetical_start
                && span.rendition.foreground.is_some()
        })
        .collect::<Vec<_>>();
    let parenthetical_spans = footer_spans
        .iter()
        .filter(|span| {
            span.start >= parenthetical_start
                && span.start.saturating_add(span.length) <= parenthetical_end
                && span.rendition.background == Some(prompt_background)
                && span.rendition.foreground.is_some()
        })
        .collect::<Vec<_>>();
    assert!(!state_spans.is_empty(), "{footer_spans:?}");
    assert!(!parenthetical_spans.is_empty(), "{footer_spans:?}");
    assert!(
        parenthetical_spans
            .iter()
            .all(|span| matches!(span.rendition.foreground, Some(TerminalColor::Rgb(red, green, blue)) if red == green && green == blue)),
        "{parenthetical_spans:?}"
    );
    let mut foregrounds = Vec::new();
    for span in state_spans {
        if let Some(foreground) = span.rendition.foreground
            && !foregrounds.contains(&foreground)
        {
            foregrounds.push(foreground);
        }
    }
    assert!(foregrounds.len() >= 3, "{foregrounds:?}");
    assert!(
        foregrounds.iter().all(|color| match color {
            TerminalColor::Rgb(red, green, blue) => red == green && green == blue,
            _ => false,
        }),
        "{foregrounds:?}"
    );
    let levels = foregrounds
        .iter()
        .filter_map(|color| match color {
            TerminalColor::Rgb(red, _, _) => Some(*red),
            TerminalColor::Indexed(_) => None,
        })
        .collect::<Vec<_>>();
    let darkest = levels.iter().copied().min().unwrap_or_default();
    let brightest = levels.iter().copied().max().unwrap_or_default();
    assert!(brightest.saturating_sub(darkest) >= 24, "{foregrounds:?}");
}

/// Verifies the live agent footer switches to dark grayscale text on light
/// themes instead of using hardcoded light greys with weak contrast.
#[test]
fn render_agent_working_footer_uses_dark_grayscale_on_light_theme() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(64, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let definition = builtin_ui_theme_definition("catppuccin_latte").unwrap();
    let theme = resolve_ui_theme("catppuccin_latte", definition).unwrap();
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ui_theme: theme,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let footer_row = view
        .lines
        .iter()
        .position(|line| line.contains("running (5m 40s • esc to interrupt)"))
        .expect("working footer should be visible");
    let levels = view.line_style_spans[footer_row]
        .iter()
        .filter_map(|span| match span.rendition.foreground {
            Some(TerminalColor::Rgb(red, green, blue)) if red == green && green == blue => {
                Some(red)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(
        !levels.is_empty(),
        "{:?}",
        view.line_style_spans[footer_row]
    );
    assert!(
        levels.iter().all(|level| *level <= 0xa8),
        "light themes should use dark readable footer greys: {levels:?}"
    );
}

/// Verifies narrow panes keep live-footer state styling even when truncation
/// removes the trailing interrupt-hint suffix from the visible line.
#[test]
fn render_agent_working_footer_keeps_state_styling_when_suffix_is_truncated() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(18, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext {
        animation_tick_ms: 320,
        ..TerminalFrameContext::default()
    };
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(crate::readline::ReadlinePrompt::new(
                crate::readline::ReadlinePromptKind::Agent,
            )),
            agent_display_lines: vec!["running (5m 40s • esc to interrupt)".to_string()],
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let footer_row = view
        .lines
        .iter()
        .position(|line| line.contains("mez> running"))
        .expect("working footer should be visible");
    let footer_text = &view.lines[footer_row];
    let state_start_byte = footer_text.find("running").unwrap();
    let state_start = UnicodeWidthStr::width(&footer_text[..state_start_byte]);

    assert!(view.line_style_spans[footer_row].iter().any(|span| {
        span.start >= state_start
            && span.rendition.foreground.is_some()
            && span.rendition.background
                == Some(config.ui_theme.colors.agent_prompt.background)
    }), "{:?}", view.line_style_spans[footer_row]);
}

/// Verifies that scrollback position owns the right side of the default pane
/// header while copy-mode is away from the live bottom.
#[test]
fn render_default_pane_frame_scroll_position_replaces_agent_info() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let inputs = vec![PaneRenderInput {
        pane_id: pane_id.clone(),
        lines: vec!["body".to_string()],
    }];
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            agent_status: Some("running".to_string()),
            agent_model: Some("default".to_string()),
            history_position: Some("4/20".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
    .unwrap();

    assert_eq!(rendered[0], " 0 shell                   4/20 ");
    assert!(!rendered[0].contains('─'), "{}", rendered[0]);
    assert_eq!(rendered[1], "body                            ");
    assert!(!rendered[0].contains("running"), "{}", rendered[0]);
    assert!(!rendered[0].contains("default"), "{}", rendered[0]);
}

/// Verifies that the top pane status row uses the theme background instead of
/// box-drawing fill and carries the dedicated scroll-indicator background while
/// the scrollback position is visible.
#[test]
fn render_default_pane_frame_scroll_position_has_background_without_box_drawing_fill() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(32, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id,
        TerminalPaneFrameContext {
            history_position: Some("4/20".to_string()),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.lines[0], " 0 shell                   4/20 ");
    assert!(!view.lines[0].contains('─'), "{}", view.lines[0]);
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 27
            && span.length == 4
            && span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
    }));
}

/// Verifies that the built-in default window frame renders ordered window
/// pillboxes from runtime frame context rather than only the active window. This
/// keeps the foreground footer useful as a multi-window navigation surface,
/// gives the styled renderer concrete spans for highlighting the focused window
/// pill, and verifies unfocused subagent windows receive their distinct pill
/// color.
#[test]
fn render_default_window_frame_uses_window_pillbox_context() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 1, "work", Size::new(40, 3).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];
    let frame_context = TerminalFrameContext {
        windows: vec![
            TerminalWindowFrameContext {
                id: "@1".to_string(),
                index: 0,
                title: "shell".to_string(),
                active: false,
                subagent: true,
            },
            TerminalWindowFrameContext {
                id: "@2".to_string(),
                index: 1,
                title: "work".to_string(),
                active: true,
                subagent: false,
            },
        ],
        ..TerminalFrameContext::default()
    };

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_WINDOW_FRAME_TEMPLATE,
            TerminalFramePosition::Bottom,
        ),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered[2].trim_end(), " 0 shell   1 work");
    let mut config = TerminalClientLoopConfig {
        frame_context,
        window_frame_template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    config.window_frames_enabled = true;
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.start >= 10 && span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    }));
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.start == 0
            && span.rendition.background.is_some()
            && span.rendition.background != Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    }));
}

/// Verifies that the window status bar renders single-cell action pills
/// with mouse-addressable geometry and a distinct pressed style. This protects
/// the templated controls as clickable terminal UI rather than passive text.
#[test]
fn render_default_window_frame_action_pills_are_clickable_and_pressed() {
    let mut ids = IdFactory::default();
    let window = Window::new(
        &mut ids,
        0,
        "abcdefghijklmnopqrstuvwxZ",
        Size::new(80, 3).unwrap(),
    );
    let horizontal_split_action = WindowFrameAction::terminal_button("-", "split-window -h");
    let new_window_action = WindowFrameAction::terminal_button("□", "new-window");
    let frame_context = TerminalFrameContext {
        pressed_window_action: Some(new_window_action.clone()),
        window_status: Some(TerminalWindowStatusContext {
            template: DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            active_pane_working_directory: Some("~/repo".to_string()),
            system_uptime: "1h".to_string(),
            datetime_local: "2026-05-09 12:00:00".to_string(),
        }),
        windows: vec![TerminalWindowFrameContext {
            id: "@1".to_string(),
            index: 0,
            title: "abcdefghijklmnopqrstuvwxZ".to_string(),
            active: true,
            subagent: false,
        }],
        ..TerminalFrameContext::default()
    };
    let config = TerminalClientLoopConfig {
        frame_context: frame_context.clone(),
        window_frame_template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
        window_frames_enabled: true,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(
        view.lines[2].contains("-   +   □   ⊕   λ"),
        "{}",
        view.lines[2]
    );
    assert!(!view.lines[2].contains(" Δ"), "{}", view.lines[2]);
    assert_ne!(view.lines[2].chars().last(), Some(' '), "{}", view.lines[2]);
    let status_start = view.lines[2]
        .find(" ~/repo")
        .expect("default window status should render the active pane cwd");
    assert_eq!(
        view.lines[2]
            .chars()
            .nth(status_start.saturating_sub(1)),
        Some('w'),
        "window action pills should use every column before the right-status block: {}",
        view.lines[2]
    );
    let cells = window_frame_action_pillbox_cells(&frame_context, 2, window.size.columns);
    assert!(
        cells
            .iter()
            .any(|cell| cell.row == 2 && cell.action == horizontal_split_action),
        "horizontal split action pill should expose clickable cells"
    );
    let new_window_start = cells
        .iter()
        .filter(|cell| cell.row == 2 && cell.action == new_window_action)
        .map(|cell| cell.column)
        .min()
        .expect("new-window action pill should expose clickable cells");
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.start == usize::from(new_window_start)
            && span.length == 3
            && span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    }));
}

/// Verifies that group frame rendering appears only for multiple groups.
///
/// The group bar is a conditional top bar, so a default single-group session
/// must keep the full terminal height for the window while a multi-group
/// session reserves one top row with styled, mouse-addressable group pills.
#[test]
fn render_attached_view_uses_conditional_window_group_bar() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "shell", Size::new(40, 4).unwrap());
    let single_group_config = TerminalClientLoopConfig {
        frame_context: TerminalFrameContext {
            groups: vec![TerminalWindowGroupFrameContext {
                id: "g1".to_string(),
                index: 0,
                title: "default".to_string(),
                active: true,
            }],
            ..TerminalFrameContext::default()
        },
        ..TerminalClientLoopConfig::default()
    };

    let single_group_view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &single_group_config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(single_group_view.lines.len(), 4);
    assert!(
        !single_group_view.lines[0].contains("default"),
        "single-group sessions should not reserve the top group bar"
    );

    let multi_group_config = TerminalClientLoopConfig {
        frame_context: TerminalFrameContext {
            groups: vec![
                TerminalWindowGroupFrameContext {
                    id: "g1".to_string(),
                    index: 0,
                    title: "default".to_string(),
                    active: false,
                },
                TerminalWindowGroupFrameContext {
                    id: "g2".to_string(),
                    index: 1,
                    title: "work".to_string(),
                    active: true,
                },
            ],
            ..TerminalFrameContext::default()
        },
        ..TerminalClientLoopConfig::default()
    };

    let multi_group_view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &multi_group_config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(multi_group_view.lines.len(), 4);
    assert!(multi_group_view.lines[0].contains("0 default"));
    assert!(multi_group_view.lines[0].contains("1 work"));
    assert!(
        multi_group_view.line_style_spans[0].iter().any(|span| {
            span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
        })
    );
}

/// Verifies that the window bar can reserve a configurable right-aligned
/// status line and style action buttons, uptime, and local datetime separately.
/// This keeps the window list usable on the left while making dynamic status
/// items visually distinct and removable through the status template.
#[test]
fn render_window_status_uses_right_aligned_themed_segments() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 1, "work", Size::new(96, 3).unwrap());
    let frame_context = TerminalFrameContext {
        windows: vec![TerminalWindowFrameContext {
            id: "@2".to_string(),
            index: 1,
            title: "work".to_string(),
            active: true,
            subagent: false,
        }],
        window_status: Some(TerminalWindowStatusContext {
            template: DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            active_pane_working_directory: Some("~/repo".to_string()),
            system_uptime: "2d 03h 04m".to_string(),
            datetime_local: "2026-05-05 10:11:12".to_string(),
        }),
        ..TerminalFrameContext::default()
    };
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frame_template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
        window_frames_enabled: true,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.lines[2].contains("1 work"));
    assert!(view.lines[2].contains("-   +   □   ⊕   λ"));
    assert!(!view.lines[2].contains(" Δ"));
    assert!(view.lines[2].contains(" ~/repo "));
    assert!(view.lines[2].find(" ~/repo ").unwrap() < view.lines[2].find(" + ").unwrap());
    assert!(view.lines[2].contains(" 2d 03h 04m "));
    assert!(view.lines[2].contains(" 2026-05-05 10:11:12"));
    let uptime_start_bytes = view.lines[2].find(" 2d 03h 04m ").unwrap();
    let uptime_start = UnicodeWidthStr::width(&view.lines[2][..uptime_start_bytes]);
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
            && span.start == uptime_start
            && span.length == " 2d 03h 04m ".len()
    }));
    let datetime_start_bytes = view.lines[2].find(" 2026-05-05 10:11:12").unwrap();
    let datetime_start = UnicodeWidthStr::width(&view.lines[2][..datetime_start_bytes]);
    assert!(view.line_style_spans[2].iter().any(|span| {
        span.rendition.background == Some(TerminalColor::Rgb(0xe6, 0xc3, 0x84))
            && span.start == datetime_start
            && span.length == " 2026-05-05 10:11:12".len()
    }));
}

/// Verifies that the pane working-directory field used by window status and
/// explicit pane frame templates is compacted to the final three path segments.
///
/// The default window footer places `pane.pwd` at the left edge of the
/// right-status region, so deep project paths must not crowd out command pills
/// and clock fields. Explicit pane templates share the same named field and
/// must keep the same display contract for scrollback-aware pane frames.
#[test]
fn render_pane_pwd_fields_compact_deep_paths_to_three_segments() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 1, "work", Size::new(120, 3).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let frame_context = TerminalFrameContext {
        windows: vec![TerminalWindowFrameContext {
            id: "@2".to_string(),
            index: 1,
            title: "work".to_string(),
            active: true,
            subagent: false,
        }],
        panes: BTreeMap::from([(
            pane_id,
            TerminalPaneFrameContext {
                current_working_directory: Some("/var/tmp/a/b/c/d".to_string()),
                ..TerminalPaneFrameContext::default()
            },
        )]),
        window_status: Some(TerminalWindowStatusContext {
            template: DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            active_pane_working_directory: Some("~/Documents/a/b/c/d".to_string()),
            system_uptime: "2d 03h 04m".to_string(),
            datetime_local: "2026-05-05 10:11:12".to_string(),
        }),
        ..TerminalFrameContext::default()
    };
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &frame_context,
        TerminalFrameRenderOptions::plain(
            true,
            DEFAULT_WINDOW_FRAME_TEMPLATE,
            TerminalFramePosition::Bottom,
        ),
        TerminalFrameRenderOptions::plain(true, "#{pane.pwd}", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered[0].trim_end(), "…/b/c/d");
    assert!(rendered[2].contains(" …/b/c/d "), "{}", rendered[2]);
    assert!(!rendered[2].contains("~/Documents/a"), "{}", rendered[2]);
}

/// Verifies that split-pane box drawing glyphs carry only a foreground color
/// and use the active-pane border color when the glyph encloses the active
/// pane. Background fill remains reserved for text spans on frame bars.
#[test]
fn render_active_pane_border_glyphs_are_foreground_only() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    let border_column = display_column_for_fragment(&view.lines[0], "\u{2502}");
    let border_span = view.line_style_spans[0]
        .iter()
        .find(|span| span.start == border_column)
        .unwrap();

    assert_eq!(
        border_span.rendition.foreground,
        Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
    );
    assert_eq!(border_span.rendition.background, None);
}

/// Verifies that pane status rows merged into divider rows keep backgrounds
/// only on title/status pills. The horizontal divider itself and its boundary
/// junctions remain foreground-only connected box-drawing cells so split lines
/// do not become filled status bars or lose their interior tee glyphs.
#[test]
fn render_merged_pane_frame_fills_status_bar_and_preserves_vertical_separators() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(28, 6).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    let merged_row = view
        .lines
        .iter()
        .position(|line| line.contains(" 2 she"))
        .expect("bottom-right pane frame should merge into divider row");
    let frame_text = " 2 she";
    assert!(view.lines[merged_row].contains(frame_text));
    let title_span = view.line_style_spans[merged_row]
        .iter()
        .find(|span| {
            span.length >= frame_text.len()
                && span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
        })
        .copied()
        .expect("merged status title should carry the title-pill background");
    let horizontal_column = view.lines[merged_row]
        .chars()
        .position(|ch| ch == '\u{2500}')
        .expect("merged divider row should retain horizontal box drawing fill");
    let horizontal_span = view.line_style_spans[merged_row]
        .iter()
        .rev()
        .find(|span| {
            horizontal_column >= span.start
                && horizontal_column < span.start.saturating_add(span.length)
        })
        .expect("horizontal divider fill should be styled");
    assert_eq!(horizontal_span.rendition.background, None);
    assert!(
        view.line_style_spans[merged_row].iter().any(|span| {
            span.start == title_span.start
                && span.length >= frame_text.len()
                && span.rendition.foreground == Some(TerminalColor::Rgb(0xdc, 0xd7, 0xba))
                && span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
        }),
        "{:?}",
        view.line_style_spans[merged_row]
    );

    let junction_column = title_span.start.saturating_sub(1);
    assert_eq!(
        view.lines[merged_row].chars().nth(junction_column),
        Some('\u{251c}')
    );
    let junction_span = view.line_style_spans[merged_row]
        .iter()
        .rev()
        .find(|span| {
            junction_column >= span.start
                && junction_column < span.start.saturating_add(span.length)
        })
        .expect("merged status junction should be styled");
    assert_eq!(junction_span.rendition.background, None);

    let vertical_row = view
        .lines
        .iter()
        .position(|line| line.contains(" 0 shell") && line.contains(" 1 shell"))
        .unwrap();
    let vertical_column = view.lines[vertical_row]
        .chars()
        .position(|ch| ch == '\u{2502}')
        .unwrap();
    let vertical_span = view.line_style_spans[vertical_row]
        .iter()
        .rev()
        .find(|span| {
            vertical_column >= span.start
                && vertical_column < span.start.saturating_add(span.length)
        })
        .expect("vertical separator should be styled");
    assert_eq!(vertical_span.rendition.background, None);
}

/// Verifies merged pane-frame rows preserve right-side tee intersections when
/// the pane status region ends at a full-height neighboring pane's divider.
#[test]
fn render_merged_pane_frame_preserves_right_side_tee_junction() {
    let window = window_from_test_geometries(
        Size::new(28, 6).unwrap(),
        vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 14,
                rows: 3,
            },
            PaneGeometry {
                index: 1,
                column: 0,
                row: 3,
                columns: 14,
                rows: 3,
            },
            PaneGeometry {
                index: 2,
                column: 14,
                row: 0,
                columns: 14,
                rows: 6,
            },
        ],
    );
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frame_template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    let merged_row = 2;
    let junction_column = 13;
    assert_eq!(
        view.lines[merged_row].chars().nth(junction_column),
        Some('\u{2524}'),
        "{:?}",
        view.lines[merged_row]
    );
    let junction_span = view.line_style_spans[merged_row]
        .iter()
        .rev()
        .find(|span| {
            junction_column >= span.start
                && junction_column < span.start.saturating_add(span.length)
        })
        .expect("right-side tee junction should be styled");

    assert_eq!(junction_span.rendition.background, None);
}

/// Verifies that configured frame positions can place pane and window frame
/// rows after body content while preserving the authoritative window height.
#[test]
fn render_frame_positions_can_place_frames_at_bottom() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(12, 3).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(true, "window", TerminalFramePosition::Bottom),
        TerminalFrameRenderOptions::plain(true, "pane", TerminalFramePosition::Bottom),
    )
    .unwrap();

    assert_eq!(
        rendered,
        vec!["body        ", "pane        ", "window      "]
    );
}

/// Verifies that configured frame styles are exposed as styled-line spans so
/// attached terminal output can replay them as SGR instead of plain text only.
/// Pane title rows include a subtle full-row theme fill and a stronger text
/// span for the configured title style.
#[test]
fn render_frame_styles_apply_to_styled_frame_lines() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(12, 3).unwrap());
    let mut config = TerminalClientLoopConfig {
        window_frames_enabled: true,
        window_frame_template: "window".to_string(),
        window_frame_style: TerminalFrameStyle::Inverse,
        pane_frames_enabled: true,
        pane_frame_template: "pane".to_string(),
        pane_frame_style: TerminalFrameStyle::Bold,
        ..TerminalClientLoopConfig::default()
    };
    config.window_frame_position = TerminalFramePosition::Top;
    config.pane_frame_position = TerminalFramePosition::Top;

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.line_style_spans[0][0].rendition.inverse);
    assert_eq!(view.line_style_spans[1][0].length, 12);
    assert!(
        view.line_style_spans[1]
            .iter()
            .any(|span| { span.length == 4 && span.rendition.bold })
    );
}

/// Verifies that a framed window never grows beyond the authoritative window
/// height when there is only enough vertical space for the window frame row.
#[test]
fn render_window_frame_fits_single_row_window() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(12, 1).unwrap());
    let inputs = vec![PaneRenderInput {
        pane_id: window.panes()[0].id.to_string(),
        lines: vec!["body".to_string()],
    }];

    let rendered = render_window_with_pane_frame_template(
        &window,
        &inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(
            true,
            "#{window.index}:#{window.name}",
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(false, "", TerminalFramePosition::Top),
    )
    .unwrap();

    assert_eq!(rendered, vec!["0:main      "]);
}

/// Verifies that after text wraps to the next line, cursor-back and
/// erase-to-end-of-line operations clear the wrapped text and the cursor
/// lands at the correct position. This simulates what readline does when
/// the user presses backspace inside multi-line wrapped input.
#[test]
fn terminal_screen_erases_wrapped_text_on_backspace() {
    let mut screen = TerminalScreen::new(Size::new(5, 3).unwrap(), 10).unwrap();

    screen.feed(b"abcde");
    assert_eq!(screen.visible_lines(), vec!["abcde", "", ""]);
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 4);

    screen.feed(b"f");
    assert_eq!(screen.visible_lines(), vec!["abcde", "f", ""]);
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 1);

    screen.feed(b"ghij");
    assert_eq!(screen.visible_lines(), vec!["abcde", "fghij", ""]);
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 4);

    screen.feed(b"k");
    assert_eq!(screen.visible_lines(), vec!["abcde", "fghij", "k"]);
    assert_eq!(screen.cursor_state().row, 2);
    assert_eq!(screen.cursor_state().column, 1);

    screen.feed(b"\x1b[D\x1b[K");
    assert!(
        screen.visible_lines()[2].is_empty(),
        "row 2 should be erased"
    );
    assert_eq!(screen.cursor_state().row, 2);
    assert_eq!(screen.cursor_state().column, 0);

    screen.feed(b"\x1b[A\x1b[4C");
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 4);

    screen.feed(b"\x1b[K");
    assert!(
        screen.visible_lines()[1].starts_with("fghi"),
        "last char on row 1 should be erased: {:?}",
        screen.visible_lines()
    );
    assert!(screen.visible_lines()[2].is_empty());
}

/// Verifies that absolute horizontal cursor movement is honored. Full-screen
/// applications such as htop use CHA/HPA to return to a gauge column on the
/// current row; ignoring it causes the gauge to wrap onto the next line.
#[test]
fn terminal_screen_honors_horizontal_absolute_cursor_movement() {
    let mut screen = TerminalScreen::new(Size::new(20, 3).unwrap(), 10).unwrap();

    screen.feed(b"CPU0: 12%\x1b[12G[||||]");

    assert_eq!(screen.visible_lines()[0], "CPU0: 12%  [||||]");
    assert!(screen.visible_lines()[1].is_empty());
}

/// Verifies that shrinking a pane with content at the live bottom preserves the
/// bottom of the viewport. Shell prompts usually live at the bottom edge, so a
/// top/bottom split must keep the latest line visible after the PTY grid shrinks.
#[test]
fn terminal_screen_resize_shrink_preserves_bottom_when_content_overflows() {
    let mut screen = TerminalScreen::new(Size::new(8, 5).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour\nfive");

    screen.resize(Size::new(8, 3).unwrap());

    assert_eq!(screen.visible_lines(), vec!["three", "four", "five"]);
    assert_eq!(screen.cursor_state().row, 2);
}

/// Verifies that the resize bottom-preservation rule is limited to overflowing
/// content or a cursor below the new bottom. Sparse top-aligned content should
/// not jump when a pane shrinks.
#[test]
fn terminal_screen_resize_shrink_keeps_top_when_content_fits() {
    let mut screen = TerminalScreen::new(Size::new(8, 5).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo");

    screen.resize(Size::new(8, 3).unwrap());

    assert_eq!(screen.visible_lines(), vec!["one", "two", ""]);
}

/// Verifies that pane-width changes reflow soft-wrapped terminal content instead
/// of discarding cells outside the narrower viewport. This protects drag-resize
/// behavior where a neighboring pane temporarily obscures content and then moves
/// back to reveal it again.
#[test]
fn terminal_screen_resize_reflows_and_restores_soft_wrapped_content() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    screen.feed(b"abcdefghijklmn");

    assert_eq!(screen.visible_lines(), vec!["abcdefghij", "klmn", ""]);

    screen.resize(Size::new(5, 3).unwrap());
    assert_eq!(screen.visible_lines(), vec!["abcde", "fghij", "klmn"]);

    screen.resize(Size::new(10, 3).unwrap());
    assert_eq!(screen.visible_lines(), vec!["abcdefghij", "klmn", ""]);
}

/// Verifies agent transcript rows keep their visual gutter on soft-wrap
/// continuation rows. Agent output is rendered into the same pane buffer as
/// shell output, so the screen model has to add display-only gutters when
/// terminal wrapping happens instead of relying only on runtime preformatting.
#[test]
fn terminal_screen_soft_wraps_agent_transcript_rows_with_gutter() {
    let mut screen = TerminalScreen::new(Size::new(12, 4).unwrap(), 10).unwrap();

    screen.feed("\x1b[31m▐ mez> \x1b[0mabcdefghi".as_bytes());

    assert_eq!(screen.visible_lines()[0], "▐ mez> abcde");
    assert_eq!(screen.visible_lines()[1], "▐ fghi");
}

/// Verifies ordinary hosted terminal output that happens to start with the
/// Mezzanine gutter glyph remains normal terminal output. Agent transcript
/// wrapping is keyed by the styled gutter that Mezzanine injects, so unstyled
/// application text must not gain synthetic continuation gutters.
#[test]
fn terminal_screen_does_not_agent_gutter_wrap_unstyled_pane_output() {
    let mut screen = TerminalScreen::new(Size::new(12, 4).unwrap(), 10).unwrap();

    screen.feed("▐ plain abcdefghi".as_bytes());

    assert_eq!(screen.visible_lines()[0], "▐ plain abcd");
    assert_eq!(screen.visible_lines()[1], "efghi");
}

/// Verifies resize reflow preserves agent transcript gutters on every
/// continuation row without treating those gutters as model-authored text.
/// This protects pane split and terminal resize paths, which rebuild physical
/// rows from wrapped logical lines after the agent transcript already exists
/// in the terminal buffer.
#[test]
fn terminal_screen_reflows_agent_transcript_rows_with_gutter() {
    let mut screen = TerminalScreen::new(Size::new(12, 5).unwrap(), 10).unwrap();
    screen.feed("\x1b[31m▐ mez> \x1b[0mabcdefghi".as_bytes());

    screen.resize(Size::new(16, 5).unwrap());
    assert_eq!(screen.visible_lines()[0], "▐ mez> abcdefghi");
    assert_eq!(screen.visible_lines()[1], "");

    screen.resize(Size::new(10, 5).unwrap());
    assert_eq!(screen.visible_lines()[0], "▐ mez> abc");
    assert_eq!(screen.visible_lines()[1], "▐ defghi");
}
/// Verifies that row-only pane resizes preserve scrollback and visible content
/// without reflowing wrapped history or pulling retained scrollback back into
/// view when the pane later grows.
///
/// Horizontal pane splits and pane-closure expansion change only the height.
/// Once a shrink has chosen the visible tail, later growth must leave that
/// rendered tail stationary unless a further shrink would otherwise truncate it.
#[test]
fn terminal_screen_row_only_resize_preserves_history_and_visible_rows() {
    let mut screen = TerminalScreen::new(Size::new(5, 3).unwrap(), 10).unwrap();
    screen.feed(b"11111\r\n22222\r\n33333\r\n44444");

    screen.resize(Size::new(5, 2).unwrap());
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["11111", "22222"]
    );
    assert_eq!(screen.visible_lines(), vec!["33333", "44444"]);

    screen.resize(Size::new(5, 4).unwrap());
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["11111", "22222"]
    );
    assert_eq!(screen.visible_lines(), vec!["33333", "44444", "", ""]);
}
/// Verifies that shrinking a pane height without cutting into the visible tail
/// keeps the live viewport stationary instead of filling newly exposed rows
/// from scrollback.
///
/// Over/under splits reduce only the row count. When the currently visible
/// content already fits within the new height, the shrink must drop blank rows
/// from the bottom of the grid and leave retained history untouched.
#[test]
fn terminal_screen_row_only_resize_keeps_stationary_view_when_tail_fits() {
    let mut screen = TerminalScreen::new(Size::new(5, 5).unwrap(), 10).unwrap();
    screen.restore_normal_content(
        &[
            "old-1".to_string(),
            "old-2".to_string(),
            "old-3".to_string(),
        ],
        &["live1".to_string(), "live2".to_string()],
    );
    screen.resize(Size::new(5, 3).unwrap());
    assert_eq!(screen.visible_lines(), vec!["live1", "live2", ""]);
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 0);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["old-1", "old-2", "old-3"]
    );
}
/// Verifies that width-changing pane resizes keep latency bounded and preserve
/// viewport position by leaving scrollback in its stored physical rows.
/// Side-by-side pane splits halve columns, so they must not synchronously
/// rebuild retained history or pull the retained tail into the new viewport.
#[test]
fn terminal_screen_width_resize_reflows_only_live_viewport() {
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 600).unwrap();
    for index in 0..520 {
        screen.feed(format!("old-{index:03}-abcdefghijkl\r\n").as_bytes());
    }
    screen.feed(b"live-one\r\nlive-two\r\nlive-three\r\nlive-four");
    let before_history_len = screen.history().len();
    let before_prefix = screen
        .history()
        .lines()
        .take(8)
        .map(str::to_string)
        .collect::<Vec<_>>();
    screen.resize(Size::new(10, 4).unwrap());
    assert_eq!(screen.history().len(), before_history_len);
    assert_eq!(
        screen
            .history()
            .lines()
            .take(8)
            .map(str::to_string)
            .collect::<Vec<_>>(),
        before_prefix
    );
    let visible_lines = screen.visible_lines();
    assert_eq!(visible_lines.len(), 4);
    assert!(visible_lines.iter().any(|line| line.contains("live-one")));
    assert!(visible_lines.iter().any(|line| line.contains("live-two")));
    assert!(
        visible_lines
            .iter()
            .all(|line| terminal_text_width(line) <= 10 && !line.contains("old-"))
    );
}

/// Verifies resize cursor restoration counts display-only agent gutter
/// continuations. Without this, the cursor below a long agent transcript could
/// be restored one row too high after a pane resize because cursor mapping
/// counted logical text width but not the extra continuation gutter cells.
#[test]
fn terminal_screen_resize_counts_agent_gutters_when_restoring_cursor() {
    let mut screen = TerminalScreen::new(Size::new(12, 6).unwrap(), 10).unwrap();
    screen.feed("\x1b[31m▐ mez> \x1b[0mabcdefghijklmnopqrst\r\nnext".as_bytes());

    screen.resize(Size::new(10, 6).unwrap());

    assert_eq!(screen.visible_lines()[4], "next");
    assert_eq!(screen.cursor_state().row, 4);
    assert_eq!(screen.cursor_state().column, 4);
}

/// Verifies that copy-text annotations on rows dropped during a top-anchored
/// shrink are committed to scrollback history so they remain recoverable.
#[test]
fn terminal_screen_resize_shrink_preserves_dropped_row_copy_text_in_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 5).unwrap(), 10).unwrap();
    screen.feed(b"line0\nline1");
    // Annotate rows that will be dropped when shrinking to 3 rows.
    screen.line_copy_texts[3] = Some("copy-three".to_string());
    screen.line_copy_texts[4] = Some("copy-four".to_string());

    screen.resize(Size::new(10, 3).unwrap());

    // Dropped rows 3 and 4 must land in history with their copy-text intact.
    let history_styled: Vec<_> = screen.history().styled_lines().collect();
    assert_eq!(history_styled.len(), 2);
    assert_eq!(history_styled[0].copy_text.as_deref(), Some("copy-three"));
    assert_eq!(history_styled[1].copy_text.as_deref(), Some("copy-four"));
    assert_eq!(screen.visible_lines(), vec!["line0", "line1", ""]);
}
