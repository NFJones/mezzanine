//! Regression tests for terminal screen wrapping behavior.

use crate::{TerminalScreen, TerminalSize as Size};

/// Verifies that terminal autowrap is deferred after writing the last column.
/// Real terminals keep the cursor visually on the bottom-right cell until the
/// next printable character arrives; this keeps echoed prompt input visible on
/// the bottom row and only scrolls when more output actually needs space.
#[test]
fn terminal_screen_defers_autowrap_until_next_printable_cell() {
    let mut screen = TerminalScreen::new(Size::new(4, 2).unwrap(), 10).unwrap();

    screen.feed(b"abcd");
    assert_eq!(screen.visible_lines(), vec!["abcd", ""]);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        Vec::<&str>::new()
    );

    screen.feed(b"e");
    assert_eq!(screen.visible_lines(), vec!["abcd", "e"]);

    screen.feed(b"fghijk");
    assert_eq!(screen.history().lines().collect::<Vec<_>>(), vec!["abcd"]);
    assert_eq!(screen.visible_lines(), vec!["efgh", "ijk"]);
}

/// Verifies that cursor-neutral SGR styling does not cancel a deferred wrap.
/// Pagers commonly reset or change rendition after filling the final column,
/// before emitting the printable glyph that should begin the next row.
#[test]
fn terminal_screen_preserves_deferred_autowrap_across_sgr() {
    let mut screen = TerminalScreen::new(Size::new(4, 2).unwrap(), 10).unwrap();

    screen.feed(b"abcd\x1b[31me");

    assert_eq!(screen.visible_lines(), vec!["abcd", "e"]);
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 1);
}

/// Verifies the pager-style reset, space, backspace, and styled-text sequence
/// wraps before the temporary space instead of overwriting the final cell.
#[test]
fn terminal_screen_preserves_pager_wrap_sequence_across_sgr() {
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();

    screen.feed(b"12345678901234567890\x1b[m \x08\x1b[31mABCDEFGHIJ");

    assert_eq!(
        screen.visible_lines(),
        vec!["12345678901234567890", "ABCDEFGHIJ"]
    );
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 10);
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

/// Verifies that cursor movement across wrap boundaries preserves the
/// correct visible position. After leaving and returning to a line, the
/// cursor should point at the expected column for the next operation.
#[test]
fn terminal_screen_cursor_returns_across_wrap_boundary() {
    let mut screen = TerminalScreen::new(Size::new(5, 2).unwrap(), 10).unwrap();

    screen.feed(b"abcde");
    screen.feed(b"fgh");

    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 3);

    screen.feed(b"\x1b[A");
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 3);
}

/// Verifies the screen model follows bash/readline's real wrapped-line
/// backspace sequence. Readline crosses a wrap boundary with carriage returns,
/// cursor-up, cursor-right, and erase-line operations, so a simplified
/// backspace-only regression can miss stale wrapped characters.
#[test]
fn terminal_screen_handles_bash_wrapped_backspace_sequence() {
    let mut screen = TerminalScreen::new(Size::new(10, 4).unwrap(), 10).unwrap();

    screen.feed(b"$ abcdefghijk");
    assert_eq!(screen.visible_lines()[0], "$ abcdefgh");
    assert_eq!(screen.visible_lines()[1], "ijk");

    screen.feed(
        b"\x08\x1b[K\x08\x1b[K\r\x1b[K\x1b[A\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[K\
          \r\n\r\x1b[K\x1b[A\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\
          \x08\x1b[K\x08\x1b[K",
    );

    assert_eq!(screen.visible_lines()[0], "$ abcde");
    assert_eq!(screen.visible_lines()[1], "");
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 7);
}

