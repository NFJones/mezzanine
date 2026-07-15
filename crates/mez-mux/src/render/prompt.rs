//! Dependency-neutral prompt layout and bounded region composition.
//!
//! This module owns terminal-cell wrapping, cursor tracking, shadow-range
//! tracking, viewport selection, region clipping, and bounded line writes. It
//! deliberately excludes prompt kinds, themes, agent labels, and completion
//! policy; product adapters provide already-rendered text and optional summary
//! rows.

use crate::presentation::ReadlinePromptRegion;
use mez_terminal::{
    terminal_emoji_width, terminal_grapheme_width, terminal_graphemes, terminal_text_width,
};

use super::fit_width;

/// One terminal-cell range occupied by completion shadow text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptShadowSpan {
    /// Zero-based terminal-cell start column.
    pub start: usize,
    /// Number of terminal cells in the range.
    pub length: usize,
}

/// Visible wrapped prompt rows and cursor metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedPromptLayout {
    /// Exact-width visible prompt rows.
    pub lines: Vec<String>,
    /// Shadow-text ranges corresponding to each visible row.
    pub shadow_spans: Vec<Vec<PromptShadowSpan>>,
    /// Cursor row relative to the visible prompt rows.
    pub cursor_row: usize,
    /// Cursor column relative to its visible row.
    pub cursor_column: usize,
    /// Whether the cursor falls inside the visible row window.
    pub cursor_visible: bool,
}

/// Produces a bounded wrapped prompt layout from already-rendered text.
///
/// `replacement_first_line` lets the caller replace the first visible row
/// with a product-authored summary while retaining mux-owned viewport and
/// cursor behavior.
pub fn layout_wrapped_prompt(
    value: &str,
    cursor_index: usize,
    shadow_range: Option<(usize, usize)>,
    width: usize,
    max_rows: usize,
    continuation_indent: usize,
    replacement_first_line: Option<&str>,
) -> WrappedPromptLayout {
    if width == 0 || max_rows == 0 {
        return WrappedPromptLayout {
            lines: Vec::new(),
            shadow_spans: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        };
    }
    let (chunks, chunk_shadow_spans, cursor_row, cursor_column) =
        wrap_prompt_line_with_cursor_and_shadow(
            value,
            cursor_index,
            shadow_range,
            width,
            continuation_indent,
        );
    let max_first_visible_chunk = chunks.len().saturating_sub(max_rows);
    let first_visible_chunk = cursor_row
        .saturating_add(1)
        .saturating_sub(max_rows)
        .min(max_first_visible_chunk);
    let mut lines = chunks
        .iter()
        .skip(first_visible_chunk)
        .take(max_rows)
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    let mut shadow_spans = chunk_shadow_spans
        .iter()
        .skip(first_visible_chunk)
        .take(max_rows)
        .cloned()
        .collect::<Vec<_>>();
    let cursor_visible = cursor_row >= first_visible_chunk
        && cursor_row < first_visible_chunk.saturating_add(lines.len());
    let mut cursor_column = cursor_column;
    if let Some(replacement) = replacement_first_line
        && let Some(first) = lines.first_mut()
    {
        *first = fit_width(replacement, width);
        if let Some(first_spans) = shadow_spans.first_mut() {
            first_spans.clear();
        }
        cursor_column = width;
    }
    WrappedPromptLayout {
        lines,
        shadow_spans,
        cursor_row: cursor_row.saturating_sub(first_visible_chunk),
        cursor_column: clamp_visible_cursor_column(cursor_column, width),
        cursor_visible,
    }
}

