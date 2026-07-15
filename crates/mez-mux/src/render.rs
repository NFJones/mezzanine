//! Dependency-neutral terminal canvas and styled-row composition.
//!
//! This module owns display-cell storage, Unicode-width-aware text placement,
//! terminal-row fitting and slicing, and style-span clipping/overlay behavior.
//! It deliberately excludes product themes, agent transcript wrapping, prompt
//! policy, overlays, and host terminal encoding.

use mez_terminal::{
    TerminalStyleSpan, TerminalStyledLine, terminal_emoji_width, terminal_grapheme_width,
    terminal_graphemes, terminal_text_width,
};

/// One display-cell slot in a mux-owned render canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRenderCell {
    text: String,
    continuation: bool,
}

impl TerminalRenderCell {
    /// Builds one leading render cell containing a single glyph.
    pub fn from_char(ch: char) -> Self {
        Self {
            text: ch.to_string(),
            continuation: false,
        }
    }

    /// Builds one leading render cell containing a complete grapheme cluster.
    pub fn from_grapheme(grapheme: &str) -> Self {
        Self {
            text: grapheme.to_string(),
            continuation: false,
        }
    }

    /// Builds one continuation cell for a multi-column grapheme cluster.
    pub fn continuation() -> Self {
        Self {
            text: String::new(),
            continuation: true,
        }
    }
}

/// Builds one render-canvas row initialized to the requested fill glyph.
pub fn blank_render_row(columns: usize, fill: char) -> Vec<TerminalRenderCell> {
    vec![TerminalRenderCell::from_char(fill); columns]
}

/// Builds a render-canvas matrix initialized to the requested fill glyph.
pub fn blank_render_cells(rows: usize, columns: usize, fill: char) -> Vec<Vec<TerminalRenderCell>> {
    (0..rows).map(|_| blank_render_row(columns, fill)).collect()
}

/// Writes one single-width cell while removing any overlapping wide glyph.
pub fn write_single_width_cell(row: &mut [TerminalRenderCell], column: usize, glyph: char) {
    if column >= row.len() {
        return;
    }
    if row[column].continuation {
        let mut left = column;
        while left > 0 && row[left].continuation {
            row[left] = TerminalRenderCell::from_char(' ');
            left = left.saturating_sub(1);
        }
        row[left] = TerminalRenderCell::from_char(' ');
    }
    let mut right = column.saturating_add(1);
    while right < row.len() && row[right].continuation {
        row[right] = TerminalRenderCell::from_char(' ');
        right = right.saturating_add(1);
    }
    row[column] = TerminalRenderCell::from_char(glyph);
}

/// Writes bounded text into a terminal cell row using wide-cell sentinels.
pub fn write_text_cells(
    row: &mut [TerminalRenderCell],
    column_start: usize,
    max_columns: usize,
    text: &str,
) {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(&fit_width(text, max_columns)) {
        let grapheme_width = active_grapheme_width(grapheme);
        if grapheme_width == 0 {
            continue;
        }
        if used.saturating_add(grapheme_width) > max_columns {
            break;
        }
        let cell = column_start.saturating_add(used);
        if cell >= row.len() {
            break;
        }
        row[cell] = TerminalRenderCell::from_grapheme(grapheme);
        for continuation in 1..grapheme_width {
            let continuation_cell = cell.saturating_add(continuation);
            if continuation_cell < row.len() {
                row[continuation_cell] = TerminalRenderCell::continuation();
            }
        }
        used = used.saturating_add(grapheme_width);
    }
}

/// Collects display cells while omitting internal continuation sentinels.
pub fn collect_text_cells(row: Vec<TerminalRenderCell>) -> String {
    let mut output = String::new();
    for cell in row {
        if !cell.continuation {
            output.push_str(&cell.text);
        }
    }
    output
}

