//! Neutral overlay viewport, search, selection, and style interaction.
//!
//! These functions operate only on mux overlay state, terminal coordinates,
//! and mux theme records. Product command execution and backend refreshes stay
//! in the composition crate.

use mez_terminal::{GraphicRendition, TerminalStyleSpan, terminal_emoji_width, terminal_graphemes};
use unicode_width::UnicodeWidthStr;

use crate::copy::{COPY_SKIP_LINE, COPY_WRAP_CONTINUATION, CopyPosition};
use crate::layout::Size;
use crate::render::{
    char_count, clipped_overlay_style_span, modal_overlay_max_scroll, modal_overlay_page_rows,
    push_or_extend_style_span,
};
use crate::theme::UiTheme;

use super::state::{DisplayOverlay, OverlaySearchMatch, OverlaySelection, OverlaySelectionKind};

/// Selector marker shown in front of the active command-overlay row.
pub const OVERLAY_ACTIVE_SELECTOR: &str = "> ";
/// Placeholder marker shown in front of inactive selectable overlay rows.
pub const OVERLAY_INACTIVE_SELECTOR: &str = "  ";

/// Measures one grapheme under the active terminal compatibility mode.
fn terminal_grapheme_width(grapheme: &str) -> usize {
    mez_terminal::terminal_grapheme_width(grapheme, terminal_emoji_width())
}

/// Returns the rendered line index for the active overlay selection.
pub fn overlay_active_line_index(overlay: &DisplayOverlay<impl Sized>) -> Option<usize> {
    overlay
        .active_selection_index
        .and_then(|index| overlay.selections.get(index))
        .map(|selection| selection.line_index)
}

/// Keeps a target overlay line within the modal page.
pub fn scroll_overlay_to_line(
    overlay: &mut DisplayOverlay<impl Sized>,
    line_index: usize,
    client_size: Size,
) {
    let page_rows = modal_overlay_page_rows(client_size).max(1);
    if line_index < overlay.scroll_offset {
        overlay.scroll_offset = line_index;
    } else if line_index >= overlay.scroll_offset.saturating_add(page_rows) {
        overlay.scroll_offset = line_index.saturating_add(1).saturating_sub(page_rows);
    }
    overlay.scroll_offset = overlay
        .scroll_offset
        .min(modal_overlay_max_scroll(overlay.lines.len(), client_size));
}

/// Clamps overlay scrolling to the visible content range for the client size.
pub fn clamp_overlay_scroll(overlay: &mut DisplayOverlay<impl Sized>, client_size: Size) {
    overlay.scroll_offset = overlay
        .scroll_offset
        .min(modal_overlay_max_scroll(overlay.lines.len(), client_size));
}

/// Returns display overlay lines with selector markers on actionable rows.
pub fn overlay_render_lines(overlay: &DisplayOverlay<impl Sized>) -> Vec<String> {
    let active_line = overlay_active_line_index(overlay);
    let inactive_prefix = (!overlay.selections.is_empty()).then_some(OVERLAY_INACTIVE_SELECTOR);
    overlay
        .lines
        .iter()
        .enumerate()
        .map(|(line_index, line)| {
            if active_line == Some(line_index) {
                format!("{OVERLAY_ACTIVE_SELECTOR}{line}")
            } else if let Some(prefix) = inactive_prefix {
                format!("{prefix}{line}")
            } else {
                line.to_string()
            }
        })
        .collect()
}

/// Returns the rendered start column after selector gutters are added.
pub fn overlay_rendered_selection_start(
    overlay: &DisplayOverlay<impl Sized>,
    selection: &OverlaySelection,
) -> usize {
    selection.start_column + overlay_line_prefix_columns(overlay, selection.line_index)
}

/// Returns the terminal-cell width occupied by one rendered overlay row gutter.
pub fn overlay_line_prefix_columns(
    overlay: &DisplayOverlay<impl Sized>,
    _line_index: usize,
) -> usize {
    usize::from(!overlay.selections.is_empty()) * overlay_selection_prefix_columns()
}

/// Returns the terminal-cell width occupied by selectable overlay row gutters.
pub fn overlay_selection_prefix_columns() -> usize {
    UnicodeWidthStr::width(OVERLAY_ACTIVE_SELECTOR)
}

