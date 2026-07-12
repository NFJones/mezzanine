//! Regression tests for terminal screen history behavior.

use crate::{
    AlternateScreenState, DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, GraphicRendition,
    HistoryBuffer, TerminalColor, TerminalScreen, TerminalSize as Size, TerminalStyleSpan,
    TerminalStyledLine,
};

/// Verifies history buffer evicts oldest lines first.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn history_buffer_evicts_oldest_lines_first() {
    let mut history = HistoryBuffer::new(2).unwrap();

    history.push_styled_line(TerminalStyledLine::plain("one"));
    history.push_styled_line(TerminalStyledLine::plain("two"));
    history.push_styled_line(TerminalStyledLine::plain("three"));

    assert_eq!(history.lines().collect::<Vec<_>>(), vec!["two", "three"]);
}

/// Verifies history buffer relimits and evicts oldest lines.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn history_buffer_relimits_and_evicts_oldest_lines() {
    let mut history = HistoryBuffer::new(4).unwrap();

    history.push_styled_line(TerminalStyledLine::plain("one"));
    history.push_styled_line(TerminalStyledLine::plain("two"));
    history.push_styled_line(TerminalStyledLine::plain("three"));
    history.push_styled_line(TerminalStyledLine::plain("four"));
    history.set_limit(2).unwrap();

    assert_eq!(history.limit(), 2);
    assert_eq!(history.lines().collect::<Vec<_>>(), vec!["three", "four"]);
    assert!(HistoryBuffer::new(1).unwrap().set_limit(0).is_err());
}

/// Verifies history buffer rotates oldest lines in configurable batches.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn history_buffer_rotates_oldest_lines_in_configured_batches() {
    let mut history = HistoryBuffer::new_with_rotation(5, 2).unwrap();

    for line in ["one", "two", "three", "four", "five", "six"] {
        history.push_styled_line(TerminalStyledLine::plain(line));
    }

    assert_eq!(history.limit(), 5);
    assert_eq!(history.rotate_lines(), 2);
    assert_eq!(
        history.lines().collect::<Vec<_>>(),
        vec!["three", "four", "five", "six"]
    );
    history.push_styled_line(TerminalStyledLine::plain("seven"));
    assert_eq!(
        history.lines().collect::<Vec<_>>(),
        vec!["three", "four", "five", "six", "seven"]
    );
    assert!(HistoryBuffer::new_with_rotation(2, 0).is_err());
}

/// Verifies terminal screen relimits history buffer.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_relimits_history_buffer() {
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 4).unwrap();
    screen.restore_normal_styled_content(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &[],
    );

    screen.set_history_limit(2).unwrap();

    assert_eq!(screen.history_limit(), 2);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["two", "three"]
    );
}

/// Verifies screen history configuration failures retain terminal-owned diagnostics.
///
/// This regression protects the compatibility boundary so screen construction
/// and live history updates do not depend on the product error aggregate.
#[test]
fn terminal_screen_reports_terminal_owned_history_configuration_errors() {
    let size = Size::new(8, 2).unwrap();
    let constructor_error = TerminalScreen::new(size, 0).unwrap_err();
    assert_eq!(
        constructor_error.message(),
        "history buffer limit must be greater than zero"
    );

    let mut screen = TerminalScreen::new(size, 4).unwrap();
    let update_error = screen.set_history_rotate_lines(0).unwrap_err();
    assert_eq!(
        update_error.message(),
        "history buffer rotation line count must be greater than zero"
    );
}

/// Verifies default history limit matches spec.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_history_limit_matches_spec() {
    let history = HistoryBuffer::default_limit();

    assert_eq!(history.limit(), DEFAULT_HISTORY_LIMIT);
    assert_eq!(history.rotate_lines(), DEFAULT_HISTORY_ROTATE_LINES);
}

/// Verifies alternate screen is not history recordable.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn alternate_screen_is_not_history_recordable() {
    let mut state = AlternateScreenState::new();
    assert!(state.should_record_to_history());

    state.enter();

    assert!(!state.should_record_to_history());
}

/// Verifies terminal screen scrolls normal output into history.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_scrolls_normal_output_into_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[4;38;5;42mone\x1b[0m\ntwo\nthree");

    assert_eq!(screen.history().lines().collect::<Vec<_>>(), vec!["one"]);
    let styled_history = screen.history().styled_lines().collect::<Vec<_>>();
    assert_eq!(styled_history[0].text, "one");
    assert_eq!(
        styled_history[0].style_spans,
        vec![TerminalStyleSpan {
            start: 0,
            length: 3,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: true,
                inverse: false,
                foreground: Some(TerminalColor::Indexed(42)),
                background: None,
            }
        }]
    );
    assert_eq!(screen.visible_lines()[1], "three");
}

