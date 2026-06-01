//! Display overlay composition for terminal rendering.
//!
//! This module owns the transient command/status overlay rows that are layered
//! over an already-rendered client view. It keeps modal overlay pagination and
//! style-span construction together so the parent renderer only chooses when to
//! apply overlays.

use super::*;

/// Runs the compose display overlay lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_display_overlay_lines(
    base_lines: &[String],
    display_lines: &[String],
    client_size: Size,
) -> Vec<String> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
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

/// Runs the compose display overlay line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_display_overlay_line_style_spans(
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    display_lines: &[String],
    client_size: Size,
    ui_theme: &UiTheme,
) -> Vec<Vec<TerminalStyleSpan>> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let mut line_style_spans = normalize_overlay_style_spans(base_line_style_spans, rows, width);
    let start_row = rows.saturating_sub(display_lines.len().max(1));
    for (offset, display_line) in display_lines.iter().enumerate().take(rows) {
        let row = start_row.saturating_add(offset);
        if row < line_style_spans.len() {
            line_style_spans[row].clear();
            let footer_spans =
                agent_live_footer_style_spans(display_line, width, 0, ui_theme, None);
            if footer_spans.is_empty() {
                let length = overlay_text_style_width(display_line, width);
                if length > 0 {
                    line_style_spans[row].push(TerminalStyleSpan {
                        start: 0,
                        length,
                        rendition: display_overlay_text_rendition(ui_theme),
                    });
                }
            } else {
                line_style_spans[row].extend(footer_spans);
            }
        }
    }
    line_style_spans
}

/// Runs the modal display overlay page rows operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn modal_display_overlay_page_rows(client_size: Size) -> usize {
    usize::from(client_size.rows).saturating_sub(2)
}

/// Runs the modal display overlay max scroll operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn modal_display_overlay_max_scroll(display_lines: &[String], client_size: Size) -> usize {
    display_lines
        .len()
        .saturating_sub(modal_display_overlay_page_rows(client_size))
}

/// Runs the compose modal display overlay lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_modal_display_overlay_lines(
    display_lines: &[String],
    client_size: Size,
    scroll_offset: usize,
) -> Vec<String> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    if rows == 0 {
        return Vec::new();
    }
    if rows == 1 {
        return vec![fit_width("esc: return", width)];
    }
    let page_rows = modal_display_overlay_page_rows(client_size);
    let max_scroll = modal_display_overlay_max_scroll(display_lines, client_size);
    let offset = scroll_offset.min(max_scroll);
    let visible_count = display_lines.len().saturating_sub(offset).min(page_rows);
    let start_line = usize::from(visible_count > 0).saturating_add(offset);
    let end_line = offset.saturating_add(visible_count);
    let mut lines = Vec::with_capacity(rows);
    lines.push(fit_width("mezzanine command output", width));
    for line in display_lines.iter().skip(offset).take(page_rows) {
        lines.push(fit_width(line, width));
    }
    while lines.len() < rows.saturating_sub(1) {
        lines.push(" ".repeat(width));
    }
    let footer = format!(
        "esc: return | {start_line}-{end_line}/{} | up/down pgup/pgdn home/end",
        display_lines.len()
    );
    lines.push(fit_width(&footer, width));
    lines
}

/// Runs the compose modal display overlay line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_modal_display_overlay_line_style_spans(
    display_lines: &[String],
    client_size: Size,
    scroll_offset: usize,
    ui_theme: &UiTheme,
) -> Vec<Vec<TerminalStyleSpan>> {
    let width = usize::from(client_size.columns);
    compose_modal_display_overlay_lines(display_lines, client_size, scroll_offset)
        .into_iter()
        .map(|line| {
            let length = overlay_text_style_width(&line, width);
            (length > 0)
                .then_some(TerminalStyleSpan {
                    start: 0,
                    length,
                    rendition: display_overlay_text_rendition(ui_theme),
                })
                .into_iter()
                .collect()
        })
        .collect()
}

/// Returns the rendered text width that should receive overlay foreground
/// styling, excluding padding inserted only to clear the row or region.
pub(crate) fn overlay_text_style_width(value: &str, max_width: usize) -> usize {
    fitted_text_width(value.trim_end_matches(' '), max_width)
}

/// Normalized base text and style-span rows for a terminal overlay canvas.
///
/// Callers can mutate `lines` and `line_style_spans` after this normalization
/// without repeating terminal-size truncation, padding, or span clipping rules.
pub(crate) struct NormalizedOverlayCanvas {
    /// Current terminal width in display cells.
    pub(crate) width: usize,
    /// Current terminal height in rows.
    pub(crate) rows: usize,
    /// Base lines fitted to `width` and padded/truncated to `rows`.
    pub(crate) lines: Vec<String>,
    /// Base style spans clipped to `width` and padded/truncated to `rows`.
    pub(crate) line_style_spans: Vec<Vec<TerminalStyleSpan>>,
}

/// Normalizes base overlay text and style-span rows to the current terminal size.
pub(crate) fn normalize_overlay_canvas(
    base_lines: &[String],
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    client_size: Size,
) -> NormalizedOverlayCanvas {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
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

/// Runs the status line rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn status_line_rendition(
    kind: ClientStatusKind,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    match kind {
        ClientStatusKind::Plain => ui_theme.colors.prompt.rendition(),
        ClientStatusKind::CopyMode
        | ClientStatusKind::PendingObserver
        | ClientStatusKind::Diagnostic => ui_theme.colors.display_overlay.rendition(),
    }
}

/// Runs the normalize overlay style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn normalize_overlay_style_spans(
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    rows: usize,
    width: usize,
) -> Vec<Vec<TerminalStyleSpan>> {
    let mut line_style_spans = base_line_style_spans
        .iter()
        .take(rows)
        .map(|spans| clipped_style_spans(spans, 0, width))
        .collect::<Vec<_>>();
    while line_style_spans.len() < rows {
        line_style_spans.push(Vec::new());
    }
    line_style_spans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the shared overlay canvas normalizer applies the same terminal
    /// size to text rows and style-span rows.
    ///
    /// Retained base rows are fitted to the current terminal width, missing
    /// rows are padded with blank text and empty span rows, and oversized spans
    /// are clipped instead of leaking past the resized overlay canvas.
    #[test]
    fn normalize_overlay_canvas_refits_lines_and_spans_to_client_size() {
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
            Size::new(4, 3).unwrap(),
        );

        assert_eq!(canvas.width, 4);
        assert_eq!(canvas.rows, 3);
        assert_eq!(canvas.lines, vec!["abcd", "    ", "    "]);
        assert_eq!(canvas.line_style_spans.len(), 3);
        assert_eq!(canvas.line_style_spans[0].len(), 1);
        assert_eq!(canvas.line_style_spans[0][0].length, 4);
        assert!(canvas.line_style_spans[1].is_empty());
        assert!(canvas.line_style_spans[2].is_empty());
    }
}