/// Returns the modal overlay footer with physical-row progress and key hints.
pub fn overlay_footer(overlay: &DisplayOverlay<impl Sized>, size: Size) -> String {
    let page_rows = modal_overlay_page_rows(size);
    let max_scroll = modal_overlay_max_scroll(overlay.lines.len(), size);
    let offset = overlay.scroll_offset.min(max_scroll);
    let visible_count = overlay.lines.len().saturating_sub(offset).min(page_rows);
    let start_line = usize::from(visible_count > 0).saturating_add(offset);
    let end_line = offset.saturating_add(visible_count);
    let navigation = if let Some(input) = overlay.search_input.as_deref() {
        format!("/{input}")
    } else if let Some(status) = overlay.search_status.as_deref() {
        status.to_string()
    } else if overlay
        .record_browser
        .as_ref()
        .is_some_and(|browser| browser.browser.deletion_enabled())
    {
        "esc: back | /: search | enter: open | d: delete | s: save | arrows pgup/pgdn".to_string()
    } else if overlay.record_browser.is_some() {
        "esc: back | /: search | enter: open | a: all | k/p/x: filter | s: save | arrows pgup/pgdn"
            .to_string()
    } else if overlay.selections.is_empty() {
        "esc: return | /: search | up/down pgup/pgdn home/end".to_string()
    } else {
        "esc: return | /: search | enter: select | arrows: choose | pgup/pgdn: scroll".to_string()
    };
    format!(
        "{start_line}-{end_line}/{} | {navigation}",
        overlay.lines.len()
    )
}

/// Returns the themed choice style for a command-overlay selection.
pub fn overlay_selection_rendition(
    ui_theme: &UiTheme,
    kind: OverlaySelectionKind,
    active: bool,
) -> GraphicRendition {
    let pair = match kind {
        OverlaySelectionKind::Primary => ui_theme.colors.agent_model,
        OverlaySelectionKind::Secondary => ui_theme.colors.agent_reasoning,
        OverlaySelectionKind::Danger => ui_theme.colors.agent_status_failed,
    };
    let mut rendition = GraphicRendition {
        foreground: Some(pair.foreground),
        ..GraphicRendition::default()
    };
    rendition.bold = true;
    rendition.underline = true;
    rendition.inverse = false;
    rendition.background = None;
    rendition.dim = false;
    if active {
        rendition.background = Some(pair.background);
    }
    rendition
}
/// Returns the selector-gutter rendition for a selectable overlay row.
///
/// The gutter marks the active row, but it is not part of the selectable body
/// range. Keep the selector glyph itself unstyled so active link treatment
/// begins only on the first body cell; otherwise front-of-line `/resume` links
/// visibly shift left into the selector prefix even when the body/background
/// math is correct.
pub fn overlay_selection_gutter_rendition(
    _ui_theme: &UiTheme,
    _kind: OverlaySelectionKind,
) -> GraphicRendition {
    GraphicRendition::default()
}
/// Returns the markdown-style rendition used for command-overlay links.
pub fn overlay_link_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(ui_theme.colors.agent_transcript_command.foreground),
        bold: true,
        underline: true,
        inverse: false,
        background: None,
        ..GraphicRendition::default()
    }
}
/// Returns the shifted, clipped markdown/body spans for one overlay line.
pub fn overlay_body_style_spans(
    overlay: &DisplayOverlay<impl Sized>,
    line_index: usize,
    max_columns: usize,
) -> Vec<TerminalStyleSpan> {
    let prefix_columns = overlay_line_prefix_columns(overlay, line_index);
    let visible_columns = max_columns.saturating_sub(prefix_columns);
    overlay
        .line_style_spans
        .get(line_index)
        .into_iter()
        .flatten()
        .filter_map(|span| clipped_overlay_style_span(*span, prefix_columns, visible_columns))
        .collect()
}
/// Appends one selection rendition only where later body spans do not apply.
fn append_uncovered_overlay_selection_span(
    spans: &mut Vec<TerminalStyleSpan>,
    selection_start: usize,
    selection_length: usize,
    rendition: GraphicRendition,
    occupied_spans: &[TerminalStyleSpan],
) {
    let selection_end = selection_start.saturating_add(selection_length);
    if selection_start >= selection_end {
        return;
    }
    let mut occupied_ranges: Vec<(usize, usize)> = occupied_spans
        .iter()
        .filter_map(|span| {
            let span_start = span.start.max(selection_start);
            let span_end = span.start.saturating_add(span.length).min(selection_end);
            (span_start < span_end).then_some((span_start, span_end))
        })
        .collect();
    occupied_ranges.sort_unstable_by_key(|(start, _)| *start);
    let mut cursor = selection_start;
    for (occupied_start, occupied_end) in occupied_ranges {
        if cursor < occupied_start {
            push_or_extend_style_span(
                spans,
                TerminalStyleSpan {
                    start: cursor,
                    length: occupied_start.saturating_sub(cursor),
                    rendition,
                },
            );
        }
        cursor = cursor.max(occupied_end);
        if cursor >= selection_end {
            return;
        }
    }
    push_or_extend_style_span(
        spans,
        TerminalStyleSpan {
            start: cursor,
            length: selection_end.saturating_sub(cursor),
            rendition,
        },
    );
}

