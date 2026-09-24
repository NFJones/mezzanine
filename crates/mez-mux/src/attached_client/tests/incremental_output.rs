//! Regression tests for terminal client incremental output behavior.

use crate::attached_client::output::{
    AttachedTerminalModeTransitions, AttachedTerminalOutputFrameState,
    compose_terminal_output_style_spans, encode_attached_terminal_output_frame_with_styles,
    encode_attached_terminal_output_update_frame_with_styles,
    encode_attached_terminal_output_update_frame_with_verified_size,
};
use crate::presentation::{
    AttachedTerminalOutputModes, ClientViewRole, RenderedClientView, TerminalCursorStyle,
};

use crate::copy::CopyPosition;
use crate::layout::Size;
use crate::theme::UiTheme;
use mez_terminal::TerminalScreen;
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};
use unicode_width::UnicodeWidthStr;

/// Returns the display column where one text fragment begins.
fn display_column_for_fragment(line: &str, needle: &str) -> usize {
    let byte_index = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing from {line:?}"));
    UnicodeWidthStr::width(&line[..byte_index])
}

/// Returns the rendition active at one displayed terminal column.
fn styled_line_rendition_at(
    line: &mez_terminal::TerminalStyledLine,
    column: usize,
) -> GraphicRendition {
    line.style_spans
        .iter()
        .rev()
        .find(|span| column >= span.start && column < span.start.saturating_add(span.length))
        .map(|span| span.rendition)
        .unwrap_or_default()
}

/// Adjacent syntax colors sharing a background use local SGR changes without
/// re-emitting the background, while the modeled cells retain exact styling.
#[test]
fn attached_terminal_output_shortens_shared_background_color_transitions() {
    let background = Some(TerminalColor::Rgb(4, 5, 6));
    let spans = vec![vec![
        TerminalStyleSpan {
            start: 0,
            length: 1,
            rendition: GraphicRendition {
                foreground: Some(TerminalColor::Indexed(1)),
                background,
                ..GraphicRendition::default()
            },
        },
        TerminalStyleSpan {
            start: 1,
            length: 1,
            rendition: GraphicRendition {
                foreground: Some(TerminalColor::Indexed(4)),
                background,
                ..GraphicRendition::default()
            },
        },
        TerminalStyleSpan {
            start: 2,
            length: 1,
            rendition: GraphicRendition {
                background,
                ..GraphicRendition::default()
            },
        },
    ]];
    let lines = vec!["ABC".to_string()];
    let frame = encode_attached_terminal_output_frame_with_styles(
        &lines,
        &spans,
        None,
        AttachedTerminalOutputModes::default(),
    );
    let output = String::from_utf8(frame.clone()).unwrap();
    assert!(output.contains("\u{1b}[34mB\u{1b}[39mC"), "{output:?}");
    assert_eq!(output.matches("48;2;4;5;6").count(), 1, "{output:?}");
    let conservative = output.replace(
        "\u{1b}[34mB\u{1b}[39mC",
        "\u{1b}[0;34;48;2;4;5;6mB\u{1b}[0;48;2;4;5;6mC",
    );
    assert!(
        output.len() < conservative.len(),
        "optimized {} vs conservative {} bytes",
        output.len(),
        conservative.len()
    );
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    screen.feed(&frame);
    let row = &screen.visible_styled_lines()[0];
    assert_eq!(row.text.trim_end(), "ABC");
    for (column, foreground) in [
        Some(TerminalColor::Indexed(1)),
        Some(TerminalColor::Indexed(4)),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            styled_line_rendition_at(row, column),
            GraphicRendition {
                foreground,
                background,
                ..GraphicRendition::default()
            }
        );
    }
}