/// Verifies terminal screen excludes alternate-screen scroll-off rows from history.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_excludes_alternate_screen_scroll_off_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?1049halt\ninside\nmore\x1b[?1049lback");

    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[0], "back");
    assert!(!screen.alternate_screen_active());
}

/// Verifies alternate-screen redraws do not pollute history.
///
/// This regression scenario covers true TUI-style painting where an
/// alternate-screen application repeatedly updates visible cells without
/// scrolling the full pane. Only rows that actually scroll off the top should
/// become pane-local history.
#[test]
fn terminal_screen_excludes_alternate_screen_redraws_from_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?1049hfirst\x1b[Hsecond\x1b[?1049lback");

    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[0], "back");
    assert!(!screen.alternate_screen_active());
}

/// Verifies DEC 47 and 1047 alternate-screen entry points isolate normal
/// history just like DEC 1049.
///
/// Some applications use the older 47/1047 private modes rather than 1049.
/// They still switch into the pane-local alternate buffer, so their visible
/// content and scroll-off rows must not leak into normal scrollback history.
#[test]
fn terminal_screen_dec47_and_dec1047_alternate_screen_do_not_record_history() {
    for mode in [47, 1047] {
        let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();
        let enter = format!("\x1b[?{mode}h");
        let leave = format!("\x1b[?{mode}l");

        screen.feed(b"normal");
        screen.feed(enter.as_bytes());
        screen.feed(b"alt\ninside\nmore");
        assert!(screen.alternate_screen_active());

        screen.feed(leave.as_bytes());
        screen.feed(b"back");

        assert!(screen.history().is_empty(), "mode {mode}");
        assert_eq!(screen.visible_lines()[0], "normalback", "mode {mode}");
        assert!(!screen.alternate_screen_active(), "mode {mode}");
    }
}

/// Verifies alternate-screen scrolling inside DECSTBM margins and DECOM origin
/// mode remains isolated from normal history.
///
/// Full-screen TUIs commonly combine alternate screen, scroll regions, origin
/// mode, and line movement while repainting panes. Even when rows move within
/// the alternate buffer, those rows are alternate-screen content and must not
/// become normal scrollback history after exit.
#[test]
fn terminal_screen_alternate_scroll_region_origin_mode_excludes_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 4).unwrap(), 10).unwrap();

    screen.feed(b"shell");
    screen.feed(b"\x1b[?1049h\x1b[2;4r\x1b[?6h\x1b[1;1Htop");
    screen.feed(b"\x1b[3;1Hone\n two\n three");
    screen.feed(b"\x1b[?6l\x1b[r\x1b[?1049l!");

    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[0], "shell!");
    assert!(!screen.alternate_screen_active());
}

/// Verifies UI clears preserve pane logs by scrolling visible rows into history.
///
/// Agent-mode entry, exit, and prompt `Ctrl+L` use this path so the pane can
/// look freshly cleared without deleting content from copyable scrollback.
#[test]
fn terminal_screen_clear_visible_into_history_preserves_log_rows() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[31mred\x1b[0m\nmiddle\nbottom");
    screen.clear_visible_into_history();

    assert_eq!(screen.visible_lines(), vec!["", "", ""]);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["red", "middle", "bottom"]
    );
    let styled_history = screen.history().styled_lines().collect::<Vec<_>>();
    assert_eq!(
        styled_history[0].style_spans[0].rendition.foreground,
        Some(TerminalColor::Indexed(1))
    );
}

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

/// Verifies that copy-text annotations on rows dropped during a top-anchored
/// shrink are committed to scrollback history so they remain recoverable.
#[test]
fn terminal_screen_resize_shrink_preserves_dropped_row_copy_text_in_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 5).unwrap(), 10).unwrap();
    screen.restore_normal_styled_content(
        &[],
        &[
            TerminalStyledLine::plain("line0"),
            TerminalStyledLine::plain("line1"),
            TerminalStyledLine::plain("line2"),
            TerminalStyledLine::plain("line3"),
            TerminalStyledLine::plain("line4"),
        ],
    );
    screen.set_recent_normal_copy_texts(
        &[
            "copy-zero".to_string(),
            "copy-one".to_string(),
            "copy-two".to_string(),
            "copy-three".to_string(),
            "copy-four".to_string(),
        ],
        "",
    );

    screen.resize(Size::new(10, 3).unwrap());

    // Dropped rows 0 and 1 must land in history, preserving copy-text when present.
    let history_styled: Vec<_> = screen.history().styled_lines().collect();
    assert_eq!(history_styled.len(), 2);
    assert_eq!(history_styled[0].text, "line0");
    assert_eq!(history_styled[0].copy_text.as_deref(), Some("copy-zero"));
    assert_eq!(history_styled[1].text, "line1");
    assert_eq!(history_styled[1].copy_text.as_deref(), Some("copy-one"));
    assert_eq!(screen.visible_lines(), vec!["line2", "line3", "line4"]);
}
