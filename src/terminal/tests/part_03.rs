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

/// Verifies that after text wraps and bash sends its backspace erasure
/// sequences, the rendered output reflects the erased characters. This
/// exercises the full screen-update + render path.
#[test]
fn render_output_reflects_wrapped_text_erasure() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    let mut screen = TerminalScreen::new(Size::new(10, 1).unwrap(), 10).unwrap();
    screen.feed(b"hello");
    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);

    let config = TerminalClientLoopConfig {
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
    assert_eq!(view.lines, vec!["hello     ", "          ", "          "]);

    let screen = screens.get_mut(window.active_pane().id.as_str()).unwrap();
    screen.feed(b"\x08 \x08");
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        view.lines,
        vec!["hell      ", "          ", "          "],
        "backspace+space should erase last char"
    );
}

/// Verifies rendering after backspace erases a wrapped character via
/// explicit CSI sequences (cursor back, delete char).
#[test]
fn render_output_reflects_wrapped_csi_erasure() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(5, 3).unwrap());
    let mut screen = TerminalScreen::new(Size::new(5, 3).unwrap(), 10).unwrap();
    screen.feed(b"abcde");
    screen.feed(b"f");
    assert_eq!(screen.visible_lines()[0], "abcde");
    assert!(screen.visible_lines()[1].starts_with('f'));

    screen.feed(b"\x1b[D\x1b[P");
    assert!(
        screen.visible_lines()[1].is_empty(),
        "row 1 should be empty after DCH: {:?}",
        screen.visible_lines()
    );

    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);
    let config = TerminalClientLoopConfig {
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
    let joined = view.lines.join("\n");
    assert!(
        !joined.contains('f'),
        "erased 'f' should not appear in rendered output:\n{joined}"
    );
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

/// Verifies that OSC 0 and OSC 2 title-setting sequences update the terminal
/// title to the specified value and that empty titles fall back to the default.
#[test]
fn terminal_screen_osc_title_setting() {
    let size = Size::new(10, 4).unwrap();
    let mut screen = TerminalScreen::new(size, 100).unwrap();

    screen.feed(b"\x1b]0;project\x07");
    assert_eq!(screen.title(), Some("project"));

    screen.feed(b"\x1b]2;build\x1b\\");
    assert_eq!(screen.title(), Some("build"));

    screen.feed(b"\x1b]0;\x07");
    assert_eq!(screen.title(), Some("")); // empty title stored as-is

    screen.feed(b"\x1b]2;project-name\x1b\\");
    assert_eq!(screen.title(), Some("project-name"));
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