/// Verifies that stable-size attached-terminal redraws are encoded as row
/// updates instead of clearing the full viewport. This reduces foreground TTY
/// flicker while still allowing the first draw and resizes to invalidate the
/// whole surface. Changed rows are already full-width, so the update must not
/// append erase-to-end-of-line after the row text because that can clear a
/// freshly drawn final-column cell while host autowrap is pending.
#[test]
fn attached_terminal_output_update_redraws_only_changed_rows() {
    let previous_lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["one    ".to_string(), "changed".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.starts_with("\x1b[?25l"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2;1H\x1b[0mchanged"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[K"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1Hone"), "{rendered:?}");
}

/// Reuses exact shifted full-width rows instead of repainting the viewport;
/// the modeled terminal must agree with an independently drawn target frame.
#[test]
fn attached_terminal_output_reuses_exact_shifted_rows() {
    let columns = 40;
    let rows = 12;
    let previous_lines = (0..rows)
        .map(|index| {
            format!(
                "row {index:02} {}",
                char::from(b'a' + index as u8)
                    .to_string()
                    .repeat(columns - 7)
            )
        })
        .collect::<Vec<_>>();
    let mut next_lines = previous_lines[1..].to_vec();
    next_lines.push(format!("row {rows:02} {}", "y".repeat(columns - 7)));
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&previous_lines, &[], modes);
    let initial =
        encode_attached_terminal_output_frame_with_styles(&previous_lines, &[], None, modes);
    let update = encode_attached_terminal_output_update_frame_with_verified_size(
        &next_lines,
        &[],
        None,
        modes,
        Some(&previous),
        AttachedTerminalModeTransitions::default(),
        Some(Size::new(columns as u16, rows as u16).unwrap()),
    );
    assert!(
        update.windows(4).any(|bytes| bytes == b"\x1b[1M"),
        "expected a line-delete candidate: {} bytes",
        update.len()
    );
    assert!(
        update.len() < initial.len() / 2,
        "scroll update {} vs full {}",
        update.len(),
        initial.len()
    );
    let mut actual =
        TerminalScreen::new(Size::new(columns as u16, rows as u16).unwrap(), 20).unwrap();
    let mut expected =
        TerminalScreen::new(Size::new(columns as u16, rows as u16).unwrap(), 20).unwrap();
    actual.feed(&initial);
    actual.feed(&update);
    expected.feed(&encode_attached_terminal_output_frame_with_styles(
        &next_lines,
        &[],
        None,
        modes,
    ));
    assert_eq!(
        actual.visible_styled_lines(),
        expected.visible_styled_lines()
    );
    assert_eq!(actual.history().len(), expected.history().len());
}

/// A two-row reverse shift must preserve unchanged chrome and physical rows
/// below the rendered region, including their effective styles.
#[test]
fn attached_terminal_output_shift_down_keeps_chrome_and_lower_rows() {
    let columns = 40;
    let previous_lines = (0..10)
        .map(|index| {
            format!(
                "row {index:02} {}",
                char::from(b'a' + index as u8).to_string().repeat(33)
            )
        })
        .collect::<Vec<_>>();
    let mut next_lines = previous_lines.clone();
    next_lines[2..9].clone_from_slice(&previous_lines[0..7]);
    next_lines[0] = format!("new 00 {}", "z".repeat(33));
    next_lines[1] = format!("new 01 {}", "y".repeat(33));
    next_lines[9] = previous_lines[9].clone();
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        ..Default::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&previous_lines, &[], modes);
    let initial =
        encode_attached_terminal_output_frame_with_styles(&previous_lines, &[], None, modes);
    let update = encode_attached_terminal_output_update_frame_with_verified_size(
        &next_lines,
        &[],
        None,
        modes,
        Some(&previous),
        AttachedTerminalModeTransitions::default(),
        Some(Size::new(columns, 12).unwrap()),
    );
    assert!(
        update.windows(4).any(|bytes| bytes == b"\x1b[2L"),
        "expected scoped insert-line reuse: {} bytes",
        update.len()
    );
    let mut actual = TerminalScreen::new(Size::new(columns, 12).unwrap(), 20).unwrap();
    let mut expected = TerminalScreen::new(Size::new(columns, 12).unwrap(), 20).unwrap();
    actual.feed(&initial);
    expected.feed(&initial);
    for screen in [&mut actual, &mut expected] {
        screen.feed(b"\x1b[12;1Hlower row");
    }
    actual.feed(&update);
    // Paint only changed presentation rows in the oracle; a full redraw would
    // erase the host-owned row below the presented region.
    for (row, line) in next_lines.iter().enumerate().take(9) {
        expected.feed(format!("\x1b[{};1H\x1b[0m\x1b[2K{}", row + 1, line).as_bytes());
    }
    assert_eq!(
        actual.visible_styled_lines(),
        expected.visible_styled_lines()
    );
    assert_eq!(actual.history().len(), expected.history().len());
}

/// Shifted styled rows retain their renditions, while the newly exposed row
/// is repainted safely even when it contains a wide glyph.
#[test]
fn attached_terminal_output_shift_preserves_styles_and_wide_cells() {
    let columns = 40;
    let previous_lines = (0..10)
        .map(|index| {
            format!(
                "row {index:02} {}",
                char::from(b'a' + index as u8).to_string().repeat(33)
            )
        })
        .collect::<Vec<_>>();
    let rendition = GraphicRendition {
        foreground: Some(TerminalColor::Rgb(100, 120, 140)),
        ..Default::default()
    };
    let previous_spans = vec![
        vec![TerminalStyleSpan {
            start: 0,
            length: 6,
            rendition
        }];
        10
    ];
    let mut next_lines = previous_lines[1..].to_vec();
    next_lines.push(format!("wide 界 {}", "z".repeat(32)));
    let mut next_spans = previous_spans[1..].to_vec();
    next_spans.push(vec![TerminalStyleSpan {
        start: 0,
        length: 7,
        rendition,
    }]);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        ..Default::default()
    };
    let previous =
        AttachedTerminalOutputFrameState::new_with_modes(&previous_lines, &previous_spans, modes);
    let initial = encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &previous_spans,
        None,
        modes,
    );
    let update = encode_attached_terminal_output_update_frame_with_verified_size(
        &next_lines,
        &next_spans,
        None,
        modes,
        Some(&previous),
        AttachedTerminalModeTransitions::default(),
        Some(Size::new(columns, 10).unwrap()),
    );
    assert!(update.windows(4).any(|bytes| bytes == b"\x1b[1M"));
    let mut actual = TerminalScreen::new(Size::new(columns, 10).unwrap(), 20).unwrap();
    let mut expected = TerminalScreen::new(Size::new(columns, 10).unwrap(), 20).unwrap();
    actual.feed(&initial);
    actual.feed(&update);
    expected.feed(&encode_attached_terminal_output_frame_with_styles(
        &next_lines,
        &next_spans,
        None,
        modes,
    ));
    assert_eq!(
        actual.visible_styled_lines(),
        expected.visible_styled_lines()
    );
    assert_eq!(actual.history().len(), expected.history().len());
}