/// Verifies bash/readline wrap-boundary deletion with the prompt shape used by
/// the foreground PTY reproduction. The important part is that the `CR LF CR`,
/// cursor-up, and cursor-right sequence returns to the previous visual row
/// instead of drifting downward.
#[test]
fn terminal_screen_handles_bash_prompt_glyph_wrap_boundary_delete() {
    let mut screen = TerminalScreen::new(Size::new(20, 6).unwrap(), 10).unwrap();

    screen.feed("\u{f432} abcdefghijklmnopqrstu".as_bytes());
    assert_eq!(screen.visible_lines()[0], "\u{f432} abcdefghijklmnopqr");
    assert_eq!(screen.visible_lines()[1], "stu");

    screen.feed(
        b"\x08\x1b[K\x08\x1b[K\r\x1b[K\x1b[A\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[K\
          \r\n\r\x1b[K\x1b[A\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\
          \x08\x1b[K\x08\x1b[K\x08\x1b[K\x08\x1b[K",
    );

    assert_eq!(screen.visible_lines()[0], "\u{f432} abcdefghijklm");
    assert_eq!(screen.visible_lines()[1], "");
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 15);
}

/// Verifies the same readline wrap-boundary delete sequence when the prompt is
/// already near the bottom of the pane. With `TERM=screen-256color`, readline
/// uses ESC M (Reverse Index) instead of CSI A to move back to the previous
/// visual row, so the emulator must treat it as vertical cursor movement.
#[test]
fn terminal_screen_handles_bash_wrap_boundary_delete_below_top_row() {
    let mut screen = TerminalScreen::new(Size::new(20, 6).unwrap(), 10).unwrap();

    screen.feed(b"\n\n\n\n");
    screen.feed("\u{f432} abcdefghijklmnopqrstu".as_bytes());
    assert_eq!(screen.visible_lines()[4], "\u{f432} abcdefghijklmnopqr");
    assert_eq!(screen.visible_lines()[5], "stu");

    screen.feed(
        b"\x08\x1b[K\x08\x1b[K\r\x1b[K\x1bM\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[K\
          \r\n\r\x1b[K\x1bM\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\
          \x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\
          \x08\x1b[K\x08\x1b[K\x08\x1b[K\x08\x1b[K",
    );

    assert_eq!(screen.visible_lines()[4], "\u{f432} abcdefghijklm");
    assert_eq!(screen.visible_lines()[5], "");
    assert_eq!(screen.cursor_state().row, 4);
    assert_eq!(screen.cursor_state().column, 15);
}

/// Verifies DEC autowrap mode can be disabled with `CSI ?7l`. Full-screen
/// TUIs use this mode when drawing at the right margin, so the pane model must
/// not defer a wrap or scroll after a final-column write while DECAWM is off.
#[test]
fn terminal_screen_decawm_disabled_keeps_printing_at_right_margin() {
    let size = Size::new(5, 2).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(b"\x1b[?7l");
    screen.feed(b"abcde");
    screen.feed(b"f");

    assert_eq!(screen.visible_lines()[0], "abcdf");
    assert_eq!(screen.visible_lines()[1], "");
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 4);
    assert_eq!(screen.history().len(), 0);
}

/// Verifies double-width glyphs that cannot fit at the right edge are clipped
/// when DEC autowrap is disabled.
///
/// A leading cell containing a wide glyph without its continuation would make
/// the row wider than the pane and can bleed into dividers or neighboring
/// panes. DECAWM-off printing should still leave a valid cell footprint.
#[test]
fn terminal_screen_decawm_disabled_clips_right_edge_wide_glyph() {
    let size = Size::new(5, 2).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(b"\x1b[?7l");
    screen.feed("abcd✅".as_bytes());

    assert_eq!(screen.visible_lines()[0], "abcd");
    assert_eq!(screen.visible_lines()[1], "");
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 4);
    assert_eq!(screen.history().len(), 0);
}

/// Verifies DEC autowrap mode can be re-enabled with `CSI ?7h`. TUIs may
/// toggle DECAWM around status-line and lower-right-corner updates, so the
/// deferred wrap behavior must resume after the mode is restored.
#[test]
fn terminal_screen_decawm_reenabled_restores_deferred_wrap() {
    let size = Size::new(5, 2).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(b"\x1b[?7l");
    screen.feed(b"\x1b[?7h");
    screen.feed(b"abcde");
    screen.feed(b"f");

    assert_eq!(screen.visible_lines()[0], "abcde");
    assert_eq!(screen.visible_lines()[1], "f");
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 1);
}
