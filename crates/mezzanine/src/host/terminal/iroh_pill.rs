//! Client-local Iroh status composition shared by terminal transport adapters.
//!
//! Preserves the established attach cell slicing and style replacement contract.
//! The server-owned base remains immutable; only its optional semantic slot is
//! decorated in a returned copy. Callers supply connection/quality evidence and
//! validate slot geometry and enclosing allocation budgets. This component does
//! not sample health, authorize input, commit output, or acknowledge receipts.

use super::{TerminalIrohStatusQuality, TerminalIrohStatusSlot};
use mez_terminal::TerminalStyleSpan;

/// Decorates a copy of the retained frame using the existing fixed up/down pill
/// and its server-theme rendition. Absent or missing-row slots leave it intact.
pub(crate) fn compose(
    base_lines: &[String],
    base_spans: &[Vec<TerminalStyleSpan>],
    slot: Option<TerminalIrohStatusSlot>,
    connected: bool,
    quality: TerminalIrohStatusQuality,
) -> (Vec<String>, Vec<Vec<TerminalStyleSpan>>) {
    let mut lines = base_lines.to_vec();
    let mut spans = base_spans.to_vec();
    let Some(slot) = slot else {
        return (lines, spans);
    };
    let Some(line) = lines.get_mut(slot.row) else {
        return (lines, spans);
    };
    let label = if connected { " up " } else { " dn " };
    let prefix = mez_mux::render::line_slice(line, 0, slot.column);
    let suffix =
        mez_mux::render::line_slice(line, slot.column.saturating_add(slot.width), usize::MAX);
    *line = format!(
        "{prefix}{}{suffix}",
        mez_mux::render::fit_width(label, slot.width)
    );
    spans.resize(lines.len(), Vec::new());
    let row_spans = &mut spans[slot.row];
    *row_spans = row_spans
        .iter()
        .flat_map(|span| {
            let slot_end = slot.column.saturating_add(slot.width);
            if mez_mux::render::style_span_overlaps_columns(*span, slot.column, slot_end) {
                mez_mux::render::style_span_segments_outside_range(*span, slot.column, slot_end)
            } else {
                vec![*span]
            }
        })
        .collect();
    let rendition = if connected {
        match quality {
            TerminalIrohStatusQuality::Good => slot.good,
            TerminalIrohStatusQuality::Degraded => slot.degraded,
            TerminalIrohStatusQuality::Poor => slot.poor,
            TerminalIrohStatusQuality::Unknown => slot.unknown,
        }
    } else {
        slot.unknown
    };
    row_spans.push(TerminalStyleSpan {
        start: slot.column,
        length: slot.width,
        rendition,
    });
    row_spans.sort_unstable_by_key(|span| span.start);
    (lines, spans)
}

#[cfg(test)]
mod tests;