/// Line edits affect physical terminal rows, so missing geometry or any
/// partial-width presentation must retain ordinary row-diff encoding.
#[test]
fn attached_terminal_output_shift_requires_verified_full_width() {
    let lines = (0..12)
        .map(|index| format!("row {index:02} {}", "x".repeat(33)))
        .collect::<Vec<_>>();
    let mut shifted = lines[1..].to_vec();
    shifted.push(format!("row 12 {}", "y".repeat(33)));
    let modes = AttachedTerminalOutputModes::default();
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], modes);
    for verified_size in [None, Some(Size::new(41, 12).unwrap())] {
        let encoded = encode_attached_terminal_output_update_frame_with_verified_size(
            &shifted,
            &[],
            None,
            modes,
            Some(&previous),
            AttachedTerminalModeTransitions::default(),
            verified_size,
        );
        assert!(!encoded.windows(4).any(|bytes| bytes == b"\x1b[1M"));
    }
}

/// Verifies an authoritative style-only command-preview update preserves the
/// existing wrapped text rows instead of clearing either the display or row.
///
/// Streamed command completion can retain identical glyphs and wrapping while
/// applying final syntax styling. The differential encoder must update those
/// styles in place so completion does not visibly erase and re-emit the command.
#[test]
fn attached_terminal_output_update_restyles_command_without_clear() {
    let lines = vec!["$ printf 'alpha beta'".to_string()];
    let previous_spans = vec![Vec::new()];
    let command_rendition = GraphicRendition {
        foreground: Some(TerminalColor::Rgb(122, 168, 159)),
        bold: true,
        ..GraphicRendition::default()
    };
    let current_spans = vec![vec![TerminalStyleSpan {
        start: 2,
        length: "printf".len(),
        rendition: command_rendition,
    }]];
    let previous = AttachedTerminalOutputFrameState::new(&lines, &previous_spans);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..AttachedTerminalOutputModes::default()
    };
    let initial_frame =
        encode_attached_terminal_output_frame_with_styles(&lines, &previous_spans, None, modes);
    let update_frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &current_spans,
        None,
        modes,
        Some(&previous),
    );
    let encoded = String::from_utf8(update_frame.clone()).unwrap();

    assert!(!encoded.contains("\x1b[2J"), "{encoded:?}");
    assert!(!encoded.contains("\x1b[2K"), "{encoded:?}");

    let mut screen = TerminalScreen::new(Size::new(40, 1).unwrap(), 10).unwrap();
    screen.feed(&initial_frame);
    screen.feed(&update_frame);
    let line = &screen.visible_styled_lines()[0];
    assert_eq!(line.text.trim_end(), lines[0]);
    assert_eq!(
        styled_line_rendition_at(line, 2),
        command_rendition,
        "style-only update did not settle on the retained command row: {line:?}"
    );
}

/// Verifies the first live command-output text and later appended text are
/// physically emitted with the same dim status rendition. The incremental
/// writer must not let newly changed cells inherit the terminal default while
/// a full-row status span remains logically unchanged.
#[test]
fn attached_terminal_output_updates_keep_streaming_status_style_on_every_chunk() {
    let status_rendition = GraphicRendition {
        foreground: Some(TerminalColor::Rgb(118, 126, 140)),
        dim: true,
        ..GraphicRendition::default()
    };
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..AttachedTerminalOutputModes::default()
    };
    let first_lines = vec!["first output        ".to_string()];
    let later_lines = vec!["first output second ".to_string()];
    let spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 20,
        rendition: status_rendition,
    }]];

    let first_frame =
        encode_attached_terminal_output_frame_with_styles(&first_lines, &spans, None, modes);
    let previous = AttachedTerminalOutputFrameState::new(&first_lines, &spans);
    let later_frame = encode_attached_terminal_output_update_frame_with_styles(
        &later_lines,
        &spans,
        None,
        modes,
        Some(&previous),
    );

    let mut screen = TerminalScreen::new(Size::new(20, 1).unwrap(), 10).unwrap();
    screen.feed(&first_frame);
    let first = &screen.visible_styled_lines()[0];
    assert_eq!(styled_line_rendition_at(first, 0), status_rendition);

    screen.feed(&later_frame);
    let later = &screen.visible_styled_lines()[0];
    assert_eq!(later.text, later_lines[0]);
    assert_eq!(styled_line_rendition_at(later, 0), status_rendition);
    assert_eq!(
        styled_line_rendition_at(later, display_column_for_fragment(&later.text, "second")),
        status_rendition,
        "newly appended output inherited a non-status rendition: {later:?}"
    );
}

