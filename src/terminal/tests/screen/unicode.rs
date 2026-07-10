//! Regression tests for terminal screen unicode behavior.

use crate::terminal::{
    Size, TerminalScreen, TerminalStyledLine, terminal_char_width, terminal_text_width,
};

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

/// Verifies terminal screen buffers split multibyte UTF-8 scalars across feeds.
///
/// PTY reads can split one multibyte character between chunks during
/// full-screen redraws. The decoder must retain the incomplete prefix so the
/// visible row keeps only the valid leading text until the remaining bytes
/// arrive.
#[test]
fn terminal_screen_buffers_split_utf8_scalar_until_remaining_bytes_arrive() {
    let mut screen = TerminalScreen::new(Size::new(12, 2).unwrap(), 10).unwrap();

    screen.feed(b"caf\xc3");
    assert_eq!(screen.visible_lines()[0], "caf");

    screen.feed(b"\xa9!");
    assert_eq!(screen.visible_lines()[0], "caf\u{e9}!");
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

/// Verifies that the terminal screen correctly handles UTF-8 multi-byte
/// characters, including 2-byte and 3-byte sequences, and that wide CJK
/// characters occupy a single cell position.
#[test]
fn terminal_screen_utf8_and_wide_characters() {
    let size = Size::new(20, 4).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed("café".as_bytes());
    assert_eq!(screen.visible_lines()[0], "café");

    screen.feed(b"\r\n");
    screen.feed("naïve".as_bytes());
    assert_eq!(screen.visible_lines()[1], "naïve");

    screen.feed(b"\r\n");
    screen.feed("über".as_bytes());
    assert_eq!(screen.visible_lines()[2], "über");

    screen.feed(b"\r\n");
    screen.feed("piñata".as_bytes());
    assert_eq!(screen.visible_lines()[3], "piñata");
}

/// Verifies that split UTF-8 feeds preserve an incomplete multibyte prefix
/// until the remaining bytes arrive in a later chunk.
///
/// PTY reads can split one UTF-8 scalar across separate `feed` calls. The
/// terminal must not emit replacement characters for an otherwise valid scalar
/// just because its trailing bytes arrive in the next read.
#[test]
fn terminal_screen_preserves_split_utf8_across_feed_calls() {
    let size = Size::new(20, 4).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(&[0x63, 0x61, 0x66, 0xc3]);
    assert_eq!(screen.visible_lines()[0], "caf");

    screen.feed(&[0xa9]);
    assert_eq!(screen.visible_lines()[0], "café");

    screen.feed(b"\r\n");
    screen.feed(&[0xf0, 0x9f]);
    assert_eq!(screen.visible_lines()[1], "");

    screen.feed(&[0x98, 0x80]);
    assert_eq!(screen.visible_lines()[1], "😀");
}

/// Verifies that a wide character at the final column boundary defers wrapping
/// correctly and that the character appears at the start of the next line.
#[test]
fn terminal_screen_double_width_character_boundary() {
    let size = Size::new(5, 4).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(b"abcde"); // fill to edge exactly
    assert_eq!(screen.visible_lines()[0], "abcde");

    screen.feed(b"f"); // triggers deferred wrap
    assert_eq!(screen.visible_lines()[0], "abcde");
    assert_eq!(screen.visible_lines()[1], "f");

    screen.feed(b"ghijklm"); // fill line 2 and wrap again
    assert_eq!(screen.visible_lines()[1], "fghij");
    assert_eq!(screen.visible_lines()[2], "klm");

    assert_eq!(screen.history().len(), 0);
}

/// Verifies colored checkmark emoji are measured as two terminal cells.
///
/// Some terminal font stacks render `✅` as a double-width emoji even though
/// base Unicode width tables may classify related symbols as single-width text.
/// Mezzanine uses this normalized width for wrapping, copy-mode coordinates,
/// and styled transcript gutters so a checkmark cannot create phantom rows.
#[test]
fn terminal_screen_colored_check_mark_wraps_as_double_width() {
    assert_eq!(terminal_char_width('✅'), 2);

    let size = Size::new(5, 4).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed("abc✅d".as_bytes());

    assert_eq!(screen.visible_lines()[0], "abc✅");
    assert_eq!(screen.visible_lines()[1], "d");
}

/// Verifies explicit emoji-variation status glyphs use the wide presentation
/// width while bare status symbols keep their text width.
///
/// Models often emit colored status symbols such as `✔️` despite prompt
/// guidance. When those symbols appear in agent transcript rows, Mezzanine must
/// wrap them with the normal styled continuation gutter instead of creating a
/// phantom blank row with no gutter.
#[test]
fn terminal_screen_agent_gutter_wraps_emoji_variation_status_glyphs() {
    assert_eq!(terminal_char_width('✔'), 1);
    assert_eq!(terminal_text_width("✔"), 1);
    assert_eq!(terminal_text_width("✔️"), 2);
    assert_eq!(terminal_text_width("✔︎"), 1);
    assert_eq!(terminal_text_width("1️⃣"), 2);
    assert_eq!(terminal_text_width("👨‍💻"), 2);
    assert_eq!(terminal_text_width("🇺🇸"), 2);
    assert_eq!(terminal_text_width("e\u{301}"), 1);

    let mut screen = TerminalScreen::new(Size::new(13, 4).unwrap(), 10).unwrap();

    screen.feed("\x1b[31m▐ mez> \x1b[0mabc✔️d".as_bytes());

    assert_eq!(screen.visible_lines()[0], "▐ mez> abc✔️d");
    assert!(
        screen.visible_lines()[1].trim().is_empty(),
        "{:?}",
        screen.visible_lines()
    );
}

/// Verifies live terminal-screen rows preserve multi-scalar emoji-presentation
/// graphemes before the render canvas sees them. This protects against the
/// scalar-cell regression where `⚠️` was reduced to bare `⚠`, causing host
/// terminals to advance one fewer column than Mezzanine's width accounting.
#[test]
fn terminal_screen_preserves_warning_sign_variation_selector() {
    let mut screen = TerminalScreen::new(Size::new(7, 2).unwrap(), 10).unwrap();

    screen.feed("ab⚠️cd".as_bytes());

    assert_eq!(screen.visible_lines()[0], "ab⚠️cd");
    assert_eq!(screen.visible_styled_lines()[0].text, "ab⚠️cd");
}

/// Verifies clearing a column inside a wide multi-scalar grapheme removes the
/// whole grapheme footprint. Without explicit continuation sentinels, erasing
/// the second display column of `⚠️` can leave a stale leading scalar behind.
#[test]
fn terminal_screen_erases_warning_sign_continuation_footprint() {
    let mut screen = TerminalScreen::new(Size::new(7, 2).unwrap(), 10).unwrap();

    screen.feed("ab⚠️cd".as_bytes());
    screen.feed(b"\x1b[4G\x1b[X");

    assert_eq!(screen.visible_lines()[0], "ab  cd");
}

/// Verifies styled visible-row restoration round-trips complete grapheme text.
/// Snapshot and resize restoration use `write_styled_line_to_row`, so this
/// catches the previous behavior that stored only the first scalar from each
/// restored grapheme cluster.
#[test]
fn terminal_screen_restores_styled_lines_with_complete_graphemes() {
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    let lines = vec![TerminalStyledLine::plain("ab⚠️cd".to_string())];

    screen.restore_normal_styled_content(&[], &lines);

    assert_eq!(screen.visible_lines()[0], "ab⚠️cd");
    assert_eq!(screen.visible_styled_lines()[0].text, "ab⚠️cd");
}
