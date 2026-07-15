//! Regression tests for terminal presentation copy mode behavior.

use crate::terminal::CopyMode;
use mez_mux::copy::{CopyPosition, SearchDirection};
use mez_mux::layout::Size;
use mez_mux::paste::PasteBuffers;
use mez_terminal::TerminalColor;
use mez_terminal::TerminalScreen;

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
    let lines = ["▐ mez> ## Heading", "▐        - item", "▐            code"];
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
        mez_mux::MuxErrorKind::InvalidArgs
    );
    assert_eq!(
        buffers.set("main", "12345").unwrap_err().kind(),
        mez_mux::MuxErrorKind::InvalidArgs
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