/// Verifies that same-width printable ASCII row changes can update only the
/// changed span instead of rewriting the whole row. This keeps frequent status
/// or prompt edits small on slower terminal links while preserving the existing
/// row-diff contract for unsafe text.
#[test]
fn attached_terminal_output_update_uses_changed_ascii_span_when_safe() {
    let previous_lines = vec!["aaaaaaaaaa".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["aaaabaaaaa".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;5H\x1b[0mb"), "{rendered:?}");
    assert!(!rendered.contains("aaaabaaaaa"), "{rendered:?}");
}

/// One changed cell under a full-width style must not resend its unchanged
/// neighbors, while preserving the effective rendition of the changed cell.
#[test]
fn attached_terminal_output_update_limits_full_span_damage_to_changed_cell() {
    let previous_lines = vec!["a".repeat(80)];
    let current_lines = vec![format!("{}b{}", "a".repeat(39), "a".repeat(40))];
    let rendition = GraphicRendition {
        background: Some(TerminalColor::Rgb(12, 34, 56)),
        ..GraphicRendition::default()
    };
    let spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 80,
        rendition,
    }]];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &spans);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..Default::default()
    };
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &spans,
        None,
        modes,
        Some(&previous),
    );
    let encoded = String::from_utf8(frame).unwrap();
    assert!(encoded.contains("\x1b[1;40H"), "{encoded:?}");
    assert!(
        !encoded.contains(&current_lines[0]),
        "unchanged cells were repainted: {encoded:?}"
    );
    let mut screen = TerminalScreen::new(Size::new(81, 1).unwrap(), 10).unwrap();
    screen.feed(&encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &spans,
        None,
        modes,
    ));
    screen.feed(encoded.as_bytes());
    let row = &screen.visible_styled_lines()[0];
    assert_eq!(&row.text[..80], current_lines[0]);
    assert_eq!(styled_line_rendition_at(row, 0), rendition);
    assert_eq!(styled_line_rendition_at(row, 39), rendition);
    assert_eq!(styled_line_rendition_at(row, 79), rendition);
}

/// Different span representations of the same effective terminal cells must
/// produce no redundant row output.
#[test]
fn attached_terminal_output_update_ignores_equivalent_style_spans() {
    let lines = vec!["same row".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&lines, &[Vec::new()]);
    let equivalent = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 8,
        rendition: GraphicRendition::default(),
    }]];
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &equivalent,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..Default::default()
        },
        Some(&previous),
    );
    assert!(
        frame.is_empty(),
        "equivalent spans repainted a row: {:?}",
        String::from_utf8_lossy(&frame)
    );

    let rendition = GraphicRendition {
        foreground: Some(TerminalColor::Rgb(12, 34, 56)),
        ..GraphicRendition::default()
    };
    let whole = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 8,
        rendition,
    }]];
    let split = vec![vec![
        TerminalStyleSpan {
            start: 0,
            length: 3,
            rendition,
        },
        TerminalStyleSpan {
            start: 3,
            length: 5,
            rendition,
        },
    ]];
    let previous = AttachedTerminalOutputFrameState::new(&lines, &whole);
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &split,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..Default::default()
        },
        Some(&previous),
    );
    assert!(
        frame.is_empty(),
        "partitioned spans repainted a row: {:?}",
        String::from_utf8_lossy(&frame)
    );
}

/// Verifies title-like frame edits can update several changed rows through
/// bounded row segments instead of repainting each full row. Window-group,
/// window, and pane title changes commonly touch a small set of frame rows, and
/// the default window bar includes non-ASCII action glyphs that previously
/// forced a full-row rewrite even when the changed title segment itself was
/// narrow.
#[test]
fn attached_terminal_output_update_uses_segment_updates_for_small_multi_row_title_changes() {
    let previous_lines = vec![
        "0 shell □ ⊕ λ".to_string(),
        "1 default".to_string(),
        "#1 shell".to_string(),
    ];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &[
            "0 build □ ⊕ λ".to_string(),
            "1 staging".to_string(),
            "#1 build".to_string(),
        ],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;3H\x1b[0mbuild"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2;3H\x1b[0mstaging"), "{rendered:?}");
    assert!(rendered.contains("\x1b[3;4H\x1b[0mbuild"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1H0 build"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[2;1H1 staging"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[3;1H#1 build"), "{rendered:?}");
}

/// Verifies multiple changed rows choose the smaller safe encoding per row,
/// rather than imposing a global changed-row threshold.
#[test]
fn attached_terminal_output_update_rewrites_full_rows_for_many_row_changes() {
    let previous_lines = vec![
        "row 001".to_string(),
        "row 002".to_string(),
        "row 003".to_string(),
        "row 004".to_string(),
    ];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let current_lines = vec![
        "row 101".to_string(),
        "row 102".to_string(),
        "row 103".to_string(),
        "row 104".to_string(),
    ];
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..AttachedTerminalOutputModes::default()
    };
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &[],
        None,
        modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame.clone()).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    for row in 1..=4 {
        assert!(
            rendered.contains(&format!("\x1b[{row};5H\x1b[0m1")),
            "{rendered:?}"
        );
    }
    let mut screen = TerminalScreen::new(Size::new(12, 4).unwrap(), 10).unwrap();
    screen.feed(&encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &[],
        None,
        modes,
    ));
    screen.feed(&frame);
    for (row, expected) in screen.visible_styled_lines().iter().zip(&current_lines) {
        assert_eq!(&row.text[..7], expected);
    }
}

