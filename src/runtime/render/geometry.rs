//! Low-level render geometry helpers.
//!
//! This module owns terminal-cell overlay and style-span clipping helpers used
//! by runtime render surfaces. The helpers are intentionally small and
//! side-effect-free so higher-level overlay and pane render code can share the
//! same clipping behavior.

use super::super::runtime_fit_status_line;
use crate::terminal::TerminalStyleSpan;

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
    let mut cells = row.chars().collect::<Vec<_>>();
    let required = column_start.saturating_add(columns);
    while cells.len() < required {
        cells.push(' ');
    }
    let fitted = runtime_fit_status_line(text, columns);
    for (offset, ch) in fitted.chars().take(columns).enumerate() {
        cells[column_start.saturating_add(offset)] = ch;
    }
    *row = cells.into_iter().collect();
}

/// Removes style spans that overlap a fixed-width terminal-cell range.
///
/// # Parameters
/// - `spans`: Style spans for one rendered row.
/// - `column_start`: The first terminal column being replaced.
/// - `columns`: The number of replaced terminal cells.
pub(super) fn remove_overlapping_style_spans(
    spans: &mut Vec<TerminalStyleSpan>,
    column_start: usize,
    columns: usize,
) {
    let column_end = column_start.saturating_add(columns);
    spans.retain(|span| {
        let span_end = span.start.saturating_add(span.length);
        span_end <= column_start || span.start >= column_end
    });
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

pub(super) fn range_overlap_u16(
    first_start: u16,
    first_end: u16,
    second_start: u16,
    second_end: u16,
) -> u16 {
    first_end
        .min(second_end)
        .saturating_sub(first_start.max(second_start))
}