/// Appends one style span without coalescing it into an adjacent span.
///
/// Overlay selection gutters must remain a standalone cell so later body or
/// fallback selection styling cannot visually absorb the gutter when adjacent
/// rendered spans share the same rendition.
fn push_style_span_without_coalescing(spans: &mut Vec<TerminalStyleSpan>, span: TerminalStyleSpan) {
    if span.length == 0 {
        return;
    }
    spans.push(span);
}

/// Appends active-selection backgrounds over body spans inside a selected range.
fn append_active_overlay_body_selection_spans(
    spans: &mut Vec<TerminalStyleSpan>,
    selection_start: usize,
    selection_length: usize,
    selection_rendition: GraphicRendition,
    body_spans: &[TerminalStyleSpan],
) {
    let selection_end = selection_start.saturating_add(selection_length);
    if selection_start >= selection_end {
        return;
    }
    for body_span in body_spans {
        let body_start = body_span.start.max(selection_start);
        let body_end = body_span
            .start
            .saturating_add(body_span.length)
            .min(selection_end);
        if body_start >= body_end {
            continue;
        }
        let mut rendition = body_span.rendition;
        rendition.background = selection_rendition.background;
        if rendition.foreground.is_none() {
            rendition.foreground = selection_rendition.foreground;
        }
        push_style_span_without_coalescing(
            spans,
            TerminalStyleSpan {
                start: body_start,
                length: body_end.saturating_sub(body_start),
                rendition,
            },
        );
    }
}
/// Returns the fully composed style spans for one rendered overlay line.
pub fn overlay_rendered_line_style_spans(
    overlay: &DisplayOverlay<impl Sized>,
    line_index: usize,
    max_columns: usize,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    let body_spans = overlay_body_style_spans(overlay, line_index, max_columns);
    let prefix_columns = overlay_line_prefix_columns(overlay, line_index);
    let mut spans = Vec::new();
    let search_span = overlay.search_match.and_then(|search_match| {
        if search_match.line_index != line_index || search_match.width == 0 {
            return None;
        }
        let start = prefix_columns.saturating_add(search_match.start_column);
        if start >= max_columns {
            return None;
        }
        Some(TerminalStyleSpan {
            start,
            length: search_match.width.min(max_columns.saturating_sub(start)),
            rendition: ui_theme.colors.copy_selection.rendition(),
        })
    });
    for (selection_index, selection) in overlay.selections.iter().enumerate() {
        if selection.line_index != line_index {
            continue;
        }
        let active = overlay.active_selection_index == Some(selection_index);
        let start = overlay_rendered_selection_start(overlay, selection);
        if start < max_columns && selection.width > 0 {
            append_uncovered_overlay_selection_span(
                &mut spans,
                start,
                selection.width.min(max_columns.saturating_sub(start)),
                overlay_selection_rendition(ui_theme, selection.kind, active),
                &body_spans,
            );
        }
        if active {
            push_style_span_without_coalescing(
                &mut spans,
                TerminalStyleSpan {
                    start: 0,
                    length: prefix_columns.min(max_columns),
                    rendition: overlay_selection_gutter_rendition(ui_theme, selection.kind),
                },
            );
        }
    }
    for span in &body_spans {
        push_or_extend_style_span(&mut spans, *span);
    }
    for (selection_index, selection) in overlay.selections.iter().enumerate() {
        if selection.line_index != line_index
            || overlay.active_selection_index != Some(selection_index)
        {
            continue;
        }
        let start = overlay_rendered_selection_start(overlay, selection);
        if start < max_columns && selection.width > 0 {
            append_active_overlay_body_selection_spans(
                &mut spans,
                start,
                selection.width.min(max_columns.saturating_sub(start)),
                overlay_selection_rendition(ui_theme, selection.kind, true),
                &body_spans,
            );
        }
    }
    if let Some(search_span) = search_span {
        push_or_extend_style_span(&mut spans, search_span);
    }
    append_display_overlay_mouse_selection_spans(
        &mut spans,
        overlay.mouse_selection,
        line_index,
        prefix_columns,
        max_columns,
        ui_theme.colors.copy_selection.rendition(),
    );
    spans
}

