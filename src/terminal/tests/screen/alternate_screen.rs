//! Regression tests for terminal screen alternate screen behavior.

use crate::terminal::{Size, TerminalScreen};

/// Verifies repeated alternate-screen DECSET preserves the active alternate
/// buffer and cursor.
///
/// Applications can redundantly emit DEC 47, 1047, or 1049 while already in
/// alternate screen. Re-entering the mode should be idempotent instead of
/// clearing the live full-screen application contents or resetting its cursor.
#[test]
fn terminal_screen_repeated_alternate_screen_decset_is_idempotent() {
    for mode in [47, 1047, 1049] {
        let mut screen = TerminalScreen::new(Size::new(12, 3).unwrap(), 10).unwrap();
        let enter = format!("\x1b[?{mode}h");
        let leave = format!("\x1b[?{mode}l");

        screen.feed(b"normal");
        screen.feed(enter.as_bytes());
        screen.feed(b"\x1b[2;3Halt");
        let before_lines = screen.visible_lines();
        let before_cursor = screen.cursor_state();

        screen.feed(enter.as_bytes());

        assert!(screen.alternate_screen_active(), "mode {mode}");
        assert_eq!(screen.visible_lines(), before_lines, "mode {mode}");
        assert_eq!(screen.cursor_state(), before_cursor, "mode {mode}");

        screen.feed(leave.as_bytes());
        assert_eq!(screen.visible_lines()[0], "normal", "mode {mode}");
        assert!(!screen.alternate_screen_active(), "mode {mode}");
    }
}

/// Verifies alternate-screen exit restores normal DEC origin and scroll-region state.
///
/// Full-screen programs commonly rewrite DECSTBM and DECOM while they own the
/// alternate buffer. Leaving alternate screen must restore the normal screen
/// mode state as well as its cells; otherwise subsequent shell cursor-addressed
/// output can land on the wrong row and visually mix with stale fullscreen rows.
#[test]
fn terminal_screen_restores_origin_mode_and_scroll_region_after_alternate_screen_exit() {
    let mut screen = TerminalScreen::new(Size::new(8, 5).unwrap(), 10).unwrap();

    screen.feed(b"[2;4r[?6hN");
    screen.feed(b"[?1049h[r[?6lalt[?1049l");

    assert!(!screen.alternate_screen_active());
    assert!(screen.mode_state().origin_mode_enabled);

    screen.feed(b"[1;1HX");

    assert_eq!(
        screen.visible_lines(),
        vec![
            "".to_string(),
            "X".to_string(),
            "".to_string(),
            "".to_string(),
            "".to_string(),
        ]
    );
}

/// Verifies terminal screen restores normal-screen content and cursor after
/// alternate-screen exit.
///
/// Full-screen TUIs expect DECSET 1049 to preserve the underlying normal
/// buffer while drawing into a separate alternate screen. Leaving alternate
/// mode must restore that buffer and resume output at the saved cursor.
#[test]
fn terminal_screen_restores_normal_screen_after_alternate_screen_exit() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"keep");
    screen.feed(b"\x1b[?1049hsecret");

    assert!(screen.alternate_screen_active());
    assert_eq!(screen.visible_lines()[0], "secret");

    screen.feed(b"\x1b[?1049l!");

    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[0], "keep!");
    assert!(!screen.alternate_screen_active());
}

/// Verifies alternate-screen resize keeps the current pane geometry after exit.
///
/// Full-screen TUIs redraw against the resized pane. Leaving alternate screen
/// must keep that live geometry instead of restoring the size captured when the
/// alternate buffer was entered.
#[test]
fn terminal_screen_keeps_resized_geometry_after_alternate_screen_exit() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"keep");
    screen.feed(b"\x1b[?1049hsecret");
    screen.resize(Size::new(12, 3).unwrap());

    screen.feed(b"\x1b[?1049l!");

    assert!(!screen.alternate_screen_active());
    assert_eq!(screen.size(), Size::new(12, 3).unwrap());
    assert_eq!(screen.visible_lines()[0], "keep!");
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 5);
}

