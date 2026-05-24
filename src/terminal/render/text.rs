//! Terminal render text-cell and width helpers.
//!
//! This module owns low-level terminal text segmentation, display-width
//! measurement, style-span clipping, copy-selection coordinate helpers, and
//! the internal wide-glyph sentinel used by pane/window canvas rendering.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::error::{MezError, Result};
use crate::layout::Size;
use crate::terminal::{CopyPosition, TerminalStyleSpan, TerminalStyledLine};

/// Internal marker for cells occupied by the continuation half of a wide glyph.
const TERMINAL_WIDE_CONTINUATION_CELL: char = '\0';

/// Writes one single-width cell while removing any overlapping wide glyph.
///
/// A divider or frame cell can land on either half of a previously rendered
/// wide glyph. If only the sentinel half is overwritten, the leading glyph
/// would still consume two terminal cells when collected into a string and
/// would shift everything to its right. Clearing both halves keeps the canvas
/// and the terminal's display-width model aligned.
pub(super) fn write_single_width_cell(row: &mut [char], column: usize, glyph: char) {
    if column >= row.len() {
        return;
    }
    if row[column] == TERMINAL_WIDE_CONTINUATION_CELL && column > 0 {
        row[column - 1] = ' ';
    }
    if row
        .get(column.saturating_add(1))
        .is_some_and(|next| *next == TERMINAL_WIDE_CONTINUATION_CELL)
    {
        row[column.saturating_add(1)] = ' ';
    }
    row[column] = glyph;
}

/// Writes bounded text into a terminal cell row, marking wide-glyph
/// continuations with an internal sentinel.
pub(super) fn write_text_cells(
    row: &mut [char],
    column_start: usize,
    max_columns: usize,
    text: &str,
) {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(&fit_width(text, max_columns)) {
        let grapheme_width = terminal_grapheme_width(grapheme);
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
        let ch = grapheme.chars().next().unwrap_or(' ');
        row[cell] = ch;
        for continuation in 1..grapheme_width {
            let continuation_cell = cell.saturating_add(continuation);
            if continuation_cell < row.len() {
                row[continuation_cell] = TERMINAL_WIDE_CONTINUATION_CELL;
            }
        }
        used = used.saturating_add(grapheme_width);
    }
}

/// Collects display cells into terminal text while omitting internal wide-cell
/// continuation sentinels.
pub(super) fn collect_text_cells(row: Vec<char>) -> String {
    let mut output = String::new();
    let mut index = 0usize;
    while index < row.len() {
        let ch = row[index];
        if ch == TERMINAL_WIDE_CONTINUATION_CELL {
            index = index.saturating_add(1);
            continue;
        }
        output.push(ch);
        if row
            .get(index.saturating_add(1))
            .is_some_and(|next| *next == TERMINAL_WIDE_CONTINUATION_CELL)
            && UnicodeWidthChar::width(ch).unwrap_or(0) == 1
            && terminal_char_width(ch) == 2
        {
            output.push('\u{FE0F}');
        }
        index = index.saturating_add(1);
    }
    output
}

/// Builds a blank terminal cell canvas for the given size.
pub(in crate::terminal) fn blank_cells(size: Size) -> Vec<Vec<char>> {
    (0..size.rows).map(|_| blank_row(size.columns)).collect()
}

/// Builds one blank terminal cell row with the requested column count.
pub(in crate::terminal) fn blank_row(columns: u16) -> Vec<char> {
    vec![' '; usize::from(columns)]
}

/// Converts a cell row into trimmed terminal text.
pub(in crate::terminal) fn trim_row(row: &[char]) -> String {
    row.iter().collect::<String>().trim_end().to_string()
}

