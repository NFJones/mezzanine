//! Regression tests for terminal screen styling behavior.

use crate::terminal::screen::{GraphicRendition, TerminalColor, TerminalStyleSpan};
use crate::terminal::{Size, TerminalScreen, TerminalStyledLine};

/// Verifies that snapshot resume can rebuild a visible terminal row with SGR
/// spans even though the original PTY byte stream is no longer available.
#[test]
fn terminal_screen_restores_styled_visible_snapshot_content() {
    let mut screen = TerminalScreen::new(Size::new(12, 2).unwrap(), 4).unwrap();
    let rendition = GraphicRendition {
        bold: true,
        dim: false,
        italic: false,
        strikethrough: false,
        double_underline: false,
        hidden: false,
        underline: false,
        inverse: false,
        foreground: Some(TerminalColor::Rgb(1, 2, 3)),
        background: Some(TerminalColor::Indexed(4)),
    };

    screen.restore_normal_styled_content(
        &["history".to_string()],
        &[TerminalStyledLine {
            text: "styled".to_string(),
            style_spans: vec![TerminalStyleSpan {
                start: 0,
                length: 6,
                rendition,
            }],
            copy_text: None,
        }],
    );

    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["history"]
    );
    assert_eq!(screen.visible_lines()[0], "styled");
    assert_eq!(
        screen.visible_styled_lines()[0].style_spans,
        vec![TerminalStyleSpan {
            start: 0,
            length: 6,
            rendition
        }]
    );
}

/// Verifies G0 DEC Special Graphics designation renders box-drawing glyphs.
///
/// Ncurses ACS output commonly designates `ESC ( 0` and then emits ASCII box
/// drawing bytes in GL. This regression guards that the terminal screen maps
/// those bytes into Unicode box-drawing characters instead of leaving the raw
/// ASCII source visible.
#[test]
fn terminal_screen_renders_dec_special_graphics_from_g0_charset() {
    let mut screen = TerminalScreen::new(Size::new(6, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b(0lqk");

    assert_eq!(screen.visible_lines()[0], "┌─┐");
}

/// Verifies saved parser state restores an invoked G1 DEC Special Graphics
/// charset across snapshot resume.
///
/// Full-screen applications may designate `ESC ) 0`, invoke it with SO, and
/// then rely on later output after snapshot restore to keep rendering ACS line
/// drawing until SI switches GL back to G0. This regression guards both the
/// SO/SI control handling and saved-state persistence for the active charset
/// selection.
#[test]
fn terminal_screen_restores_invoked_g1_dec_special_graphics_from_saved_state() {
    let mut source = TerminalScreen::new(Size::new(6, 2).unwrap(), 10).unwrap();
    source.feed(b"\x1b)0\x0e");
    let saved_state = source.saved_state();

    let mut restored = TerminalScreen::new(Size::new(6, 2).unwrap(), 10).unwrap();
    restored.restore_saved_state(&saved_state);
    restored.feed(b"x\x0fq");

    assert_eq!(restored.visible_lines()[0], "│q");
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
    screen.feed(b"\x1b[38;2;1;2;3;48;5;42mY");
    assert_eq!(screen.visible_lines()[0], "XY");

    screen.feed(b"\x1b[22;24;39;49mZ");
    assert_eq!(screen.visible_lines()[0], "XYZ");
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