/// Four independent one-cell edits must not repaint every unchanged cell merely
/// because they occur in more than three rows.
#[test]
fn attached_terminal_output_update_uses_sparse_segments_across_four_rows() {
    let previous_lines = (0..4).map(|_| "a".repeat(80)).collect::<Vec<_>>();
    let current_lines = (0..4)
        .map(|_| format!("{}b{}", "a".repeat(39), "a".repeat(40)))
        .collect::<Vec<_>>();
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..Default::default()
    };
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &[],
        None,
        modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame.clone()).unwrap();
    assert!(
        frame.len() < 180,
        "four sparse edits repainted rows: {rendered:?}"
    );
    for row in 1..=4 {
        assert!(
            rendered.contains(&format!("\x1b[{row};40H\x1b[0mb")),
            "{rendered:?}"
        );
    }
    let mut screen = TerminalScreen::new(Size::new(81, 4).unwrap(), 10).unwrap();
    screen.feed(&encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &[],
        None,
        modes,
    ));
    screen.feed(&frame);
    for (row, expected) in screen.visible_styled_lines().iter().zip(&current_lines) {
        assert_eq!(&row.text[..80], expected);
    }
}

/// Distant one-cell changes should not transmit the unchanged interior when
/// two independent cursor-addressed runs are cheaper than one bounding span.
#[test]
fn attached_terminal_output_update_uses_two_distant_changed_runs() {
    let previous_lines = vec!["a".repeat(160)];
    let mut current = previous_lines[0].clone().into_bytes();
    current[2] = b'b';
    current[157] = b'c';
    let current_lines = vec![String::from_utf8(current).unwrap()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..Default::default()
    };
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &[],
        None,
        modes,
        Some(&previous),
    );
    let encoded = String::from_utf8(frame.clone()).unwrap();
    assert!(encoded.contains("\x1b[1;3H\x1b[0mb"), "{encoded:?}");
    assert!(encoded.contains("\x1b[1;158H\x1b[0mc"), "{encoded:?}");
    assert!(
        frame.len() < 150,
        "unchanged middle was repainted: {encoded:?}"
    );
    let mut screen = TerminalScreen::new(Size::new(161, 1).unwrap(), 10).unwrap();
    screen.feed(&encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &[],
        None,
        modes,
    ));
    screen.feed(&frame);
    assert_eq!(
        &screen.visible_styled_lines()[0].text[..160],
        current_lines[0]
    );
}

/// Many alternating changes stay bounded by choosing one contiguous row
/// candidate instead of generating an unbounded sequence of cursor moves.
#[test]
fn attached_terminal_output_update_bounds_dense_run_planning() {
    let previous_lines = vec!["a".repeat(80)];
    let current_lines = vec!["ba".repeat(40)];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..Default::default()
    };
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &[],
        None,
        modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame.clone()).unwrap();
    assert!(
        frame.len() < 160,
        "dense update generated too many runs: {rendered:?}"
    );
    let mut screen = TerminalScreen::new(Size::new(81, 1).unwrap(), 10).unwrap();
    screen.feed(&encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &[],
        None,
        modes,
    ));
    screen.feed(&frame);
    assert_eq!(
        &screen.visible_styled_lines()[0].text[..80],
        current_lines[0]
    );
}

