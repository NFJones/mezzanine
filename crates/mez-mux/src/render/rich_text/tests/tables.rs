//! Table layout, inline style continuity and authored-source spacing regressions.
//!
//! Narrow stacked fallback must retain every column; physical continuations stay
//! adjacent to their source row without consuming the following blank line.

use super::*;

/// Literal table geometry must retain only first-cell value ranges in box and
/// stacked layouts, including hard-wrapped UUIDs. Metadata is literal text and
/// cannot introduce Markdown structure or steal another row's identity.
#[test]
fn literal_table_ranges_preserve_wrapped_row_identity() {
    let ids = [
        "11111111-1111-1111-1111-111111111111",
        "22222222-2222-2222-2222-222222222222",
    ];
    for width in [29, 28, 12] {
        let layout = render_literal_table(
            vec![
                "ID".to_string(),
                "Title".to_string(),
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
                "E".to_string(),
            ],
            ids.iter()
                .map(|id| {
                    vec![
                        id.to_string(),
                        "[inert](mez-agent:evil) │ value".to_string(),
                    ]
                })
                .collect(),
            width,
            &theme(),
        );
        for (row, id) in ids.iter().enumerate() {
            let fragments = layout
                .first_cells
                .iter()
                .filter(|range| range.row == row)
                .collect::<Vec<_>>();
            assert!(!fragments.is_empty(), "width={width}, row={row}");
            let mut value = String::new();
            for range in fragments {
                assert!(range.start + range.width <= width);
                let line = &layout.lines[range.line].display;
                let mut column = 0;
                for grapheme in
                    unicode_segmentation::UnicodeSegmentation::graphemes(line.as_str(), true)
                {
                    let next = column + terminal_grapheme_width(grapheme);
                    if column >= range.start && next <= range.start + range.width {
                        value.push_str(grapheme);
                    }
                    column = next;
                }
            }
            assert_eq!(&value, id, "width={width}, row={row}");
        }
    }
}

/// Verifies CommonMark tables become structural rows and retain source
/// metadata without depending on product transcript types.
#[test]
fn markdown_tables_render_as_structural_rich_text_rows() {
    let lines = render_markdown("| A | B |\n| - | - |\n| one | two |", &theme(), Some(30));
    assert!(lines.iter().any(markdown_rendered_line_is_table_row));
    assert!(
        lines
            .iter()
            .any(|line| line.kind == RichTextLineKind::MarkdownTableSeparator)
    );
    assert!(lines.iter().any(|line| line.copy_text.is_some()));
}

/// Verifies every physical fragment of wrapped table links retains the
/// link rendition on both alternating and ordinary body rows.
#[test]
fn markdown_tables_preserve_link_styles_across_wrapped_body_rows() {
    let first = "11111111-1111-1111-1111-111111111111";
    let second = "22222222-2222-2222-2222-222222222222";
    let markdown = format!(
        "| ID | Title |\n| --- | --- |\n| [{first}](https://example.test/{first}) | First |\n| [{second}](https://example.test/{second}) | Second |"
    );
    let lines = render_markdown(&markdown, &theme(), Some(24));
    let body_lines = lines
        .iter()
        .filter(|line| {
            matches!(
                line.kind,
                RichTextLineKind::MarkdownTableRow | RichTextLineKind::MarkdownTableContinuation
            ) && line.display.contains('│')
                && !line.display.contains(" ID ")
        })
        .collect::<Vec<_>>();

    assert!(body_lines.len() >= 4, "{body_lines:?}");
    for line in body_lines {
        let mut dividers = line.display.match_indices('│').map(|(index, _)| index);
        let first_divider = dividers.next().unwrap();
        let second_divider = dividers.next().unwrap();
        let cell = &line.display[first_divider + '│'.len_utf8()..second_divider];
        let fragment = cell.trim();
        if fragment.is_empty() {
            continue;
        }
        let fragment_byte_start = line.display.find(fragment).unwrap();
        let fragment_start = terminal_text_width(&line.display[..fragment_byte_start]);
        let fragment_end = fragment_start.saturating_add(terminal_text_width(fragment));
        assert!(
            line.style_spans.iter().any(|span| {
                span.start <= fragment_start
                    && span.start.saturating_add(span.length) >= fragment_end
                    && span.rendition.foreground == Some(theme().link)
                    && span.rendition.bold
                    && span.rendition.underline
            }),
            "wrapped link fragment lacks link style: {line:?}"
        );
    }
}

