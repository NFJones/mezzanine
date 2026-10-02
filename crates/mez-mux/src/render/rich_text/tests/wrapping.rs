//! Wrapping source coordinates, labels, Unicode and copy-continuation regressions.
//!
//! Display-only indentation never shifts the authored source range or style
//! boundaries, including consumed whitespace and hard-split narrow rows.

use super::*;

/// Verifies width wrapping preserves the first source identity and marks
/// continuation rows for source-aware copy selection.
#[test]
fn rich_text_wrapping_marks_copy_continuations() {
    let line = RichTextLine {
        display: "prefix alpha beta gamma".to_string(),
        style_spans: Vec::new(),
        copy_text: Some("raw source".to_string()),
        kind: RichTextLineKind::Normal,
    };
    let wrapped = wrap_rich_text_line_to_width(line, 12);
    assert!(wrapped.len() > 1);
    assert_eq!(wrapped[0].copy_text.as_deref(), Some("raw source"));
    assert_eq!(
        wrapped[1].copy_text.as_deref(),
        Some(COPY_WRAP_CONTINUATION)
    );
}

/// Verifies consumed soft-wrap spaces advance source coordinates so
/// styles on later link-like labels remain aligned after repeated wraps.
#[test]
fn rich_text_wrapping_accounts_for_consumed_spaces_in_style_ranges() {
    let rendition = GraphicRendition {
        bold: true,
        underline: true,
        ..GraphicRendition::default()
    };
    let line = RichTextLine {
        display: "alpha beta gamma".to_string(),
        style_spans: vec![
            TerminalStyleSpan {
                start: 0,
                length: 5,
                rendition,
            },
            TerminalStyleSpan {
                start: 6,
                length: 4,
                rendition,
            },
            TerminalStyleSpan {
                start: 11,
                length: 5,
                rendition,
            },
        ],
        copy_text: None,
        kind: RichTextLineKind::Normal,
    };

    let wrapped = wrap_rich_text_line_to_width_with_source_ranges(line, 6);

    assert_eq!(
        wrapped
            .iter()
            .map(|row| row.line.display.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "gamma"]
    );
    assert_eq!(
        wrapped
            .iter()
            .map(|row| (row.source_start_column, row.source_end_column))
            .collect::<Vec<_>>(),
        [(0, 5), (6, 10), (11, 16)]
    );
    assert!(wrapped.iter().all(|row| {
        row.line.style_spans
            == vec![TerminalStyleSpan {
                start: 0,
                length: terminal_text_width(row.line.display.as_str()),
                rendition,
            }]
    }));
}

/// Verifies a long breakable Unicode line advances monotonically through
/// source columns while keeping every physical row within the target.
#[test]
fn rich_text_wrapping_handles_long_unicode_lines_incrementally() {
    let text = "alpha 中 e\u{301} ".repeat(2_000);
    let line = RichTextLine {
        display: text,
        style_spans: Vec::new(),
        copy_text: None,
        kind: RichTextLineKind::Normal,
    };

    let wrapped = wrap_rich_text_line_to_width_with_source_ranges(line, 12);

    assert!(wrapped.len() > 1_000, "{}", wrapped.len());
    assert!(wrapped.iter().all(|row| {
        terminal_text_width(row.line.display.as_str()) <= 12
            && row.source_start_column <= row.source_end_column
    }));
    assert!(
        wrapped
            .windows(2)
            .all(|rows| { rows[0].source_end_column <= rows[1].source_start_column })
    );
    assert!(wrapped.last().unwrap().source_end_column > 20_000);
}