/// Verifies rows that change display width still keep the full-row rewrite
/// path. Cursor-column updates can only target a bounded segment when the old
/// and new text occupy the same visible columns.
#[test]
fn attached_terminal_output_update_rewrites_rows_when_glyph_width_changes() {
    let previous_lines = vec!["aa✔aa".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["aaXaa ".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H\x1b[0m"), "{rendered:?}");
    assert!(rendered.contains("aaXaa "), "{rendered:?}");
}

/// Verifies a changed prompt retains the full-row background on trailing
/// padding without unnecessarily repainting the unchanged styled cells.
///
/// Pasting multiline input replaces visible text while the same prompt style
/// remains active on the trailing spaces. The physical result must match a
/// fresh styled render even when only the changed prefix is emitted.
#[test]
fn attached_terminal_output_update_repaints_trailing_prompt_padding_after_text_change() {
    let previous_lines = vec!["      ".to_string()];
    let current_lines = vec!["alpha ".to_string()];
    let prompt_span = mez_terminal::TerminalStyleSpan {
        start: 0,
        length: 6,
        rendition: mez_terminal::GraphicRendition {
            foreground: Some(mez_terminal::TerminalColor::Rgb(255, 255, 255)),
            background: Some(mez_terminal::TerminalColor::Rgb(37, 40, 39)),
            ..mez_terminal::GraphicRendition::default()
        },
    };
    let previous_spans = vec![vec![prompt_span]];
    let current_spans = vec![vec![prompt_span]];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &current_spans,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H\x1b[0m"), "{rendered:?}");
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..Default::default()
    };
    let mut screen = TerminalScreen::new(Size::new(16, 1).unwrap(), 10).unwrap();
    screen.feed(&encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &previous_spans,
        None,
        modes,
    ));
    screen.feed(rendered.as_bytes());
    let row = &screen.visible_styled_lines()[0];
    assert_eq!(&row.text[..6], "alpha ");
    assert_eq!(styled_line_rendition_at(row, 5), prompt_span.rendition);
}

/// Verifies wrapped agent-prompt continuation rows keep their unchanged
/// indentation and padding with the original style after a bounded update.
///
/// History navigation can swap one wrapped continuation row for another while
/// keeping the changed text inside a narrow interior segment. Compare actual
/// terminal cells and styles instead of requiring an obsolete full-row repaint.
#[test]
fn attached_terminal_output_update_rewrites_fully_styled_prompt_continuation_rows() {
    let previous_lines = vec!["      alpha     ".to_string()];
    let current_lines = vec!["      omega     ".to_string()];
    let prompt_span = mez_terminal::TerminalStyleSpan {
        start: 0,
        length: 16,
        rendition: mez_terminal::GraphicRendition {
            foreground: Some(mez_terminal::TerminalColor::Rgb(255, 255, 255)),
            background: Some(mez_terminal::TerminalColor::Rgb(37, 40, 39)),
            ..mez_terminal::GraphicRendition::default()
        },
    };
    let previous_spans = vec![vec![prompt_span]];
    let current_spans = vec![vec![prompt_span]];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &current_spans,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;7H\x1b[0m"), "{rendered:?}");
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..Default::default()
    };
    let mut screen = TerminalScreen::new(Size::new(20, 1).unwrap(), 10).unwrap();
    screen.feed(&encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &previous_spans,
        None,
        modes,
    ));
    screen.feed(rendered.as_bytes());
    let row = &screen.visible_styled_lines()[0];
    assert_eq!(&row.text[..16], "      omega     ");
    assert_eq!(styled_line_rendition_at(row, 0), prompt_span.rendition);
    assert_eq!(styled_line_rendition_at(row, 15), prompt_span.rendition);
}

/// Verifies bounded row-segment updates keep selected-link styling off the
/// separator cell on a `/resume` picker row.
///
/// The live pager moves selection between saved-session rows without a full
/// redraw. When that happens, the row-differential encoder must preserve the
/// link foreground, underline, and active background on the first session-id
/// cell without shifting any of that styling one column left into the bullet
/// separator.
#[test]
fn attached_terminal_output_update_preserves_resume_picker_link_boundary() {
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    let link_rendition = GraphicRendition {
        foreground: Some(TerminalColor::Rgb(230, 195, 132)),
        bold: true,
        underline: true,
        ..GraphicRendition::default()
    };
    let active_link_rendition = GraphicRendition {
        background: Some(TerminalColor::Rgb(122, 168, 159)),
        ..link_rendition
    };
    let previous_lines = vec!["> • latest".to_string(), format!("  • {session_id}")];
    let current_lines = vec!["  • latest".to_string(), format!("> • {session_id}")];
    let previous_spans = vec![
        vec![
            TerminalStyleSpan {
                start: 0,
                length: 2,
                rendition: GraphicRendition::default(),
            },
            TerminalStyleSpan {
                start: 4,
                length: "latest".len(),
                rendition: active_link_rendition,
            },
        ],
        vec![TerminalStyleSpan {
            start: 4,
            length: session_id.len(),
            rendition: link_rendition,
        }],
    ];
    let current_spans = vec![
        vec![TerminalStyleSpan {
            start: 4,
            length: "latest".len(),
            rendition: link_rendition,
        }],
        vec![
            TerminalStyleSpan {
                start: 0,
                length: 2,
                rendition: GraphicRendition::default(),
            },
            TerminalStyleSpan {
                start: 4,
                length: session_id.len(),
                rendition: active_link_rendition,
            },
        ],
    ];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);
    let modes = AttachedTerminalOutputModes {
        cursor_visible: false,
        cursor_blink: false,
        ..AttachedTerminalOutputModes::default()
    };
    let initial_frame = encode_attached_terminal_output_frame_with_styles(
        &previous_lines,
        &previous_spans,
        None,
        modes,
    );
    let update_frame = encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &current_spans,
        None,
        modes,
        Some(&previous),
    );
    let mut screen = TerminalScreen::new(Size::new(120, 2).unwrap(), 10).unwrap();
    screen.feed(&initial_frame);
    screen.feed(&update_frame);

    let styled_lines = screen.visible_styled_lines();
    let row = styled_lines
        .iter()
        .find(|line| line.text.contains(session_id))
        .unwrap();
    let start = display_column_for_fragment(&row.text, session_id);
    let previous_rendition = styled_line_rendition_at(row, start.saturating_sub(1));
    let first_rendition = styled_line_rendition_at(row, start);

    assert_ne!(
        previous_rendition.foreground,
        Some(TerminalColor::Rgb(230, 195, 132)),
        "resume picker link foreground shifted left after segment update: {styled_lines:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left after segment update: {styled_lines:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(TerminalColor::Rgb(122, 168, 159)),
        "resume picker active background shifted left after segment update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(TerminalColor::Rgb(230, 195, 132)),
        "resume picker first session-id cell lost link foreground after segment update: {styled_lines:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline after segment update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(TerminalColor::Rgb(122, 168, 159)),
        "resume picker first session-id cell lost active background after segment update: {styled_lines:?}"
    );
}