/// Wraps one prompt line while preserving cursor and shadow-text positions.
pub fn wrap_prompt_line_with_cursor_and_shadow(
    value: &str,
    cursor_index: usize,
    shadow_range: Option<(usize, usize)>,
    width: usize,
    continuation_indent: usize,
) -> (Vec<String>, Vec<Vec<PromptShadowSpan>>, usize, usize) {
    let mut chunks = Vec::new();
    let mut chunk_shadow_spans = Vec::new();
    let mut current = String::new();
    let mut current_shadow_spans = Vec::new();
    let mut used = 0usize;
    let mut cursor = None;
    let mut last_space_break: Option<(usize, usize, Vec<PromptShadowSpan>)> = None;
    let continuation_prefix = " ".repeat(continuation_indent);
    for (index, ch) in value.chars().enumerate() {
        if ch == '\n' {
            if cursor.is_none() && index == cursor_index {
                cursor = Some((chunks.len(), used));
            }
            chunks.push(current);
            chunk_shadow_spans.push(current_shadow_spans);
            current = continuation_prefix.clone();
            current_shadow_spans = Vec::new();
            used = continuation_indent;
            last_space_break = None;
            continue;
        }
        let ch_width = terminal_text_width(&ch.to_string(), terminal_emoji_width()).max(1);
        if used > 0 && used.saturating_add(ch_width) > width {
            if let Some((text_break, consumed_break, spans_at_break)) = last_space_break.take() {
                let consumed_columns =
                    terminal_text_width(&current[..consumed_break], terminal_emoji_width());
                if consumed_columns > continuation_indent {
                    if let Some(cursor_position) = cursor.as_mut()
                        && cursor_position.0 == chunks.len()
                        && cursor_position.1 >= consumed_columns
                    {
                        cursor_position.0 = cursor_position.0.saturating_add(1);
                        cursor_position.1 = continuation_indent
                            .saturating_add(cursor_position.1.saturating_sub(consumed_columns));
                    }
                    chunks.push(current[..text_break].to_string());
                    chunk_shadow_spans.push(spans_at_break);
                    current = format!("{continuation_prefix}{}", &current[consumed_break..]);
                    current_shadow_spans = prompt_shadow_spans_after_consumed(
                        &current_shadow_spans,
                        consumed_columns,
                        continuation_indent,
                    );
                    used = terminal_text_width(&current, terminal_emoji_width());
                } else {
                    chunks.push(current);
                    chunk_shadow_spans.push(current_shadow_spans);
                    current = continuation_prefix.clone();
                    current_shadow_spans = Vec::new();
                    used = continuation_indent;
                }
            } else {
                chunks.push(current);
                chunk_shadow_spans.push(current_shadow_spans);
                current = continuation_prefix.clone();
                current_shadow_spans = Vec::new();
                used = continuation_indent;
            }
        }
        if cursor.is_none() && index == cursor_index {
            cursor = Some((chunks.len(), used));
        }
        let current_byte_len = current.len();
        current.push(ch);
        if shadow_range.is_some_and(|(start, end)| index >= start && index < end) {
            push_prompt_shadow_cell(&mut current_shadow_spans, used, ch_width);
        }
        used = used.saturating_add(ch_width);
        if ch.is_whitespace() && used > 0 {
            last_space_break = Some((
                current_byte_len,
                current.len(),
                current_shadow_spans.clone(),
            ));
        }
    }
    if cursor.is_none() && value.chars().count() == cursor_index {
        cursor = Some((chunks.len(), used));
    }
    chunks.push(current);
    chunk_shadow_spans.push(current_shadow_spans);
    let (cursor_row, cursor_column) = cursor.unwrap_or((chunks.len().saturating_sub(1), 0));
    (chunks, chunk_shadow_spans, cursor_row, cursor_column)
}

/// Clips a prompt region to the available client cells.
pub fn clipped_prompt_region(
    region: ReadlinePromptRegion,
    client_width: usize,
    client_rows: usize,
) -> Option<ReadlinePromptRegion> {
    if region.row >= client_rows || region.column >= client_width {
        return None;
    }
    let columns = region
        .columns
        .min(client_width.saturating_sub(region.column));
    let rows = region.rows.min(client_rows.saturating_sub(region.row));
    (columns > 0 && rows > 0).then_some(ReadlinePromptRegion {
        row: region.row,
        column: region.column,
        columns,
        rows,
    })
}

/// Writes one exact-width value into a line while retaining unaffected cells.
pub fn write_line_segment(line: &mut String, column: usize, width: usize, value: &str) {
    if width == 0 {
        return;
    }
    let target_end = column.saturating_add(width);
    let original = line.clone();
    let mut output = String::new();
    let mut current_column = 0usize;
    for grapheme in terminal_graphemes(&original) {
        let grapheme_width = terminal_grapheme_width(grapheme, terminal_emoji_width());
        let next_column = current_column.saturating_add(grapheme_width);
        if next_column <= column {
            output.push_str(grapheme);
            current_column = next_column;
            continue;
        }
        break;
    }
    let output_width = terminal_text_width(&output, terminal_emoji_width());
    if output_width < column {
        output.push_str(&" ".repeat(column.saturating_sub(output_width)));
    }
    output.push_str(&fit_width(value, width));
    current_column = 0;
    for grapheme in terminal_graphemes(&original) {
        let grapheme_width = terminal_grapheme_width(grapheme, terminal_emoji_width());
        if current_column >= target_end {
            output.push_str(grapheme);
        }
        current_column = current_column.saturating_add(grapheme_width);
    }
    *line = output;
}

