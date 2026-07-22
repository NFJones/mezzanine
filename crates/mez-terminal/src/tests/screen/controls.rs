//! Regression tests for terminal screen controls behavior.

use crate::{TerminalCursorState, TerminalModeState, TerminalScreen, TerminalSize as Size};

/// Verifies terminal screen prints line oriented output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_prints_line_oriented_output() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"hello\r\nworld");

    assert_eq!(screen.visible_lines()[0], "hello");
    assert_eq!(screen.visible_lines()[1], "world");
}

/// Verifies terminal screen tracks activity and bell events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_activity_and_bell_events() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"hello\x07");
    screen.feed(b"world\x07\x07");

    assert_eq!(screen.activity_events(), 2);
    assert_eq!(screen.bell_events(), 3);
    assert_eq!(screen.visible_lines()[0], "helloworld");
}

/// Verifies terminal screen handles cursor address and clear line.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_cursor_address_and_clear_line() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"abcdef\x1b[1;3H\x1b[Kxy");

    assert_eq!(screen.visible_lines()[0], "abxy");
}

/// Verifies DEC origin mode makes CUP and VPA rows relative to the active
/// scroll region.
///
/// This regression scenario documents that changing the scroll region while
/// DECOM is active homes the cursor at the top margin and later row-addressing
/// commands stay relative to that margin.
#[test]
fn terminal_screen_origin_mode_offsets_cursor_addressing_into_scroll_region() {
    let mut screen = TerminalScreen::new(Size::new(6, 5).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?6h\x1b[2;4rA\x1b[2;3HB\x1b[3d\x1b[1GC");

    assert_eq!(screen.visible_lines(), vec!["", "A", "  B", "C", ""]);
}

/// Verifies terminal screen handles relative cursor movement and c0 controls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_relative_cursor_movement_and_c0_controls() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"top\r\nmid\r\nbot\x1b[A\x1b[2DZZ\x1b[B\x1b[CQ\r!\tT\x08?");

    assert_eq!(screen.visible_lines()[0], "top");
    assert_eq!(screen.visible_lines()[1], "mZZ");
    assert_eq!(screen.visible_lines()[2], "!ot Q   ?");
}

/// Verifies terminal screen handles erase display variants.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_erase_display_variants() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"abc\r\n123\r\nxyz\x1b[2;2H\x1b[J");
    assert_eq!(screen.visible_lines(), vec!["abc", "1", ""]);

    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[2Jabc\r\n123\r\nxyz\x1b[2;2H\x1b[1J");
    assert_eq!(screen.visible_lines(), vec!["", "  3", "xyz"]);

    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[2Jdone");
    assert_eq!(screen.visible_lines()[0], "done");

    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();
    screen.feed(b"one\r\ntwo\r\nthree\x1b[3J");
    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[1], "three");
}

/// Verifies terminal device status reports queue pane-directed reply bytes.
///
/// Full-screen terminal applications query cursor position with CSI 6n and
/// expect the terminal emulator to write a 1-based CPR response back to the
/// pane process rather than rendering the query or silently dropping it.
#[test]
fn terminal_screen_queues_device_status_report_replies() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[2;4H\x1b[6n\x1b[5n");

    assert_eq!(screen.drain_terminal_response_bytes(), b"\x1b[2;4R\x1b[0n");
    assert!(screen.drain_terminal_response_bytes().is_empty());
}

/// Verifies CPR replies report the live cursor row after DECOM-relative
/// addressing inside a scroll region.
///
/// Full-screen TUIs often combine origin mode, scroll margins, and cursor
/// position queries while redrawing bounded regions. This regression ensures
/// the terminal reports the resolved cursor position after DECOM-adjusted CUP
/// addressing instead of leaking the relative parameter row or dropping the
/// reply.
#[test]
fn terminal_screen_reports_cpr_after_origin_mode_relative_addressing() {
    let mut screen = TerminalScreen::new(Size::new(6, 5).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?6h\x1b[2;4r\x1b[2;3H\x1b[6n");

    assert_eq!(screen.drain_terminal_response_bytes(), b"\x1b[3;3R");
    assert!(screen.drain_terminal_response_bytes().is_empty());
}

/// Verifies overlong CSI sequences are dropped with deterministic recovery.
///
/// CSI parameters can be split across reads and may be attacker controlled.
/// The parser must bound retained bytes, avoid emitting replies from truncated
/// sequences, and recover at the final byte so later valid CSI traffic works.
#[test]
fn terminal_screen_bounds_csi_accumulation_and_recovers_after_final_byte() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    let mut overlong = Vec::from(b"\x1b[".as_slice());
    overlong.extend(std::iter::repeat_n(b'1', 2048));
    overlong.push(b'n');

    screen.feed(&overlong);
    assert!(screen.drain_terminal_response_bytes().is_empty());

    screen.feed(b"\x1b[2;3H\x1b[6n");

    assert_eq!(screen.drain_terminal_response_bytes(), b"\x1b[2;3R");
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
    screen.feed(b"visible");
    screen.set_recent_normal_copy_texts(&["hidden raw copy".to_string()], "");

    screen.feed(b"\x1b[2K");

    assert_eq!(screen.visible_styled_lines()[0].copy_text, None);
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

    screen.feed(b"ab\x1b7cd\x1b8XY\r\n12\x1b[s34\x1b[uZZ");

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
    screen.feed(b"one\r\ntwo\r\nthree\r\nfour");
    screen.feed(b"\x1b[2;4r\x1b[2;1H\x1b[L");
    assert_eq!(screen.visible_lines(), vec!["one", "", "two", "three"]);

    screen.feed(b"\x1b[2;1H\x1b[M");
    assert_eq!(screen.visible_lines(), vec!["one", "two", "three", ""]);

    screen.feed(b"\x1b[2;4r\x1b[4;1H\n");
    assert_eq!(screen.visible_lines(), vec!["one", "three", "", ""]);
    assert!(screen.history().is_empty());
}

/// Verifies VT-style LF, IND, and NEL line movement semantics, including LNM
/// defaults and explicit transitions. Full-screen applications use these
/// controls inside scroll regions, so LF/IND must keep the current column
/// unless ANSI line-feed/newline mode is explicitly enabled, while NEL returns
/// to column zero.
#[test]
fn terminal_screen_handles_vt_line_movement_controls() {
    let mut screen = TerminalScreen::new(Size::new(8, 4).unwrap(), 10).unwrap();

    screen.feed(b"ab\ncd");
    assert_eq!(screen.visible_lines(), vec!["ab", "  cd", "", ""]);
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 4);

    screen.feed(b"\x1b[20h\nef");
    assert_eq!(screen.visible_lines(), vec!["ab", "  cd", "ef", ""]);
    assert_eq!(screen.cursor_state().row, 2);
    assert_eq!(screen.cursor_state().column, 2);

    screen.feed(b"\x1b[20l\ngh");
    assert_eq!(screen.visible_lines(), vec!["ab", "  cd", "ef", "  gh"]);
    assert_eq!(screen.cursor_state().row, 3);
    assert_eq!(screen.cursor_state().column, 4);

    let mut screen = TerminalScreen::new(Size::new(8, 4).unwrap(), 10).unwrap();

    screen.feed(b"\x1bDef");
    assert_eq!(screen.visible_lines(), vec!["", "ef", "", ""]);
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 2);

    screen.feed(b"\x1bEgh");
    assert_eq!(screen.visible_lines(), vec!["", "ef", "gh", ""]);
    assert_eq!(screen.cursor_state().row, 2);
    assert_eq!(screen.cursor_state().column, 2);
}

/// Verifies relative vertical cursor movement stays inside the active scroll
/// region while DEC origin mode is enabled. TUIs combine DECOM, margins, and
/// relative movement during incremental redraws, so CUU/CUD must not escape
/// the region in that mode.
#[test]
fn terminal_screen_origin_mode_clamps_relative_vertical_movement_to_scroll_region() {
    let mut screen = TerminalScreen::new(Size::new(8, 5).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[2;4r\x1b[?6h");
    assert_eq!(screen.cursor_state().row, 1);

    screen.feed(b"\x1b[10A");
    assert_eq!(screen.cursor_state().row, 1);

    screen.feed(b"\x1b[10B");
    assert_eq!(screen.cursor_state().row, 3);

    screen.feed(b"\x1b[?6l\x1b[10A");
    assert_eq!(screen.cursor_state().row, 0);
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

    screen.feed(b"\x1b[?1002h");
    assert!(screen.application_mouse_enabled());
    assert!(!screen.application_sgr_mouse_enabled());
    assert!(!screen.mode_state().normal_mouse_tracking_enabled);
    assert!(screen.mode_state().button_event_mouse_tracking_enabled);
    assert!(!screen.mode_state().any_event_mouse_tracking_enabled);

    screen.feed(b"\x1b[?1006h");
    assert!(screen.application_mouse_enabled());
    assert!(screen.application_sgr_mouse_enabled());

    screen.feed(b"\x1b[?1000h");
    assert!(screen.mode_state().normal_mouse_tracking_enabled);
    assert!(screen.mode_state().button_event_mouse_tracking_enabled);

    screen.feed(b"\x1b[?1000l");
    assert!(!screen.mode_state().normal_mouse_tracking_enabled);
    assert!(screen.mode_state().button_event_mouse_tracking_enabled);
    assert!(screen.application_sgr_mouse_enabled());

    screen.feed(b"\x1b[?1003h");
    assert!(screen.mode_state().any_event_mouse_tracking_enabled);

    screen.feed(b"\x1b[?1002l");
    assert!(!screen.mode_state().button_event_mouse_tracking_enabled);
    assert!(screen.mode_state().any_event_mouse_tracking_enabled);

    screen.feed(b"\x1b[?1003l\x1b[?1006l");
    assert!(!screen.application_mouse_enabled());
    assert!(!screen.application_sgr_mouse_enabled());
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
        normal_mouse_tracking_enabled: false,
        button_event_mouse_tracking_enabled: true,
        any_event_mouse_tracking_enabled: false,
        sgr_mouse_enabled: true,
        application_cursor_enabled: true,
        origin_mode_enabled: false,
        autowrap_enabled: false,
        application_keypad_enabled: true,
        focus_events_enabled: true,
    };

    screen.restore_mode_state(&state);

    assert_eq!(screen.mode_state(), state);
    assert_eq!(screen.title(), Some("snapshot-title"));
    assert!(!screen.cursor_visible());
    assert!(screen.bracketed_paste_enabled());
    assert!(screen.application_mouse_enabled());
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
    original.feed(b"ab\x1b[s\x1b[?1;1002;1006;2004h\x1b[?1;1002;1006;2004s\x1b[?1;1002;1006;2004l");
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
    assert!(
        saved_state
            .saved_dec_private_modes
            .iter()
            .any(|mode| mode.mode == 1002 && mode.enabled)
    );

    let mut restored = TerminalScreen::new(Size::new(10, 4).unwrap(), 10).unwrap();
    restored.restore_saved_state(&saved_state);
    restored.feed(b"zz\x1b[uXY\x1b[?1;1002;1006;2004r");

    assert_eq!(restored.visible_lines()[0], "zzXY");
    assert!(restored.application_cursor_enabled());
    assert!(restored.application_sgr_mouse_enabled());
    assert!(restored.bracketed_paste_enabled());
    assert!(!restored.mode_state().normal_mouse_tracking_enabled);
    assert!(restored.mode_state().button_event_mouse_tracking_enabled);
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

/// Verifies that CSI cursor movement sequences (CUU, CUD, CUF, CUB) move the
/// cursor correctly within the terminal grid, and that movement beyond grid
/// boundaries is clamped to the last row/column.
#[test]
fn terminal_screen_csi_cursor_movement() {
    let size = Size::new(10, 8).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(b"\x1b[5B"); // CUD 5
    assert_eq!(screen.cursor_state().row, 5);
    assert_eq!(screen.cursor_state().column, 0);

    screen.feed(b"\x1b[8C"); // CUF 8
    assert_eq!(screen.cursor_state().column, 8);

    screen.feed(b"\x1b[3A"); // CUU 3
    assert_eq!(screen.cursor_state().row, 2);

    screen.feed(b"\x1b[4D"); // CUB 4
    assert_eq!(screen.cursor_state().column, 4);

    screen.feed(b"\x1b[20B"); // CUD beyond bottom
    assert_eq!(screen.cursor_state().row, 7);

    screen.feed(b"\x1b[20C"); // CUF beyond right
    assert_eq!(screen.cursor_state().column, 9);

    screen.feed(b"\x1b[20A"); // CUU beyond top
    assert_eq!(screen.cursor_state().row, 0);

    screen.feed(b"\x1b[20D"); // CUB beyond left
    assert_eq!(screen.cursor_state().column, 0);
}