/// Verifies stable-row attached-terminal updates clear only rows that shrink
/// instead of falling back to a full-screen redraw. This avoids stale trailing
/// cells over remote terminal links while keeping the update bounded to the
/// changed row.
#[test]
fn attached_terminal_output_update_clears_shrinking_rows_without_full_redraw() {
    let previous_lines = vec!["wide text".to_string(), "steady".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["short".to_string(), "steady".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(
        rendered.contains("\x1b[1;1H\x1b[0m\x1b[2Kshort"),
        "{rendered:?}"
    );
    assert!(!rendered.contains("\x1b[2;1Hsteady"), "{rendered:?}");
}

/// Verifies stable-size attached-terminal updates avoid sending any bytes when
/// the rendered rows, style spans, bracketed-paste mode, and cursor
/// presentation are unchanged. This keeps idle status refreshes cheap over
/// higher-latency terminal links.
#[test]
fn attached_terminal_output_update_omits_unchanged_frame_bytes() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: false,
        cursor_row: 0,
        cursor_column: 0,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], modes);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        modes,
        Some(&previous),
    );

    assert!(frame.is_empty(), "{:?}", String::from_utf8_lossy(&frame));
}

/// Verifies pane-local alternate-screen metadata changes do not invalidate the
/// attached host's retained frame. Focus can switch between panes with
/// different buffer modes without changing the host terminal's normal-screen
/// presentation; unchanged cells need no output and changed/shrinking rows
/// remain safely differential in both directions.
#[test]
fn attached_terminal_output_update_diffs_across_alternate_screen_metadata_changes() {
    let prior_lines = vec!["one    ".to_string(), "two    ".to_string()];
    let next_lines = vec!["one".to_string(), "changed".to_string()];

    for (previous_alternate, next_alternate) in [(true, false), (false, true)] {
        let previous_modes = AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            alternate_screen: previous_alternate,
            ..AttachedTerminalOutputModes::default()
        };
        let previous =
            AttachedTerminalOutputFrameState::new_with_modes(&prior_lines, &[], previous_modes);
        let next_modes = AttachedTerminalOutputModes {
            alternate_screen: next_alternate,
            ..previous_modes
        };

        let unchanged = encode_attached_terminal_output_update_frame_with_styles(
            &prior_lines,
            &[],
            None,
            next_modes,
            Some(&previous),
        );
        assert!(
            unchanged.is_empty(),
            "{previous_alternate}->{next_alternate}: {:?}",
            String::from_utf8_lossy(&unchanged)
        );

        let changed = encode_attached_terminal_output_update_frame_with_styles(
            &next_lines,
            &[],
            None,
            next_modes,
            Some(&previous),
        );
        let rendered = String::from_utf8(changed).unwrap();
        assert!(
            !rendered.contains("\x1b[2J"),
            "{previous_alternate}->{next_alternate}: {rendered:?}"
        );
        assert!(
            rendered.contains("\x1b[1;1H\x1b[0m\x1b[2Kone"),
            "{previous_alternate}->{next_alternate}: {rendered:?}"
        );
        assert!(
            rendered.contains("changed"),
            "{previous_alternate}->{next_alternate}: {rendered:?}"
        );
        assert!(
            !rendered.contains("\x1b[?1049h"),
            "{previous_alternate}->{next_alternate}: {rendered:?}"
        );
    }
}