/// Verifies fixed-width modal wrapping preserves an unbreakable token by
/// splitting it at grapheme boundaries instead of exposing it to the
/// compositor's defensive clipping path.
#[test]
fn rich_text_hard_wrapping_bounds_unbreakable_tokens() {
    let line = RichTextLine {
        display: "averyveryverylongtoken".to_string(),
        style_spans: Vec::new(),
        copy_text: Some("averyveryverylongtoken".to_string()),
        kind: RichTextLineKind::Normal,
    };

    let wrapped = wrap_rich_text_line_to_width_with_source_ranges_hard(line, 8);

    assert!(wrapped.len() > 1, "{wrapped:?}");
    assert!(
        wrapped
            .iter()
            .all(|line| terminal_text_width(line.line.display.as_str()) <= 8),
        "{wrapped:?}"
    );
}

/// Verifies a shorter continuation indent never turns the label separator
/// into a first-row break, and wrap-only padding is absent from source copy.
#[test]
fn agent_log_wrap_preserves_label_and_uses_five_space_continuations() {
    let line = RichTextLine {
        display: "agent: alpha beta gamma".to_string(),
        style_spans: Vec::new(),
        copy_text: Some("agent: alpha beta gamma".to_string()),
        kind: RichTextLineKind::Normal,
    };
    let wrapped = wrap_rich_text_line_to_width_with_source_ranges_hard(line, 12);
    assert_eq!(
        wrapped
            .iter()
            .map(|row| row.line.display.as_str())
            .collect::<Vec<_>>(),
        ["agent: alpha", "     beta", "     gamma"]
    );
    assert_eq!(
        wrapped[0].line.copy_text.as_deref(),
        Some("agent: alpha beta gamma")
    );
    assert_eq!(
        wrapped[1].line.copy_text.as_deref(),
        Some(COPY_WRAP_CONTINUATION)
    );

    let narrow = wrap_rich_text_line_to_width_with_source_ranges_hard(
        RichTextLine {
            display: "agent: abcdefghijk".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        },
        8,
    );
    assert_eq!(narrow[0].line.display, "agent: a");
    assert!(
        narrow
            .iter()
            .skip(1)
            .all(|row| row.line.display.starts_with("     "))
    );
    assert!(
        narrow
            .iter()
            .all(|row| terminal_text_width(&row.line.display) <= 8)
    );
}

/// Verifies a boundary-space skip retains original source positions and
/// moves a styled word after the display-only five-cell indent.
#[test]
fn agent_log_wrap_keeps_style_and_source_columns_after_boundary_space() {
    let rendition = GraphicRendition {
        bold: true,
        ..GraphicRendition::default()
    };
    let wrapped = wrap_rich_text_line_to_width_with_source_ranges_hard(
        RichTextLine {
            display: "agent: alpha beta".to_string(),
            style_spans: vec![TerminalStyleSpan {
                start: 13,
                length: 4,
                rendition,
            }],
            copy_text: Some("agent: alpha beta".to_string()),
            kind: RichTextLineKind::Normal,
        },
        12,
    );
    assert_eq!(wrapped[1].line.display, "     beta");
    assert_eq!(wrapped[1].source_start_column, 13);
    assert_eq!(wrapped[1].line.style_spans[0].start, 5);
    assert_eq!(wrapped[1].line.style_spans[0].length, 4);
    assert_eq!(
        wrapped[1].line.copy_text.as_deref(),
        Some(COPY_WRAP_CONTINUATION)
    );
}

/// Verifies source-less thinking rows keep their full first-row label
/// even when the shorter continuation could break at its separator.
#[test]
fn thinking_log_fallback_preserves_label_on_narrow_rows() {
    let wrapped = wrap_rich_text_line_to_width_with_source_ranges_hard(
        RichTextLine {
            display: "thinking: alpha beta".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        },
        12,
    );
    assert!(
        wrapped[0].line.display.starts_with("thinking: "),
        "{wrapped:?}"
    );
    assert!(
        wrapped
            .iter()
            .skip(1)
            .all(|row| row.line.display.starts_with("     "))
    );
    assert!(
        wrapped
            .iter()
            .all(|row| terminal_text_width(&row.line.display) <= 12)
    );
}
