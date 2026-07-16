//! Display overlay composition for terminal rendering.
//!
//! This module owns the transient command/status overlay rows that are layered
//! over an already-rendered client view. It keeps modal overlay pagination and
//! style-span construction together so the parent renderer only chooses when to
//! apply overlays.

#[cfg(test)]
use super::{
    TerminalStyleSpan, UiTheme, agent_live_footer_style_spans, display_overlay_text_rendition,
};
#[cfg(test)]
use mez_mux::render::{normalize_overlay_style_spans, overlay_text_style_width};

use super::Size;

/// Runs the compose display overlay line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
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
    mez_mux::render::compose_modal_overlay_lines(
        display_lines,
        client_size,
        scroll_offset,
        "mezzanine command output",
        "esc: return",
        "esc: return | ",
        "up/down pgup/pgdn home/end",
    )
}

/// Runs the compose modal display overlay line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
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