/// Verifies structurally narrow stacked tables retain link styling when a
/// linked cell value wraps across multiple physical rows.
#[test]
fn stacked_markdown_tables_preserve_wrapped_link_styles() {
    let id = "11111111-1111-1111-1111-111111111111";
    let markdown =
        format!("| ID | Title |\n| --- | --- |\n| [{id}](https://example.test/{id}) | First |");
    let lines = render_markdown(&markdown, &theme(), Some(8));
    let linked_fragments = lines
        .iter()
        .filter(|line| {
            line.display
                .chars()
                .any(|character| character.is_ascii_digit())
        })
        .collect::<Vec<_>>();

    assert!(linked_fragments.len() > 1, "{lines:?}");
    for line in linked_fragments {
        let start = line
            .display
            .find(|character: char| character.is_ascii_digit())
            .map(|byte| terminal_text_width(&line.display[..byte]))
            .unwrap();
        let length = terminal_text_width(line.display[start..].trim_end());
        assert!(
            line.style_spans.iter().any(|span| {
                span.start <= start
                    && span.start.saturating_add(span.length) >= start.saturating_add(length)
                    && span.rendition.foreground == Some(theme().link)
                    && span.rendition.bold
                    && span.rendition.underline
            }),
            "stacked link fragment lacks link style: {line:?}"
        );
    }
}

/// Verifies tables below their box-drawing structural width fall back to
/// header/value rows that preserve every column without exceeding the pane.
#[test]
fn markdown_tables_stack_when_their_structural_width_exceeds_the_pane() {
    let lines = render_markdown(
        "| Name | City | State | Tier |\n| --- | --- | --- | --- |\n| Ada | Zürich | ready | gold |",
        &theme(),
        Some(14),
    );
    let rendered = lines
        .iter()
        .map(|line| line.display.as_str())
        .collect::<Vec<_>>();

    assert!(
        rendered.iter().any(|line| line.contains("Name: Ada")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains("City: Zürich")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains("State: ready")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains("Tier: gold")),
        "{rendered:?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| terminal_text_width(line.display.as_str()) <= 14),
        "{rendered:?}"
    );
    assert!(
        lines.iter().all(|line| !line.display.contains('│')),
        "{rendered:?}"
    );
}

/// Verifies wrapped fragments of a final table row remain contiguous when
/// the source table is followed by an authored blank line and prose.
#[test]
fn markdown_table_continuations_precede_following_authored_blank_line() {
    let lines = render_markdown(
        "| Column |\n| --- |\n| final row has several words |\n\nAfter",
        &theme(),
        Some(12),
    );
    let blank_index = lines
        .iter()
        .position(|line| line.display.is_empty())
        .expect("the authored blank line should be rendered");
    let continuation_indices = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.kind == RichTextLineKind::MarkdownTableContinuation).then_some(index)
        })
        .collect::<Vec<_>>();

    assert!(!continuation_indices.is_empty(), "{lines:?}");
    assert!(
        continuation_indices
            .iter()
            .all(|index| *index < blank_index),
        "{lines:?}"
    );
    assert_eq!(
        lines
            .get(blank_index.saturating_add(1))
            .map(|line| line.display.as_str()),
        Some("After"),
        "{lines:?}"
    );
}

/// Verifies rendered tables receive one presentation-only buffer before
/// immediately following prose while table-only output stays unchanged.
#[test]
fn markdown_tables_buffer_following_content() {
    let followed = render_markdown(
        "| Column |\n| --- |\n| value |\n\nAfter",
        &theme(),
        Some(24),
    );
    let after_index = followed
        .iter()
        .position(|line| line.display == "After")
        .expect("following prose should be rendered");
    assert!(after_index > 0, "{followed:?}");
    assert!(followed[after_index - 1].display.is_empty(), "{followed:?}");
    assert_eq!(followed[after_index - 1].copy_text.as_deref(), Some(""));

    let table_only = render_markdown("| Column |\n| --- |\n| value |", &theme(), Some(24));
    assert!(
        table_only
            .last()
            .is_some_and(|line| !line.display.is_empty())
    );
}