/// Appends copy-selection style spans for one rendered overlay content row.
fn append_display_overlay_mouse_selection_spans(
    spans: &mut Vec<TerminalStyleSpan>,
    selection: Option<(CopyPosition, CopyPosition)>,
    line_index: usize,
    prefix_columns: usize,
    max_columns: usize,
    rendition: GraphicRendition,
) {
    let Some((start, end)) = selection else {
        return;
    };
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if line_index < start.line || line_index > end.line {
        return;
    }
    let content_start = if line_index == start.line {
        start.column
    } else {
        0
    };
    let content_end = if line_index == end.line {
        end.column
    } else {
        max_columns.saturating_sub(prefix_columns)
    };
    let rendered_start = prefix_columns
        .saturating_add(content_start)
        .min(max_columns);
    let rendered_end = prefix_columns.saturating_add(content_end).min(max_columns);
    if rendered_start >= rendered_end {
        return;
    }
    push_or_extend_style_span(
        spans,
        TerminalStyleSpan {
            start: rendered_start,
            length: rendered_end.saturating_sub(rendered_start),
            rendition,
        },
    );
}

/// Computes terminal placement for a pane agent model/reasoning selector.
/// Copies the currently selected primary display-overlay text.
pub fn overlay_copy_selection(overlay: &DisplayOverlay<impl Sized>) -> Option<String> {
    let (start, end) = overlay.mouse_selection?;
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if start.line == end.line {
        let line = overlay.lines.get(start.line)?;
        if start.column == 0 && end.column >= char_count(line) {
            return overlay_source_copy_line(overlay, start.line)
                .or_else(|| Some(overlay_line_slice(line, start.column, end.column)));
        }
        return Some(overlay_line_slice(line, start.column, end.column));
    }
    let mut copied = Vec::new();
    let first = overlay.lines.get(start.line)?;
    if start.column == 0 {
        if let Some(copy_line) = overlay_source_copy_line(overlay, start.line) {
            copied.push(copy_line);
        }
    } else {
        copied.push(overlay_line_slice(first, start.column, char_count(first)));
    }
    for line_index in start.line.saturating_add(1)..end.line {
        if let Some(copy_line) = overlay_source_copy_line(overlay, line_index) {
            copied.push(copy_line);
        }
    }
    let last = overlay.lines.get(end.line)?;
    if end.column >= char_count(last) {
        if let Some(copy_line) = overlay_source_copy_line(overlay, end.line) {
            copied.push(copy_line);
        }
    } else {
        copied.push(overlay_line_slice(last, 0, end.column));
    }
    Some(copied.join("\n"))
}

/// Returns source copy text for one fully selected overlay row.
fn overlay_source_copy_line(
    overlay: &DisplayOverlay<impl Sized>,
    line_index: usize,
) -> Option<String> {
    match overlay
        .line_copy_texts
        .get(line_index)
        .and_then(|copy_text| copy_text.as_deref())
    {
        Some(COPY_SKIP_LINE | COPY_WRAP_CONTINUATION) => None,
        Some(copy_text) => Some(copy_text.to_string()),
        None => overlay.lines.get(line_index).cloned(),
    }
}

