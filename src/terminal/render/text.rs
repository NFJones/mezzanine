//! Terminal render text-cell and width helpers.
//!
//! This module owns low-level terminal text segmentation, display-width
//! measurement, style-span clipping, copy-selection coordinate helpers, and
//! the internal wide-glyph sentinel used by pane/window canvas rendering.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::error::{MezError, Result};
use crate::layout::Size;
use crate::terminal::{CopyPosition, GraphicRendition, TerminalStyleSpan, TerminalStyledLine};

/// Internal marker for cells occupied by the continuation half of a wide glyph.
const TERMINAL_WIDE_CONTINUATION_CELL: char = '\0';
/// Maximum display-cell width for Mezzanine-owned agent log rows.
pub(crate) const AGENT_LOG_WRAP_COLUMN_CAP: usize = 120;

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
        // Clear additional continuation cells to the left for emoji ZWJ
        // sequences wider than 2 cells.
        let mut left = column.saturating_sub(2);
        while left > 0 && row[left] == TERMINAL_WIDE_CONTINUATION_CELL {
            row[left - 1] = ' ';
            left = left.saturating_sub(1);
            if left == 0 {
                break;
            }
            left = left.saturating_sub(1);
        }
    }
    // Clear all continuation cells to the right.
    let mut right = column.saturating_add(1);
    while right < row.len() && row[right] == TERMINAL_WIDE_CONTINUATION_CELL {
        row[right] = ' ';
        right = right.saturating_add(1);
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

/// Collects screen cells into terminal text, omitting wide-character continuation
/// cells.
///
/// The terminal screen stores wide characters as a leading glyph followed by
/// `' '` continuation cells with the same rendition. These continuation
/// cells inflate display-width measurements and produce unwanted extra
/// spacing in rendered output. This function detects continuation cells by
/// inspecting both cell characters and renditions, stripping them from the
/// output so downstream measurement and rendering operate on semantic content
/// widths.
///
/// When `trim_trailing_whitespace` is true, trailing whitespace is removed
/// from the output. This is appropriate for plain-text rendering where blank
/// trailing cells carry no meaning. Styled outputs should pass `false` so
/// that trailing cells with non-default graphic renditions are preserved.
pub(in crate::terminal) fn collect_screen_cells(
    cells: &[char],
    renditions: &[GraphicRendition],
    trim_trailing_whitespace: bool,
) -> String {
    let mut output = String::new();
    let mut index = 0usize;
    while index < cells.len() {
        let ch = cells[index];
        if ch == ' '
            && index > 0
            && terminal_char_width(cells[index.saturating_sub(1)]) > 1
            && renditions.get(index).copied().unwrap_or_default()
                == renditions
                    .get(index.saturating_sub(1))
                    .copied()
                    .unwrap_or_default()
        {
            index = index.saturating_add(1);
            continue;
        }
        output.push(ch);
        index = index.saturating_add(1);
    }
    if trim_trailing_whitespace {
        output = output.trim_end().to_string();
    }
    output
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

/// Returns the bounded display width used for Mezzanine-owned agent log rows.
pub(crate) fn agent_log_wrap_width(terminal_width: u16) -> usize {
    usize::from(terminal_width).clamp(1, AGENT_LOG_WRAP_COLUMN_CAP)
}

/// Word-wraps one Mezzanine-owned agent log text block for terminal display.
///
/// Explicit newlines are preserved as row breaks. Individual logical rows wrap
/// at the nearest whitespace boundary before the display-cell limit, falling
/// back to hard grapheme boundaries when an unbroken token exceeds the limit.
pub(crate) fn wrap_agent_log_text(value: &str, terminal_width: u16) -> Vec<String> {
    let wrap_width = agent_log_wrap_width(terminal_width);
    value
        .split('\n')
        .flat_map(|line| wrap_agent_log_physical_line(line, wrap_width))
        .collect()
}

/// Word-wraps Mezzanine-owned agent log rows for terminal display.
pub(crate) fn wrap_agent_log_lines(lines: &[String], terminal_width: u16) -> Vec<String> {
    let mut wrapped = Vec::new();
    for line in lines {
        wrapped.extend(wrap_agent_log_text(line, terminal_width));
    }
    wrapped
}

/// Wraps one logical agent log row to a display-cell width.
fn wrap_agent_log_physical_line(line: &str, wrap_width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut remaining = line;
    while !remaining.is_empty() {
        let mut used = 0usize;
        let mut end_byte = 0usize;
        let mut saw_content = false;
        let mut last_break = None;
        for (byte_index, grapheme) in remaining.grapheme_indices(true) {
            let grapheme_width = terminal_grapheme_width(grapheme);
            if used.saturating_add(grapheme_width) > wrap_width {
                break;
            }
            if grapheme.chars().all(char::is_whitespace) {
                if saw_content {
                    last_break = Some((byte_index, byte_index.saturating_add(grapheme.len())));
                }
            } else {
                saw_content = true;
            }
            used = used.saturating_add(grapheme_width);
            end_byte = byte_index.saturating_add(grapheme.len());
        }
        if end_byte >= remaining.len() {
            rows.push(remaining.to_string());
            break;
        }
        if end_byte == 0
            && let Some(grapheme) = remaining.graphemes(true).next()
        {
            end_byte = grapheme.len();
        }
        if remaining[end_byte..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            rows.push(remaining[..end_byte].to_string());
            remaining = remaining[end_byte..].trim_start_matches(char::is_whitespace);
            continue;
        }
        if let Some((break_byte, next_byte)) = last_break
            && break_byte > 0
        {
            rows.push(remaining[..break_byte].to_string());
            remaining = remaining[next_byte..].trim_start_matches(char::is_whitespace);
        } else {
            rows.push(remaining[..end_byte].to_string());
            remaining = &remaining[end_byte..];
        }
    }
    rows
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

/// Replaces one fixed-width column range with source style spans clipped to that range.
///
/// Text overlays in terminal and runtime render paths both need the same style
/// invariant: preexisting spans outside the overlay range survive, overlapping
/// portions are removed, and source spans are clipped to the overlay width then
/// shifted into absolute terminal columns.
pub(crate) fn overlay_fixed_column_style_spans(
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
pub(super) fn style_span_overlaps_columns(
    span: TerminalStyleSpan,
    start: usize,
    end: usize,
) -> bool {
    span.start < end && span.start.saturating_add(span.length) > start
}

/// Keeps the parts of a style span that fall outside a replaced column range.
pub(super) fn style_span_segments_outside_range(
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
/// Terminal renderers display each grapheme cluster in a single cell span of
/// zero, one, or two columns even when a multi-scalar cluster contains emoji
/// or combining scalars whose Unicode widths would sum to a larger number.
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
    UnicodeWidthStr::width(grapheme).min(2)
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

#[cfg(test)]
mod tests {
    use super::{agent_log_wrap_width, terminal_text_width, wrap_agent_log_text};

    /// Verifies agent log wrapping uses the pane width until the global cap
    /// applies, so very wide terminals do not create unbounded transcript rows.
    #[test]
    fn agent_log_wrap_width_caps_terminal_width_at_120_columns() {
        assert_eq!(agent_log_wrap_width(0), 1);
        assert_eq!(agent_log_wrap_width(80), 80);
        assert_eq!(agent_log_wrap_width(200), 120);
    }

    /// Verifies ordinary agent prose wraps at whitespace and preserves explicit
    /// newlines, including blank lines that separate rendered log paragraphs.
    #[test]
    fn wrap_agent_log_text_preserves_newlines_and_wraps_at_words() {
        let wrapped = wrap_agent_log_text("alpha beta gamma\n\nbottom", 10);

        assert_eq!(wrapped, vec!["alpha beta", "gamma", "", "bottom"]);
    }

    /// Verifies long unbroken tokens are hard-split at grapheme boundaries so a
    /// single generated identifier cannot exceed the transcript row contract.
    #[test]
    fn wrap_agent_log_text_hard_splits_unbroken_tokens() {
        let wrapped = wrap_agent_log_text("abcdefghijkl", 4);

        assert_eq!(wrapped, vec!["abcd", "efgh", "ijkl"]);
    }

    /// Verifies wide Unicode graphemes count by terminal display width instead
    /// of bytes or scalar count when rows are split.
    #[test]
    fn wrap_agent_log_text_counts_wide_graphemes() {
        let wrapped = wrap_agent_log_text("✅✅✅", 4);

        assert_eq!(wrapped, vec!["✅✅", "✅"]);
        assert!(wrapped.iter().all(|line| terminal_text_width(line) <= 4));
    }

    /// Verifies multi-scalar terminal emoji grapheme clusters keep their
    /// rendered two-cell width so pane row accounting does not overcount
    /// modifier and regional-indicator sequences.
    #[test]
    fn terminal_text_width_keeps_terminal_emoji_clusters_at_two_cells() {
        for grapheme in ["👍🏻", "👍🏼", "👍🏽", "👍🏾", "🇪🇺", "🇯🇵", "🇧🇷", "🇨🇦"]
        {
            assert_eq!(super::terminal_grapheme_width(grapheme), 2, "{grapheme}");
            assert_eq!(terminal_text_width(grapheme), 2, "{grapheme}");
        }
    }

    /// Verifies mixed fullwidth text and multi-scalar emoji clusters still sum
    /// to the correct terminal row width after cluster widths are normalized.
    #[test]
    fn terminal_text_width_counts_mixed_fullwidth_text_and_emoji_clusters() {
        assert_eq!(terminal_text_width("ｓ 👍🏻 🇪🇺"), 8);
    }

    /// Verifies the 120-column cap is applied even when the active pane is
    /// wider, protecting persisted replay rows from host-width drift.
    #[test]
    fn wrap_agent_log_text_applies_global_column_cap() {
        let wrapped = wrap_agent_log_text(&"x".repeat(130), 200);

        assert_eq!(terminal_text_width(&wrapped[0]), 120);
        assert_eq!(terminal_text_width(&wrapped[1]), 10);
    }
}
