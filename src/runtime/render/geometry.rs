//! Low-level render geometry helpers.
//!
//! This module owns terminal-cell overlay and style-span clipping helpers used
//! by runtime render surfaces. The helpers are intentionally small and
//! side-effect-free so higher-level overlay and pane render code can share the
//! same clipping behavior.

use super::super::runtime_fit_status_line;
use crate::terminal::{terminal_grapheme_width, terminal_graphemes};
use mez_terminal::TerminalStyleSpan;

/// Replaces a fixed-width terminal-cell range with text.
///
/// # Parameters
/// - `row`: The rendered row being mutated.
/// - `column_start`: The first terminal column to replace.
/// - `columns`: The number of terminal cells reserved for replacement.
/// - `text`: The replacement text, clipped by terminal-cell width.
pub(super) fn overlay_text_cells(
    row: &mut String,
    column_start: usize,
    columns: usize,
    text: &str,
) {
    if columns == 0 {
        return;
    }
    let target_end = column_start.saturating_add(columns);
    let fitted = runtime_fit_status_line(text, columns);

    let mut output = String::new();
    let mut current_column = 0usize;
    let mut inserted = false;

    for grapheme in terminal_graphemes(row) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        let next_column = current_column.saturating_add(grapheme_width);

        if next_column <= column_start {
            output.push_str(grapheme);
        } else if !inserted {
            let reached = crate::terminal::terminal_text_width(&output);
            if reached < column_start {
                output.push_str(&" ".repeat(column_start.saturating_sub(reached)));
            }
            output.push_str(&fitted);
            inserted = true;
        }

        if current_column >= target_end {
            output.push_str(grapheme);
        }

        current_column = next_column;
    }

    if !inserted {
        let reached = crate::terminal::terminal_text_width(&output);
        if reached < column_start {
            output.push_str(&" ".repeat(column_start.saturating_sub(reached)));
        }
        output.push_str(&fitted);
    }

    *row = output;
}

/// Clips one style span into a destination overlay range.
///
/// # Parameters
/// - `span`: Source style span relative to overlay-local text.
/// - `column_start`: Destination column where overlay text begins.
/// - `columns`: Number of cells available for the overlay text.
pub(super) fn clipped_overlay_style_span(
    span: TerminalStyleSpan,
    column_start: usize,
    columns: usize,
) -> Option<TerminalStyleSpan> {
    let start = span.start.min(columns);
    let end = span.start.saturating_add(span.length).min(columns);
    (end > start).then(|| TerminalStyleSpan {
        start: column_start.saturating_add(start),
        length: end.saturating_sub(start),
        rendition: span.rendition,
    })
}

#[cfg(test)]
mod tests {
    use crate::terminal::terminal_text_width;

    use super::overlay_text_cells;

    /// Verifies that overlay with only single-width characters preserves the
    /// correct column alignment when wide characters exist before the target.
    #[test]
    fn overlay_preserves_wide_chars_before_target() {
        let mut row = String::from("ＡＢＣ"); // 3 fullwidth chars = 6 display cols
        overlay_text_cells(&mut row, 4, 2, "XY");
        // A at cols 0-2, B at 2-4, C at 4-6
        // Replace cols 4-6 with "XY" (2 single-width = 2 display cols)
        // Result: A(0-2) + B(2-4) + "XY"(4-6) = "ＡＢXY"
        assert_eq!(row, "ＡＢXY");
        assert_eq!(terminal_text_width(&row), 6);
    }

    /// Verifies that overlay correctly replaces content when no wide characters
    /// are present, using display-column positions for both input and output.
    #[test]
    fn overlay_replaces_single_width_content() {
        let mut row = String::from("hello world");
        overlay_text_cells(&mut row, 6, 5, "there");
        assert_eq!(row, "hello there");
        assert_eq!(terminal_text_width(&row), 11);
    }

    /// Verifies that an overlay spanning zero columns is a no-op even when the
    /// input contains wide characters.
    #[test]
    fn overlay_zero_columns_is_noop() {
        let mut row = String::from("ＡＢ");
        overlay_text_cells(&mut row, 0, 0, "X");
        assert_eq!(row, "ＡＢ");
        assert_eq!(terminal_text_width(&row), 4);
    }

    /// Verifies that overlay replaces a full-width character range and pads the
    /// replacement to maintain the original total display width.
    #[test]
    fn overlay_replaces_wide_chars_with_narrow_text() {
        // 3 fullwidth chars = 6 display cols
        let mut row = String::from("ＡＢＣ");
        overlay_text_cells(&mut row, 0, 6, "ab");
        // "ab" (2 display cols) fitted to 6 → "ab    " (4 padding spaces)
        assert_eq!(terminal_text_width(&row), 6);
        assert!(row.starts_with("ab"));
    }

    /// Verifies that overlay at the end of a row with wide characters positions
    /// correctly when the target starts after all existing content.
    #[test]
    fn overlay_after_row_end_pads_correctly() {
        let mut row = String::from("Ａ"); // 2 display cols
        overlay_text_cells(&mut row, 4, 2, "XY");
        // A at 0-2, then pad 2 spaces to reach col 4, then "XY"
        assert!(row.starts_with('Ａ'));
        assert_eq!(terminal_text_width(&row), 6);
    }
}
