//! Regression tests for terminal screen resize reflow behavior.

use crate::terminal::{Size, TerminalScreen, terminal_text_width};

/// Verifies alternate-screen resize keeps the live grid top-left anchored.
///
/// Normal-screen row shrink can preserve the bottom of overflowing content, but
/// alternate-screen applications own absolute viewport rows. Shrinking an
/// active alternate buffer must keep row zero visible and clip the bottom.
#[test]
fn terminal_screen_alternate_resize_keeps_top_left_anchor() {
    let mut screen = TerminalScreen::new(Size::new(6, 4).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?1049hrow0\r\nrow1\r\nrow2\r\nrow3");
    screen.resize(Size::new(6, 2).unwrap());

    assert!(screen.alternate_screen_active());
    assert_eq!(screen.size(), Size::new(6, 2).unwrap());
    assert_eq!(
        screen.visible_lines(),
        vec!["row0".to_string(), "row1".to_string()]
    );
    assert!(screen.history().is_empty());
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