/// Verifies alternate-screen exit uses normal resize semantics for saved rows.
///
/// If a pane shrinks while a fullscreen alternate-screen app is active, the
/// saved normal screen must be restored through the same bottom-preserving
/// resize path as ordinary shell output. Copying the saved grid from the
/// top-left would drop the shell prompt tail and resume input on stale rows.
#[test]
fn terminal_screen_preserves_prompt_tail_after_alternate_screen_resize_shrink() {
    let mut screen = TerminalScreen::new(Size::new(10, 4).unwrap(), 10).unwrap();

    screen.feed(b"top\r\nmiddle\r\nprompt");
    screen.feed(b"\x1b[?1049hfullscreen");
    screen.resize(Size::new(10, 2).unwrap());

    screen.feed(b"\x1b[?1049l!");

    assert!(!screen.alternate_screen_active());
    assert_eq!(screen.size(), Size::new(10, 2).unwrap());
    let visible = screen.visible_lines();
    assert_eq!(visible[0], "middle");
    assert_eq!(visible[1], "prompt!");
    assert_eq!(screen.cursor_state().row, 1);
    assert_eq!(screen.cursor_state().column, 7);
}

/// Verifies DEC autowrap mode is saved and restored across alternate screen
/// enter/exit so that an alternate-screen program's DEC private mode 7
/// changes do not leak into the normal screen.
#[test]
fn terminal_screen_saves_and_restores_autowrap_across_alternate_screen() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    // Disable autowrap before entering alternate screen
    screen.feed(b"[?7l");
    assert!(!screen.mode_state().autowrap_enabled);

    // Enter alternate screen, enable autowrap, exit
    screen.feed(b"[?1049h");
    assert!(screen.alternate_screen_active());
    screen.feed(b"[?7h");
    assert!(screen.mode_state().autowrap_enabled);
    screen.feed(b"[?1049l");

    // Autowrap should be restored to disabled
    assert!(!screen.alternate_screen_active());
    assert!(!screen.mode_state().autowrap_enabled);
}

/// Verifies stray alternate-screen resets leave normal content intact.
///
/// Terminal applications can emit extra DEC alternate-screen reset sequences
/// while the pane is already using the normal screen. Those resets must not
/// erase normal-screen content or enter alternate mode.
#[test]
fn terminal_screen_preserves_normal_screen_on_stray_alternate_screen_reset() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"keep");
    screen.feed(b"[?1049l!");

    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[0], "keep!");
    assert!(!screen.alternate_screen_active());
}

/// Verifies DEC private mode 1048 saves and restores only the cursor.
///
/// Full-screen wrappers can use `CSI ?1048h` and `CSI ?1048l` separately from
/// alternate-buffer switching, so the mode must not enter alternate screen or
/// clear visible content while still restoring the saved cursor position.
#[test]
fn terminal_screen_dec1048_saves_cursor_without_switching_buffers() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"ab\x1b[?1048hcd\x1b[2;4HXY\x1b[?1048l!");

    assert!(!screen.alternate_screen_active());
    assert_eq!(screen.visible_lines()[0], "ab!d");
    assert_eq!(screen.visible_lines()[1], "   XY");
    assert!(screen.history().is_empty());
}

/// Verifies combined DEC private-mode sequences apply alternate-screen and cursor visibility together.
///
/// Full-screen TUIs can batch DEC private modes such as `CSI ?1049;25h` and
/// `CSI ?1049;25l` in one control sequence. The parser must apply every
/// parameter so combined mode updates enter the alternate buffer, restore the
/// normal screen on exit, and update cursor visibility without leaking
/// alternate content into scrollback.
#[test]
fn terminal_screen_combined_private_modes_enter_and_exit_alternate_screen() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"[?25lkeep");
    assert!(!screen.cursor_visible());

    screen.feed(b"[?1049;25hsecret");
    assert!(screen.alternate_screen_active());
    assert!(screen.cursor_visible());
    assert_eq!(screen.visible_lines()[0], "secret");

    screen.feed(b"[?1049;25l!");

    assert!(!screen.alternate_screen_active());
    assert!(!screen.cursor_visible());
    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[0], "keep!");
}