/// Fits terminal text to an exact display width, padding with spaces if needed.
pub(super) fn fit_width(value: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    for grapheme in terminal_graphemes(value) {
        let grapheme_width = terminal_grapheme_width(grapheme);
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

/// Returns the display width that would be occupied when fitting text to a
/// bounded terminal row.
pub(super) fn fitted_text_width(value: &str, max_width: usize) -> usize {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(value) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        if used.saturating_add(grapheme_width) > max_width {
            break;
        }
        used = used.saturating_add(grapheme_width);
    }
    used
}

/// Fits a styled terminal line and clips its style spans to the retained width.
pub(super) fn fit_styled_width(line: &TerminalStyledLine, width: usize) -> TerminalStyledLine {
    let text = fit_width(&line.text, width);
    let style_spans = line
        .style_spans
        .iter()
        .filter_map(|span| clip_style_span(*span, width))
        .collect::<Vec<_>>();
    TerminalStyledLine {
        text,
        style_spans,
        copy_text: line.copy_text.clone(),
    }
}

/// Shifts a style span by a terminal column offset.
pub(super) fn offset_style_span(
    span: TerminalStyleSpan,
    column_offset: usize,
) -> TerminalStyleSpan {
    TerminalStyleSpan {
        start: span.start.saturating_add(column_offset),
        length: span.length,
        rendition: span.rendition,
    }
}

/// Clips a style span to the given terminal row width.
pub(super) fn clip_style_span(span: TerminalStyleSpan, width: usize) -> Option<TerminalStyleSpan> {
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

/// Searches forward from the requested line, wrapping to the top if needed.
pub(in crate::terminal) fn search_forward(
    lines: &[String],
    start_line: usize,
    query: &str,
) -> Option<(CopyPosition, usize)> {
    if lines.is_empty() {
        return None;
    }
    for (line_index, line) in lines
        .iter()
        .enumerate()
        .skip(start_line.min(lines.len() - 1))
    {
        if let Some(byte_index) = line.find(query) {
            return Some((
                CopyPosition {
                    line: line_index,
                    column: char_column_at_byte(line, byte_index),
                },
                char_count(query),
            ));
        }
    }
    for (line_index, line) in lines.iter().enumerate().take(start_line.min(lines.len())) {
        if let Some(byte_index) = line.find(query) {
            return Some((
                CopyPosition {
                    line: line_index,
                    column: char_column_at_byte(line, byte_index),
                },
                char_count(query),
            ));
        }
    }
    None
}

/// Searches backward from the requested line, wrapping to the bottom if needed.
pub(in crate::terminal) fn search_backward(
    lines: &[String],
    start_line: usize,
    query: &str,
) -> Option<(CopyPosition, usize)> {
    if lines.is_empty() {
        return None;
    }
    let start = start_line.min(lines.len() - 1);
    for line_index in (0..=start).rev() {
        if let Some(byte_index) = lines[line_index].rfind(query) {
            return Some((
                CopyPosition {
                    line: line_index,
                    column: char_column_at_byte(&lines[line_index], byte_index),
                },
                char_count(query),
            ));
        }
    }
    for line_index in ((start + 1)..lines.len()).rev() {
        if let Some(byte_index) = lines[line_index].rfind(query) {
            return Some((
                CopyPosition {
                    line: line_index,
                    column: char_column_at_byte(&lines[line_index], byte_index),
                },
                char_count(query),
            ));
        }
    }
    None
}

/// Validates that a copy-mode position references an existing rendered line.
pub(in crate::terminal) fn validate_copy_position(
    lines: &[String],
    position: CopyPosition,
) -> Result<()> {
    if lines.get(position.line).is_none() {
        return Err(MezError::invalid_args(
            "copy mode selection line is out of range",
        ));
    }
    Ok(())
}

/// Orders a copy-mode selection range from earlier position to later position.
pub(in crate::terminal) fn normalize_selection(
    start: CopyPosition,
    end: CopyPosition,
) -> (CopyPosition, CopyPosition) {
    if start <= end {
        (start, end)
    } else {
        (end, start)
    }
}

/// Returns a display-column slice from one terminal line.
pub(in crate::terminal) fn line_slice(line: &str, start: usize, end: usize) -> String {
    let mut output = String::new();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = terminal_grapheme_width(grapheme);
        let next = column.saturating_add(width);
        if next <= start {
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
pub(in crate::terminal) fn char_count(value: &str) -> usize {
    terminal_text_width(value)
}

/// Returns the terminal display column for a byte index inside a value.
fn char_column_at_byte(value: &str, byte_index: usize) -> usize {
    terminal_text_width(&value[..byte_index])
}

/// Returns the terminal display width of one Unicode scalar.
pub(in crate::terminal) fn terminal_char_width(ch: char) -> usize {
    if terminal_scalar_has_emoji_presentation_width(ch) {
        return 2;
    }
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Returns the display width of one Unicode grapheme cluster.
///
/// # Parameters
/// - `grapheme`: The extended grapheme cluster to measure.
pub(crate) fn terminal_grapheme_width(grapheme: &str) -> usize {
    let mut chars = grapheme.chars();
    if let Some(ch) = chars.next()
        && chars.next().is_none()
    {
        return terminal_char_width(ch);
    }
    UnicodeWidthStr::width(grapheme)
}

/// Returns the display width of one complete terminal string.
///
/// # Parameters
/// - `value`: The terminal text to measure.
pub(crate) fn terminal_text_width(value: &str) -> usize {
    terminal_graphemes(value).map(terminal_grapheme_width).sum()
}

/// Returns an iterator over Unicode grapheme clusters in terminal text.
///
/// # Parameters
/// - `value`: The terminal text to segment.
pub(crate) fn terminal_graphemes(value: &str) -> impl Iterator<Item = &str> {
    UnicodeSegmentation::graphemes(value, true)
}

/// Returns whether a non-ASCII scalar has emoji presentation when followed by a
/// variation selector.
///
/// This is the conservative fallback for terminal parser paths that receive one
/// scalar at a time and cannot see the full grapheme cluster before deciding
/// whether the cursor should advance. Full-string rendering paths should use
/// [`terminal_grapheme_width`] instead so text-presentation sequences such as
/// `✔︎` retain their one-cell width.
///
/// # Parameters
/// - `ch`: The Unicode scalar whose terminal-cell width is being normalized.
fn terminal_scalar_has_emoji_presentation_width(ch: char) -> bool {
    if ch.is_ascii() || UnicodeWidthChar::width(ch).unwrap_or(0) != 1 {
        return false;
    }
    let mut emoji_presentation = String::new();
    emoji_presentation.push(ch);
    emoji_presentation.push('\u{FE0F}');
    UnicodeWidthStr::width(emoji_presentation.as_str()) == 2
}