/// Verifies stable-size attached-terminal updates emit only cursor bytes when
/// the visible content is unchanged and the cursor moves. Row-differential
/// updates should resend coordinate-state presentation setup before cursor
/// addressing, but not clear or repaint static content.
#[test]
fn attached_terminal_output_update_uses_cursor_only_frame_for_cursor_moves() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous_modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: false,
        cursor_row: 0,
        cursor_column: 0,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], previous_modes);
    let next_modes = AttachedTerminalOutputModes {
        cursor_column: 1,
        ..previous_modes
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        next_modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[?2004"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1Hone"), "{rendered:?}");
    assert_eq!(
        rendered,
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?25l\x1b[0m\x1b[2 q\x1b[1;2H\x1b[?25h"
    );
}

/// Verifies a cursor-only blink-off update preserves the physical cursor
/// position while hiding it. The presentation prologue resets DEC scroll
/// margins, which also homes the terminal cursor, so the hidden-cursor suffix
/// must restore the modeled location instead of leaving the host at column zero.
#[test]
fn attached_terminal_output_blink_off_update_preserves_cursor_position() {
    let lines = vec!["parent$         ".to_string()];
    let visible_modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        cursor_blink_elapsed_ms: 0,
        cursor_row: 0,
        cursor_column: 8,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], visible_modes);
    let hidden_modes = AttachedTerminalOutputModes {
        cursor_blink_elapsed_ms: 250,
        ..visible_modes
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        hidden_modes,
        Some(&previous),
    );
    let mut screen = TerminalScreen::new(Size::new(16, 2).unwrap(), 10).unwrap();
    screen.feed(b"parent$ ");
    screen.feed(&frame);

    assert!(!screen.cursor_visible());
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 8);
}

/// Verifies stable-size attached-terminal updates emit bracketed-paste mode
/// changes without resending the rest of the static presentation prologue.
#[test]
fn attached_terminal_output_update_emits_only_changed_bracketed_paste_mode() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous = AttachedTerminalOutputFrameState::new_with_modes(
        &lines,
        &[],
        AttachedTerminalOutputModes::default(),
    );
    let next_modes = AttachedTerminalOutputModes {
        bracketed_paste: true,
        ..AttachedTerminalOutputModes::default()
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        next_modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert_eq!(rendered, "\x1b[?2004h");
}

/// Repainting a changed row must assign changed mouse and focus modes once,
/// after the mandatory coordinate reset, without redundant pre-reset writes.
#[test]
fn attached_terminal_repaint_sets_mouse_and_focus_once() {
    let prior = vec!["before  ".to_string()];
    let previous = AttachedTerminalOutputFrameState::new_with_modes(
        &prior,
        &[],
        AttachedTerminalOutputModes::default(),
    );
    let next = vec!["after   ".to_string()];
    let modes = AttachedTerminalOutputModes {
        host_mouse_reporting: false,
        focus_events: true,
        ..AttachedTerminalOutputModes::default()
    };
    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &next,
        &[],
        None,
        modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();
    assert!(rendered.contains("\x1b[?6l\x1b[?69l\x1b[r"), "{rendered:?}");
    assert_eq!(
        rendered
            .matches("\x1b[?1006l\x1b[?1002l\x1b[?1000l")
            .count(),
        1
    );
    assert_eq!(rendered.matches("\x1b[?1004h").count(), 1);
}

/// Verifies that attached-terminal row-diff updates keep styling on the final
/// changed cell when a full-word style span reaches the row end. Segment-only
/// updates are the most likely place for an off-by-one to leave the trailing
/// glyph plain even though the line-level span length is correct.
#[test]
fn attached_terminal_output_update_keeps_style_on_final_changed_character() {
    let previous_lines = vec!["gone".to_string()];
    let previous_spans = vec![Vec::new()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &previous_spans);
    let spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 4,
        rendition: GraphicRendition {
            foreground: Some(TerminalColor::Indexed(4)),
            ..GraphicRendition::default()
        },
    }]];

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["blue".to_string()],
        &spans,
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(
        rendered.contains("\x1b[1;1H\x1b[0m\x1b[0;34mblue"),
        "{rendered:?}"
    );
    assert!(
        !rendered.contains("\x1b[1;1H\x1b[0m\x1b[0;34mblue\x1b[0m"),
        "{rendered:?}"
    );
}

/// Verifies terminal output styling drops styles for matching rendered slices.
///
/// Partial text matches are not a safe ownership proof because unrelated action
/// output can match rows already visible in the rendered presentation. This
/// regression keeps render-owned styles from leaking onto bounded writes.
#[test]
fn terminal_output_style_spans_drop_styles_for_matching_row_slice() {
    let rendered_view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(16, 3).unwrap(),
        client_size: Size::new(16, 3).unwrap(),
        lines: vec![
            "alpha".to_string(),
            "beta match".to_string(),
            "gamma".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        selection: Some((
            CopyPosition { line: 1, column: 5 },
            CopyPosition {
                line: 1,
                column: 10,
            },
        )),
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        focus_events: false,
        alternate_screen: false,
        host_mouse_reporting: true,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
        readline_input_active: false,
    };
    let output_lines = vec!["beta match".to_string()];
    let style_spans =
        compose_terminal_output_style_spans(&output_lines, Some(&(rendered_view, None)));
    assert!(style_spans.is_empty(), "{style_spans:?}");
}