/// Fits terminal text to an exact display width, padding with spaces.
pub fn fit_width(value: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    for grapheme in terminal_graphemes(value) {
        let grapheme_width = active_grapheme_width(grapheme);
        if used.saturating_add(grapheme_width) > width {
            break;
        }
        output.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    if used < width {
        output.push_str(&" ".repeat(width - used));
    }
    output
}

/// Returns the display width retained by bounded terminal-row fitting.
pub fn fitted_text_width(value: &str, max_width: usize) -> usize {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(value) {
        let grapheme_width = active_grapheme_width(grapheme);
        if used.saturating_add(grapheme_width) > max_width {
            break;
        }
        used = used.saturating_add(grapheme_width);
    }
    used
}

/// Fits a styled terminal line and clips spans to the retained width.
pub fn fit_styled_width(line: &TerminalStyledLine, width: usize) -> TerminalStyledLine {
    let text = fit_width(&line.text, width);
    let retained_width = fitted_text_width(&line.text, width);
    let style_spans = line
        .style_spans
        .iter()
        .filter_map(|span| clip_style_span(*span, retained_width))
        .collect::<Vec<_>>();
    TerminalStyledLine {
        text,
        style_spans,
        copy_text: line.copy_text.clone(),
    }
}

/// Replaces one fixed-width range with clipped source style spans.
pub fn overlay_fixed_column_style_spans(
    spans: &mut Vec<TerminalStyleSpan>,
    column_start: usize,
    width: usize,
    source_spans: &[TerminalStyleSpan],
) {
    let region_end = column_start.saturating_add(width);
    let mut retained = Vec::with_capacity(spans.len().saturating_add(source_spans.len()));
    for span in std::mem::take(spans) {
        if style_span_overlaps_columns(span, column_start, region_end) {
            retained.extend(style_span_segments_outside_range(
                span,
                column_start,
                region_end,
            ));
        } else {
            retained.push(span);
        }
    }
    retained.extend(
        source_spans
            .iter()
            .filter_map(|span| clip_style_span(*span, width))
            .map(|span| offset_style_span(span, column_start)),
    );
    *spans = retained;
}

/// Returns whether a style span touches a half-open column range.
pub fn style_span_overlaps_columns(span: TerminalStyleSpan, start: usize, end: usize) -> bool {
    span.start < end && span.start.saturating_add(span.length) > start
}

/// Keeps the parts of a style span outside a replaced column range.
pub fn style_span_segments_outside_range(
    span: TerminalStyleSpan,
    start: usize,
    end: usize,
) -> Vec<TerminalStyleSpan> {
    let span_end = span.start.saturating_add(span.length);
    let mut segments = Vec::with_capacity(2);
    if span.start < start {
        segments.push(TerminalStyleSpan {
            start: span.start,
            length: start.saturating_sub(span.start),
            rendition: span.rendition,
        });
    }
    if span_end > end {
        segments.push(TerminalStyleSpan {
            start: end,
            length: span_end.saturating_sub(end),
            rendition: span.rendition,
        });
    }
    segments
        .into_iter()
        .filter(|segment| segment.length > 0)
        .collect()
}

/// Shifts a style span by a terminal column offset.
pub fn offset_style_span(span: TerminalStyleSpan, column_offset: usize) -> TerminalStyleSpan {
    TerminalStyleSpan {
        start: span.start.saturating_add(column_offset),
        length: span.length,
        rendition: span.rendition,
    }
}

/// Clips a style span to the given terminal row width.
pub fn clip_style_span(span: TerminalStyleSpan, width: usize) -> Option<TerminalStyleSpan> {
    if span.start >= width {
        return None;
    }
    let end = span.start.saturating_add(span.length).min(width);
    Some(TerminalStyleSpan {
        start: span.start,
        length: end.saturating_sub(span.start),
        rendition: span.rendition,
    })
    .filter(|span| span.length > 0)
}

/// Returns a display-column slice from one terminal line.
pub fn line_slice(line: &str, start: usize, end: usize) -> String {
    let mut output = String::new();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = active_grapheme_width(grapheme);
        let next = column.saturating_add(width);
        if next <= start {
            column = next;
            continue;
        }
        if column < start && next > start {
            column = next;
            continue;
        }
        if column >= end || next > end {
            break;
        }
        output.push_str(grapheme);
        column = next;
    }
    output
}

/// Returns the terminal display column count for a value.
pub fn char_count(value: &str) -> usize {
    terminal_text_width(value, terminal_emoji_width())
}

fn active_grapheme_width(grapheme: &str) -> usize {
    terminal_grapheme_width(grapheme, terminal_emoji_width())
}

#[cfg(test)]
mod tests {
    use super::{
        blank_render_row, collect_text_cells, fit_styled_width, line_slice,
        overlay_fixed_column_style_spans, write_single_width_cell, write_text_cells,
    };
    use mez_terminal::{GraphicRendition, TerminalStyleSpan, TerminalStyledLine};

    /// Verifies wide-glyph canvas writes preserve display columns when a
    /// divider overwrites the continuation half of a grapheme.
    #[test]
    fn render_canvas_clears_overwritten_wide_glyphs() {
        let mut row = blank_render_row(4, ' ');
        write_text_cells(&mut row, 0, 4, "界x");
        write_single_width_cell(&mut row, 1, '│');

        assert_eq!(collect_text_cells(row), " │x ");
    }

    /// Verifies styled fitting clips spans and terminal slicing never returns
    /// half of a wide grapheme cluster.
    #[test]
    fn styled_rows_clip_and_slice_on_display_columns() {
        let rendition = GraphicRendition::default();
        let line = TerminalStyledLine {
            text: "a界b".to_string(),
            style_spans: vec![TerminalStyleSpan {
                start: 1,
                length: 2,
                rendition,
            }],
            copy_text: None,
        };

        let fitted = fit_styled_width(&line, 2);
        assert_eq!(fitted.text, "a ");
        assert!(fitted.style_spans.is_empty());
        assert_eq!(line_slice("a界b", 1, 3), "界");
    }

    /// Verifies a fixed-column style overlay preserves span fragments on both
    /// sides and shifts the replacement span into absolute columns.
    #[test]
    fn style_overlay_preserves_outside_segments() {
        let rendition = GraphicRendition::default();
        let mut spans = vec![TerminalStyleSpan {
            start: 0,
            length: 6,
            rendition,
        }];
        overlay_fixed_column_style_spans(
            &mut spans,
            2,
            2,
            &[TerminalStyleSpan {
                start: 0,
                length: 2,
                rendition,
            }],
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| (span.start, span.length))
                .collect::<Vec<_>>(),
            vec![(0, 2), (4, 2), (2, 2)]
        );
    }
}