/// Applies a signed scroll delta to a display overlay and clamps the viewport.
pub fn apply_overlay_scroll_delta(
    overlay: &mut DisplayOverlay<impl Sized>,
    delta: isize,
    size: Size,
) -> bool {
    let previous = overlay.scroll_offset;
    if delta.is_negative() {
        overlay.scroll_offset = overlay.scroll_offset.saturating_sub(delta.unsigned_abs());
    } else {
        overlay.scroll_offset = overlay
            .scroll_offset
            .saturating_add(usize::try_from(delta).unwrap_or(usize::MAX));
    }
    clamp_overlay_scroll(overlay, size);
    update_overlay_active_selection_for_viewport(overlay, size);
    previous != overlay.scroll_offset
}

/// Returns whether one overlay selection is currently visible in the viewport.
pub fn overlay_selection_index_is_visible(
    overlay: &DisplayOverlay<impl Sized>,
    selection_index: usize,
    size: Size,
) -> bool {
    let Some(selection) = overlay.selections.get(selection_index) else {
        return false;
    };
    let page_rows = modal_overlay_page_rows(size).max(1);
    let visible_start = overlay.scroll_offset;
    let visible_end = visible_start.saturating_add(page_rows);
    selection.line_index >= visible_start && selection.line_index < visible_end
}

/// Keeps the active overlay selection executable only when it is visible.
pub fn update_overlay_active_selection_for_viewport(
    overlay: &mut DisplayOverlay<impl Sized>,
    size: Size,
) {
    if overlay.selections.is_empty() {
        overlay.active_selection_index = None;
        return;
    }
    if overlay
        .active_selection_index
        .is_some_and(|selection_index| {
            overlay_selection_index_is_visible(overlay, selection_index, size)
        })
    {
        return;
    }
    let page_rows = modal_overlay_page_rows(size).max(1);
    let visible_start = overlay.scroll_offset;
    let visible_end = visible_start.saturating_add(page_rows);
    overlay.active_selection_index = overlay.selections.iter().position(|selection| {
        selection.line_index >= visible_start && selection.line_index < visible_end
    });
}

