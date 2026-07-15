//! Dependency-neutral transient overlay composition.
//!
//! This module owns fixed-size overlay canvas normalization, bottom-aligned
//! transient row placement, and modal pagination. Callers provide product text
//! such as titles and key hints, plus any theme-derived style policy.

use mez_terminal::{TerminalSize, TerminalStyleSpan};

use super::{clip_style_spans, fit_width, fitted_text_width};

/// Normalized text and style rows for a fixed terminal overlay canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedOverlayCanvas {
    /// Current terminal width in display cells.
    pub width: usize,
    /// Current terminal height in rows.
    pub rows: usize,
    /// Base lines fitted to `width` and padded or truncated to `rows`.
    pub lines: Vec<String>,
    /// Base style spans clipped to `width` and padded or truncated to `rows`.
    pub line_style_spans: Vec<Vec<TerminalStyleSpan>>,
}

/// Normalizes base overlay text and style rows to one terminal size.
pub fn normalize_overlay_canvas(
    base_lines: &[String],
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    size: TerminalSize,
) -> NormalizedOverlayCanvas {
    let width = usize::from(size.columns);
    let rows = usize::from(size.rows);
    NormalizedOverlayCanvas {
        width,
        rows,
        lines: normalize_overlay_lines(base_lines, rows, width),
        line_style_spans: normalize_overlay_style_spans(base_line_style_spans, rows, width),
    }
}

/// Fits base overlay text rows to a fixed terminal canvas.
fn normalize_overlay_lines(base_lines: &[String], rows: usize, width: usize) -> Vec<String> {
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
    lines
}

/// Clips and pads style rows to a fixed terminal canvas.
pub fn normalize_overlay_style_spans(
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    rows: usize,
    width: usize,
) -> Vec<Vec<TerminalStyleSpan>> {
    let mut line_style_spans = base_line_style_spans
        .iter()
        .take(rows)
        .map(|spans| clip_style_spans(spans, 0, width))
        .collect::<Vec<_>>();
    while line_style_spans.len() < rows {
        line_style_spans.push(Vec::new());
    }
    line_style_spans
}

/// Composes bottom-aligned display rows over a normalized base canvas.
pub fn compose_bottom_overlay_lines(
    base_lines: &[String],
    display_lines: &[String],
    size: TerminalSize,
) -> Vec<String> {
    let width = usize::from(size.columns);
    let rows = usize::from(size.rows);
    let mut lines = normalize_overlay_lines(base_lines, rows, width);
    let start_row = rows.saturating_sub(display_lines.len().max(1));
    for (offset, line) in display_lines.iter().take(rows).enumerate() {
        let row = start_row.saturating_add(offset);
        if row < lines.len() {
            lines[row] = fit_width(line, width);
        }
    }
    lines
}

/// Returns the rows available for modal content between header and footer.
pub fn modal_overlay_page_rows(size: TerminalSize) -> usize {
    usize::from(size.rows).saturating_sub(2)
}

/// Returns the greatest valid modal scroll offset.
pub fn modal_overlay_max_scroll(display_line_count: usize, size: TerminalSize) -> usize {
    display_line_count.saturating_sub(modal_overlay_page_rows(size))
}

/// Composes one exact-size modal overlay from caller-provided product labels.
pub fn compose_modal_overlay_lines(
    display_lines: &[String],
    size: TerminalSize,
    scroll_offset: usize,
    title: &str,
    single_row_hint: &str,
    footer_prefix: &str,
    navigation_hint: &str,
) -> Vec<String> {
    let width = usize::from(size.columns);
    let rows = usize::from(size.rows);
    if rows == 0 {
        return Vec::new();
    }
    if rows == 1 {
        return vec![fit_width(single_row_hint, width)];
    }
    let page_rows = modal_overlay_page_rows(size);
    let max_scroll = modal_overlay_max_scroll(display_lines.len(), size);
    let offset = scroll_offset.min(max_scroll);
    let visible_count = display_lines.len().saturating_sub(offset).min(page_rows);
    let start_line = usize::from(visible_count > 0).saturating_add(offset);
    let end_line = offset.saturating_add(visible_count);
    let mut lines = Vec::with_capacity(rows);
    lines.push(fit_width(title, width));
    for line in display_lines.iter().skip(offset).take(page_rows) {
        lines.push(fit_width(line, width));
    }
    while lines.len() < rows.saturating_sub(1) {
        lines.push(" ".repeat(width));
    }
    let footer = format!(
        "{footer_prefix}{start_line}-{end_line}/{} | {navigation_hint}",
        display_lines.len()
    );
    lines.push(fit_width(&footer, width));
    lines
}

/// Returns the non-padding display width that should receive overlay styling.
pub fn overlay_text_style_width(value: &str, max_width: usize) -> usize {
    fitted_text_width(value.trim_end_matches(' '), max_width)
}

#[cfg(test)]
mod tests {
    use mez_terminal::{GraphicRendition, TerminalStyleSpan};

    use super::*;

    /// Verifies normalization applies one authoritative terminal size to text
    /// and style rows, clipping oversized spans and padding missing rows.
    #[test]
    fn overlay_canvas_normalization_refits_text_and_styles() {
        let canvas = normalize_overlay_canvas(
            &["abcdef".to_string()],
            &[vec![TerminalStyleSpan {
                start: 0,
                length: 99,
                rendition: GraphicRendition {
                    inverse: true,
                    ..GraphicRendition::default()
                },
            }]],
            TerminalSize::new(4, 3).unwrap(),
        );

        assert_eq!(canvas.lines, vec!["abcd", "    ", "    "]);
        assert_eq!(canvas.line_style_spans.len(), 3);
        assert_eq!(canvas.line_style_spans[0][0].length, 4);
        assert!(canvas.line_style_spans[1].is_empty());
    }

    /// Verifies modal pagination keeps product labels injected while mux-owned
    /// clipping, scrolling, and exact-height padding remain deterministic.
    #[test]
    fn modal_overlay_composition_pages_with_caller_labels() {
        let lines = compose_modal_overlay_lines(
            &["one".into(), "two".into(), "three".into()],
            TerminalSize::new(18, 4).unwrap(),
            1,
            "output",
            "esc",
            "esc | ",
            "up/down",
        );

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "output            ");
        assert_eq!(lines[1], "two               ");
        assert_eq!(lines[2], "three             ");
        assert_eq!(lines[3], "esc | 2-3/3 | up/d");
    }
}