fn clamp_visible_cursor_column(column: usize, width: usize) -> usize {
    column.min(width.saturating_sub(1))
}

fn prompt_shadow_spans_after_consumed(
    spans: &[PromptShadowSpan],
    consumed_columns: usize,
    shift_columns: usize,
) -> Vec<PromptShadowSpan> {
    spans
        .iter()
        .filter_map(|span| {
            let end = span.start.saturating_add(span.length);
            (end > consumed_columns).then_some(PromptShadowSpan {
                start: span
                    .start
                    .saturating_sub(consumed_columns)
                    .saturating_add(shift_columns),
                length: end.saturating_sub(consumed_columns.max(span.start)),
            })
        })
        .collect()
}

fn push_prompt_shadow_cell(spans: &mut Vec<PromptShadowSpan>, start: usize, length: usize) {
    if let Some(last) = spans.last_mut()
        && last.start.saturating_add(last.length) == start
    {
        last.length = last.length.saturating_add(length);
        return;
    }
    spans.push(PromptShadowSpan { start, length });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies wrapping preserves cursor and shadow metadata without product prompt policy.
    #[test]
    fn wrapped_prompt_layout_tracks_cursor_and_shadow_cells() {
        let layout = layout_wrapped_prompt("ab cdef", 4, Some((3, 6)), 5, 2, 0, None);

        assert_eq!(layout.lines, vec!["ab   ", "cdef "]);
        assert_eq!(layout.cursor_row, 1);
        assert_eq!(layout.cursor_column, 1);
        assert!(layout.cursor_visible);
        assert_eq!(
            layout.shadow_spans[1],
            vec![PromptShadowSpan {
                start: 0,
                length: 3
            }]
        );
    }

    /// Verifies a cursor inside a retroactively wrapped word moves onto the
    /// continuation row instead of remaining on the preceding visual row.
    #[test]
    fn wrap_prompt_cursor_tracks_retroactive_whitespace_wrap() {
        let (chunks, shadow_spans, cursor_row, cursor_column) =
            wrap_prompt_line_with_cursor_and_shadow("ab cdef", 4, None, 5, 0);

        assert_eq!(chunks, vec!["ab".to_string(), "cdef".to_string()]);
        assert_eq!(shadow_spans, vec![Vec::new(), Vec::new()]);
        assert_eq!((cursor_row, cursor_column), (1, 1));
    }

    /// Verifies a cursor captured at a wrap boundary moves to the configured
    /// continuation indent rather than the consumed trailing-space position.
    #[test]
    fn wrap_prompt_cursor_moves_to_continuation_indent_at_wrap_boundary() {
        let (chunks, shadow_spans, cursor_row, cursor_column) =
            wrap_prompt_line_with_cursor_and_shadow("aa bcd", 3, None, 5, 2);

        assert_eq!(chunks, vec!["aa".to_string(), "  bcd".to_string()]);
        assert_eq!(shadow_spans, vec![Vec::new(), Vec::new()]);
        assert_eq!((cursor_row, cursor_column), (1, 2));
    }

    /// Verifies prompt regions clip safely at the client viewport boundary.
    #[test]
    fn prompt_region_clips_to_client_viewport() {
        assert_eq!(
            clipped_prompt_region(
                ReadlinePromptRegion {
                    row: 2,
                    column: 3,
                    rows: 4,
                    columns: 8
                },
                6,
                4,
            ),
            Some(ReadlinePromptRegion {
                row: 2,
                column: 3,
                rows: 2,
                columns: 3
            })
        );
    }

    /// Verifies bounded segment writes preserve text outside the replaced region.
    #[test]
    fn line_segment_write_preserves_surrounding_cells() {
        let mut line = String::from("abcdef");
        write_line_segment(&mut line, 2, 2, "XY");
        assert_eq!(line, "abXYef");
    }
}