/// Returns one display-column slice from a primary display-overlay line.
pub fn overlay_line_slice(line: &str, start: usize, end: usize) -> String {
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

/// Returns the overlay selection index under a mouse position.
pub fn overlay_selection_index_at_position(
    overlay: &DisplayOverlay<impl Sized>,
    line_index: usize,
    column: usize,
) -> Option<usize> {
    overlay
        .selections
        .iter()
        .enumerate()
        .filter(|(_, selection)| selection.line_index == line_index)
        .find(|(_, selection)| {
            let start = overlay_rendered_selection_start(overlay, selection);
            let end = start.saturating_add(selection.width);
            column >= start && column < end
        })
        .map(|(index, _)| index)
}

/// Returns the next forward pager-search match, wrapping once to the start.
pub fn overlay_next_search_match(
    overlay: &DisplayOverlay<impl Sized>,
    query: &str,
    current_line: usize,
) -> Option<OverlaySearchMatch> {
    if query.is_empty() || overlay.lines.is_empty() {
        return None;
    }
    let start = current_line.saturating_add(1).min(overlay.lines.len());
    overlay.lines[start..]
        .iter()
        .enumerate()
        .find_map(|(index, line)| {
            overlay_search_match_on_line(line, query, start.saturating_add(index))
        })
        .or_else(|| {
            overlay.lines[..start]
                .iter()
                .enumerate()
                .find_map(|(index, line)| overlay_search_match_on_line(line, query, index))
        })
}

/// Returns the render-cell range for a query match on one pager line.
pub fn overlay_search_match_on_line(
    line: &str,
    query: &str,
    line_index: usize,
) -> Option<OverlaySearchMatch> {
    let byte_start = line.find(query)?;
    let byte_end = byte_start.saturating_add(query.len());
    Some(OverlaySearchMatch {
        line_index,
        start_column: UnicodeWidthStr::width(&line[..byte_start]),
        width: UnicodeWidthStr::width(&line[byte_start..byte_end]),
    })
}

/// Replaces a fixed-width region of a rendered line with overlay text.
pub fn overlay_text_at(line: &mut String, column: usize, width: usize, text: &str) {
    let mut cells = line.chars().collect::<Vec<_>>();
    let required = column.saturating_add(width);
    if cells.len() < required {
        cells.resize(required, ' ');
    }
    for (offset, ch) in text.chars().take(width).enumerate() {
        if let Some(cell) = cells.get_mut(column.saturating_add(offset)) {
            *cell = ch;
        }
    }
    *line = cells.into_iter().collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::default_ui_theme;

    /// Builds neutral overlay state for interaction tests.
    fn overlay(lines: &[&str]) -> DisplayOverlay<()> {
        DisplayOverlay {
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
            line_style_spans: vec![Vec::new(); lines.len()],
            line_copy_texts: vec![None; lines.len()],
            scroll_offset: 0,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            selections: Vec::new(),
            active_selection_index: None,
            dismiss_on_any_input: false,
            record_browser: None,
        }
    }

    /// Verifies forward search wraps and reports terminal-cell coordinates.
    #[test]
    fn overlay_search_wraps_across_content() {
        let overlay = overlay(&["target first", "middle", "target last"]);
        assert_eq!(
            overlay_next_search_match(&overlay, "target", 2),
            Some(OverlaySearchMatch {
                line_index: 0,
                start_column: 0,
                width: 6,
            })
        );
    }

    /// Verifies scrolling selects only actions visible in the current page.
    #[test]
    fn overlay_scroll_keeps_active_selection_in_viewport() {
        let mut overlay = overlay(&["zero", "one", "two", "three"]);
        overlay.selections = vec![
            OverlaySelection {
                line_index: 0,
                start_column: 0,
                width: 4,
                command: "zero".to_string(),
                kind: OverlaySelectionKind::Primary,
            },
            OverlaySelection {
                line_index: 3,
                start_column: 0,
                width: 5,
                command: "three".to_string(),
                kind: OverlaySelectionKind::Primary,
            },
        ];
        overlay.active_selection_index = Some(0);
        let size = Size::new(20, 3).unwrap();
        assert!(apply_overlay_scroll_delta(&mut overlay, 3, size));
        assert_eq!(overlay.active_selection_index, Some(1));
    }

    /// Verifies overlay footers count the physical rows used by scrolling and
    /// pagination before appending interaction hints.
    #[test]
    fn overlay_footer_reports_visible_physical_row_range() {
        let mut overlay = overlay(&["one", "two", "three", "four"]);
        overlay.scroll_offset = 1;

        let footer = overlay_footer(&overlay, Size::new(20, 4).unwrap());

        assert!(footer.starts_with("2-3/4 | "), "{footer}");
        assert!(footer.contains("esc: return"), "{footer}");
    }

    /// Verifies multiline mouse selections produce bounded copied text.
    #[test]
    fn overlay_copy_selection_uses_display_columns() {
        let mut overlay = overlay(&["alpha", "beta", "gamma"]);
        overlay.mouse_selection = Some((
            CopyPosition { line: 0, column: 2 },
            CopyPosition { line: 2, column: 3 },
        ));
        assert_eq!(
            overlay_copy_selection(&overlay).as_deref(),
            Some("pha\nbeta\ngam")
        );
    }

    /// Verifies full-row overlay selections copy raw Markdown once while
    /// presentation-only wrap continuations remain absent from copied text.
    #[test]
    fn overlay_copy_selection_uses_source_text_for_wrapped_markdown_rows() {
        let mut overlay = overlay(&["rendered first", "continued", "rendered second"]);
        overlay.line_copy_texts = vec![
            Some("**raw first source**".to_string()),
            Some(COPY_WRAP_CONTINUATION.to_string()),
            Some("`raw second source`".to_string()),
        ];
        overlay.mouse_selection = Some((
            CopyPosition { line: 0, column: 0 },
            CopyPosition {
                line: 2,
                column: "rendered second".len(),
            },
        ));

        assert_eq!(
            overlay_copy_selection(&overlay).as_deref(),
            Some("**raw first source**\n`raw second source`")
        );
    }

    /// Verifies active selection style layers preserve a neutral selector gutter.
    #[test]
    fn overlay_style_layering_keeps_gutter_separate() {
        let mut overlay = overlay(&["select"]);
        overlay.selections.push(OverlaySelection {
            line_index: 0,
            start_column: 0,
            width: 6,
            command: "select".to_string(),
            kind: OverlaySelectionKind::Primary,
        });
        overlay.active_selection_index = Some(0);
        let spans = overlay_rendered_line_style_spans(&overlay, 0, 20, &default_ui_theme());
        assert!(spans.iter().any(|span| span.start == 0 && span.length == 2));
        assert!(spans.iter().any(|span| span.start == 2 && span.length == 6));
    }
}
