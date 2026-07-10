//! Regression tests for terminal client output frames behavior.

use crate::terminal::TerminalCursorStyle;
use crate::terminal::client_loop::{
    AttachedTerminalOutputModes, encode_attached_terminal_output_frame_with_keypad_transition,
    encode_attached_terminal_output_frame_with_styles,
};
use crate::terminal::screen::{GraphicRendition, TerminalColor, TerminalStyleSpan};

/// Verifies that attached-terminal frames suppress the host cursor, reset
/// coordinate-affecting terminal modes, enable host mouse reporting, clear
/// stale viewport cells, and restore a configured Mezzanine cursor at the
/// requested active-surface position.
#[test]
fn attached_terminal_output_frame_controls_cursor_presentation() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &[
            "pane    ".to_string(),
            "body    ".to_string(),
            "status  ".to_string(),
        ],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_style: TerminalCursorStyle::Underline,
            cursor_blink: false,
            cursor_visible: true,
            cursor_row: 2,
            cursor_column: 3,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(rendered.starts_with(
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[2J\x1b[H"
    ));
    assert!(rendered.contains("pane"));
    assert!(rendered.ends_with("\x1b[?25l\x1b[0m\x1b[4 q\x1b[3;4H\x1b[?25h"));
}

/// Verifies attached-terminal frames honor the configured host mouse policy.
///
/// Sessions with mouse disabled must not place the containing terminal in xterm
/// mouse modes, otherwise normal host selection and scrolling can be captured
/// even though Mezzanine mouse support is disabled.
#[test]
fn attached_terminal_output_frame_disables_host_mouse_reporting_when_configured() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["pane".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            host_mouse_reporting: false,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[?1000;1002;1006h"), "{rendered:?}");
    assert!(
        rendered.contains("\x1b[?1006l\x1b[?1002l\x1b[?1000l"),
        "{rendered:?}"
    );
}

/// Verifies attached-terminal frames keep the containing terminal on its
/// normal screen even when the pane-local screen model is in alternate screen.
/// Codex-like TUIs can enter DEC 1049 internally without capturing the mouse,
/// and mirroring that state to the host terminal would make ordinary host
/// scrollback unavailable despite Mezzanine retaining pane-local history and
/// copy-mode ownership.
#[test]
fn attached_terminal_output_frame_keeps_host_normal_screen_for_alternate_panes() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["fullscreen pane".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            alternate_screen: true,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[?1049h"), "{rendered:?}");
    assert!(rendered.contains("\x1b[?1049l"), "{rendered:?}");
    assert!(rendered.contains("fullscreen pane"), "{rendered:?}");
}

/// Verifies attached-terminal cursor presentation clamps to the rendered frame
/// bounds before emitting one-based terminal coordinates. A visible cursor at the
/// internal end-of-row insertion point must not become column `width + 1`, since
/// terminals can wrap or clamp that coordinate differently.
#[test]
fn attached_terminal_output_frame_clamps_visible_cursor_to_rendered_bounds() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["abcde".to_string(), "vwxyz".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            cursor_row: 9,
            cursor_column: 5,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(
        rendered.ends_with("\x1b[?25l\x1b[0m\x1b[2 q\x1b[2;5H\x1b[?25h"),
        "{rendered:?}"
    );
    assert!(!rendered.contains("\x1b[10;6H"), "{rendered:?}");
}

/// Verifies attached-terminal redraws place the cursor at the screen-model
/// insertion point even after high Private Use prompt glyphs. Font-specific
/// width guesses can put the visible cursor one column away from the next
/// echoed character, so presentation frames must not add a separate glyph-width
/// correction over the terminal screen cursor.
#[test]
fn attached_terminal_output_frame_uses_screen_cursor_after_patched_font_prompt_glyph() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["\u{f432}       ".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            cursor_row: 0,
            cursor_column: 1,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(
        rendered.ends_with("\x1b[?25l\x1b[0m\x1b[2 q\x1b[1;2H\x1b[?25h"),
        "{rendered:?}"
    );
}

/// Verifies that Mezzanine-owned cursor blink timing hides the cursor during
/// the off phase instead of relying on terminal-emulator blink rates.
#[test]
fn attached_terminal_output_frame_honors_cursor_blink_interval_phase() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["pane".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: true,
            cursor_blink_interval_ms: 500,
            cursor_blink_elapsed_ms: 250,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(rendered.ends_with("\x1b[?25l\x1b[0m"), "{rendered:?}");
}

/// Verifies that the presentation restore sequence disables Mezzanine-owned
/// mouse capture, resets coordinate-affecting terminal modes, clears
/// Mezzanine's drawn viewport, makes the host cursor visible again, and resets
/// cursor style after foreground detachment.
#[test]
fn attached_terminal_restore_frame_restores_cursor_visibility() {
    let restore = String::from_utf8(
        crate::terminal::client_loop::attached_terminal_restore_presentation_frame().to_vec(),
    )
    .unwrap();

    assert_eq!(
        restore,
        "\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b>\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[2J\x1b[H\x1b[?25h\x1b[0 q"
    );
}

/// Verifies that a full redraw does not append a style reset immediately after
/// a line whose final visible cell is still styled. Some host terminals keep
/// the last-column glyph pending until a following control arrives, so a
/// trailing SGR reset can make the final styled character render plain.
#[test]
fn attached_terminal_output_frame_avoids_trailing_reset_after_fully_styled_line() {
    let lines = vec!["blue".to_string()];
    let spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 4,
        rendition: GraphicRendition {
            foreground: Some(TerminalColor::Indexed(4)),
            ..GraphicRendition::default()
        },
    }]];

    let frame = encode_attached_terminal_output_frame_with_styles(
        &lines,
        &spans,
        None,
        AttachedTerminalOutputModes::default(),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(rendered.contains("\x1b[0;34mblue\x1b[?25l"), "{rendered:?}");
    assert!(
        !rendered.contains("\x1b[0;34mblue\x1b[0m\x1b[?25l"),
        "{rendered:?}"
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
            .starts_with(b"\x1b=\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[2J\x1b[H")
    );
    assert!(
        encode_attached_terminal_output_frame_with_keypad_transition(&lines, Some(false),)
            .starts_with(b"\x1b>\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[2J\x1b[H")
    );
    assert!(
        encode_attached_terminal_output_frame_with_keypad_transition(&lines, None).starts_with(
            b"\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[2J\x1b[H"
        )
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
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[2J\x1b[H\x1b[0;1;38;5;120mAB\x1b[0;4;7;38;2;1;2;3;44mC\x1b[0mD\x1b[?25l\x1b[0m"
    );
}
