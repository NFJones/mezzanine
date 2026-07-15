//! Dependency-neutral terminal canvas and styled-row composition.
//!
//! This module owns display-cell storage, Unicode-width-aware text placement,
//! terminal-row fitting and slicing, and style-span clipping/overlay behavior.
//! It deliberately excludes product themes, agent transcript wrapping, prompt
//! policy, overlays, and host terminal encoding.

use mez_terminal::{
    GraphicRendition, TerminalStyleSpan, TerminalStyledLine, terminal_emoji_width,
    terminal_grapheme_width, terminal_graphemes, terminal_text_width,
};

use crate::layout::PaneGeometry;
use crate::presentation::{PaneDividerCell, TerminalFrameStyle, pane_divider_cells};

/// One display-cell slot in a mux-owned render canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRenderCell {
    text: String,
    continuation: bool,
}

impl TerminalRenderCell {
    /// Builds one leading render cell containing a single glyph.
    pub fn from_char(ch: char) -> Self {
        Self {
            text: ch.to_string(),
            continuation: false,
        }
    }

    /// Builds one leading render cell containing a complete grapheme cluster.
    pub fn from_grapheme(grapheme: &str) -> Self {
        Self {
            text: grapheme.to_string(),
            continuation: false,
        }
    }

    /// Builds one continuation cell for a multi-column grapheme cluster.
    pub fn continuation() -> Self {
        Self {
            text: String::new(),
            continuation: true,
        }
    }
}

/// Builds one render-canvas row initialized to the requested fill glyph.
pub fn blank_render_row(columns: usize, fill: char) -> Vec<TerminalRenderCell> {
    vec![TerminalRenderCell::from_char(fill); columns]
}

/// Builds a render-canvas matrix initialized to the requested fill glyph.
pub fn blank_render_cells(rows: usize, columns: usize, fill: char) -> Vec<Vec<TerminalRenderCell>> {
    (0..rows).map(|_| blank_render_row(columns, fill)).collect()
}

/// Writes one single-width cell while removing any overlapping wide glyph.
pub fn write_single_width_cell(row: &mut [TerminalRenderCell], column: usize, glyph: char) {
    if column >= row.len() {
        return;
    }
    if row[column].continuation {
        let mut left = column;
        while left > 0 && row[left].continuation {
            row[left] = TerminalRenderCell::from_char(' ');
            left = left.saturating_sub(1);
        }
        row[left] = TerminalRenderCell::from_char(' ');
    }
    let mut right = column.saturating_add(1);
    while right < row.len() && row[right].continuation {
        row[right] = TerminalRenderCell::from_char(' ');
        right = right.saturating_add(1);
    }
    row[column] = TerminalRenderCell::from_char(glyph);
}

/// Writes mux-owned pane-divider glyphs and caller-supplied style policy into
/// render canvases.
pub fn draw_styled_pane_dividers(
    text_canvas: &mut [Vec<TerminalRenderCell>],
    style_canvas: &mut [Vec<TerminalStyleSpan>],
    geometries: &[PaneGeometry],
    include_horizontal: bool,
    active_pane_index: usize,
    active_rendition: GraphicRendition,
    divider_rendition: GraphicRendition,
) {
    for cell in pane_divider_cells(geometries, include_horizontal) {
        let row = usize::from(cell.row);
        let column = usize::from(cell.column);
        if let Some(line) = text_canvas.get_mut(row) {
            write_single_width_cell(line, column, cell.glyph);
        }
        if let Some(spans) = style_canvas.get_mut(row) {
            spans.push(TerminalStyleSpan {
                start: column,
                length: 1,
                rendition: if divider_cell_touches_pane(cell, geometries, active_pane_index) {
                    active_rendition
                } else {
                    divider_rendition
                },
            });
        }
    }
}

/// Builds caller-styled spans for divider junctions bounding a merged pane
/// frame row.
pub fn merged_pane_frame_boundary_style_spans(
    geometries: &[PaneGeometry],
    row: u16,
    column_start: usize,
    width: usize,
    rendition: GraphicRendition,
) -> Vec<TerminalStyleSpan> {
    pane_divider_cells(geometries, true)
        .into_iter()
        .filter(|cell| {
            cell.row == row && merged_pane_frame_boundary_cell(*cell, column_start, width)
        })
        .map(|cell| TerminalStyleSpan {
            start: usize::from(cell.column),
            length: 1,
            rendition,
        })
        .collect()
}

fn merged_pane_frame_boundary_cell(
    cell: PaneDividerCell,
    column_start: usize,
    width: usize,
) -> bool {
    if cell.glyph == '\u{2502}' {
        return false;
    }
    let column = usize::from(cell.column);
    let column_end = column_start.saturating_add(width);
    (column_start > 0 && column.saturating_add(1) == column_start) || column == column_end
}

fn divider_cell_touches_pane(
    cell: PaneDividerCell,
    geometries: &[PaneGeometry],
    pane_index: usize,
) -> bool {
    let Some(geometry) = geometries
        .iter()
        .find(|geometry| geometry.index == pane_index)
    else {
        return false;
    };
    let column = cell.column;
    let row = cell.row;
    let vertical_overlap = row >= geometry.row && row < geometry.row.saturating_add(geometry.rows);
    let horizontal_overlap =
        column >= geometry.column && column < geometry.column.saturating_add(geometry.columns);
    let right_edge = geometry
        .column
        .saturating_add(geometry.columns)
        .saturating_sub(1);
    let bottom_edge = geometry.row.saturating_add(geometry.rows).saturating_sub(1);
    (vertical_overlap && (column == right_edge || column.saturating_add(1) == geometry.column))
        || (horizontal_overlap && (row == bottom_edge || row.saturating_add(1) == geometry.row))
}

/// Writes bounded text into a terminal cell row using wide-cell sentinels.
pub fn write_text_cells(
    row: &mut [TerminalRenderCell],
    column_start: usize,
    max_columns: usize,
    text: &str,
) {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(&fit_width(text, max_columns)) {
        let grapheme_width = active_grapheme_width(grapheme);
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
        row[cell] = TerminalRenderCell::from_grapheme(grapheme);
        for continuation in 1..grapheme_width {
            let continuation_cell = cell.saturating_add(continuation);
            if continuation_cell < row.len() {
                row[continuation_cell] = TerminalRenderCell::continuation();
            }
        }
        used = used.saturating_add(grapheme_width);
    }
}

/// Collects display cells while omitting internal continuation sentinels.
pub fn collect_text_cells(row: Vec<TerminalRenderCell>) -> String {
    let mut output = String::new();
    for cell in row {
        if !cell.continuation {
            output.push_str(&cell.text);
        }
    }
    output
}

/// Writes bounded text without right-padding and returns consumed display cells.
pub fn write_text_cells_with_width(
    row: &mut [TerminalRenderCell],
    column_start: usize,
    max_columns: usize,
    text: &str,
) -> usize {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(text) {
        let grapheme_width = active_grapheme_width(grapheme);
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
        row[cell] = TerminalRenderCell::from_grapheme(grapheme);
        for continuation in 1..grapheme_width {
            let continuation_cell = cell.saturating_add(continuation);
            if continuation_cell < row.len() {
                row[continuation_cell] = TerminalRenderCell::continuation();
            }
        }
        used = used.saturating_add(grapheme_width);
    }
    used
}

/// Builds a styled frame row from caller-selected rendition policy.
pub fn styled_frame_line_with_rendition(
    text: &str,
    width: usize,
    rendition: Option<GraphicRendition>,
) -> TerminalStyledLine {
    let text = fit_width(text, width);
    let Some(rendition) = rendition else {
        return TerminalStyledLine::plain(text);
    };
    TerminalStyledLine {
        text,
        style_spans: vec![TerminalStyleSpan {
            start: 0,
            length: width,
            rendition,
        }],
        copy_text: None,
    }
}

/// Returns terminal rendition flags for one mux frame style.
pub fn frame_style_rendition(frame_style: TerminalFrameStyle) -> Option<GraphicRendition> {
    match frame_style {
        TerminalFrameStyle::Default => None,
        TerminalFrameStyle::Bold => Some(GraphicRendition {
            bold: true,
            ..GraphicRendition::default()
        }),
        TerminalFrameStyle::Underline => Some(GraphicRendition {
            underline: true,
            ..GraphicRendition::default()
        }),
        TerminalFrameStyle::Inverse => Some(GraphicRendition {
            inverse: true,
            ..GraphicRendition::default()
        }),
    }
}

/// Chooses body rows for a bottom-aligned transient display overlay.
pub fn display_overlay_targets<T>(
    lines: &[T],
    content_start: usize,
    content_end: usize,
    display_line_count: usize,
    is_blank: impl Fn(&T) -> bool,
) -> Vec<usize> {
    if display_line_count == 0 || content_start >= content_end {
        return Vec::new();
    }
    let display_count = display_line_count.min(content_end.saturating_sub(content_start));
    let mut targets = Vec::with_capacity(display_count);
    for row in (content_start..content_end).rev() {
        if is_blank(&lines[row]) {
            targets.push(row);
            if targets.len() == display_count {
                break;
            }
        }
    }
    if targets.len() < display_count {
        for row in (content_start..content_end).rev() {
            if !targets.contains(&row) {
                targets.push(row);
                if targets.len() == display_count {
                    break;
                }
            }
        }
    }
    targets.sort_unstable();
    targets
}

/// Overlays transient display lines without changing pane content height.
pub fn overlay_display_lines<T: Clone>(
    lines: &mut [T],
    content_start: usize,
    content_end: usize,
    display_lines: &[T],
    is_blank: impl Fn(&T) -> bool,
) {
    let targets = display_overlay_targets(
        lines,
        content_start,
        content_end,
        display_lines.len(),
        is_blank,
    );
    let source_start = display_lines.len().saturating_sub(targets.len());
    for (target, display_line) in targets
        .into_iter()
        .zip(display_lines[source_start..].iter())
    {
        lines[target] = display_line.clone();
    }
}

/// Renders pane-frame title text over a caller-selected fill glyph.
pub fn pane_frame_text_with_fill(text: &str, width: usize, fill: char) -> (String, usize) {
    let mut row = blank_render_row(width, fill);
    let written_width = write_text_cells_with_width(&mut row, 0, width, text);
    (collect_text_cells(row), written_width)
}

/// Extends a pane-title pill over its separator before right-aligned status.
pub fn pane_frame_left_pill_style_width(text_width: usize, available_width: usize) -> usize {
    if text_width > 0 && text_width < available_width {
        text_width.saturating_add(1)
    } else {
        text_width
    }
}

/// Removes terminal control characters from frame and status text.
pub fn sanitize_frame_text(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

/// One caller-identified pill rendered in a window or group frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePillboxEntry<K> {
    /// Caller-owned semantic target used for actions and hit testing.
    pub target: K,
    /// Copyable display text for the pill.
    pub text: String,
    /// Whether the pill uses the active presentation state.
    pub active: bool,
    /// Whether the pill represents a spawned-subagent window.
    pub subagent: bool,
}

/// Display-column placement of one rendered frame pill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePillboxSegment<K> {
    /// Display-column offset within the containing frame row.
    pub start: usize,
    /// Terminal display width occupied by the pill.
    pub width: usize,
    /// Caller-owned semantic target used for actions and hit testing.
    pub target: K,
    /// Whether the pill uses the active presentation state.
    pub active: bool,
    /// Whether the pill represents a spawned-subagent window.
    pub subagent: bool,
}

/// Joins frame pills with one separating terminal cell.
pub fn render_frame_pillbox_text<K>(entries: &[FramePillboxEntry<K>]) -> String {
    entries
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Computes terminal display-column placements for caller-owned frame pills.
pub fn render_frame_pillbox_segments<K: Clone>(
    entries: &[FramePillboxEntry<K>],
) -> Vec<FramePillboxSegment<K>> {
    let mut segments = Vec::with_capacity(entries.len());
    let mut start = 0usize;
    for (entry_index, entry) in entries.iter().enumerate() {
        if entry_index > 0 {
            start = start.saturating_add(1);
        }
        let width = char_count(&entry.text);
        segments.push(FramePillboxSegment {
            start,
            width,
            target: entry.target.clone(),
            active: entry.active,
            subagent: entry.subagent,
        });
        start = start.saturating_add(width);
    }
    segments
}

/// Returns clipped local columns occupied by one frame-pill segment.
pub fn frame_pillbox_segment_columns(
    start: usize,
    width: usize,
    frame_width: usize,
) -> impl Iterator<Item = usize> {
    let start = start.min(frame_width);
    let end = start.saturating_add(width).min(frame_width);
    start..end
}

/// One semantic segment within a right-aligned frame status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameStatusSegment<K> {
    /// Display-column offset relative to the containing row or status text.
    pub start: usize,
    /// Display width of the segment.
    pub width: usize,
    /// Caller-owned semantic key used for styling and hit testing.
    pub key: K,
    /// Raw caller-owned value retained for presentation policy.
    pub value: String,
}

/// One semantic value to render in a right-aligned frame status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameStatusValue<K> {
    /// Caller-owned semantic key used for styling and hit testing.
    pub key: K,
    /// Raw caller-owned value retained for presentation policy.
    pub value: String,
    /// Text displayed in the status row.
    pub display: String,
}

/// Rendered right-aligned status text and its semantic segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFrameStatus<K> {
    /// Sanitized status text.
    pub text: String,
    /// Semantic segments relative to `text`.
    pub segments: Vec<FrameStatusSegment<K>>,
}

/// Right-aligned frame status placed within an authoritative row width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionedFrameStatus<K> {
    /// Sanitized status text.
    pub text: String,
    /// Absolute display-column offset within the frame row.
    pub start: usize,
    /// Display width occupied by the status text.
    pub width: usize,
    /// Semantic segments translated to absolute frame-row columns.
    pub segments: Vec<FrameStatusSegment<K>>,
}

/// Exact-width pane frame row with semantic right-status placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneFrameRowLayout<K> {
    /// Exact-width rendered frame text.
    pub text: String,
    /// Display width occupied by the left title pill.
    pub left_text_width: usize,
    /// Right-status segments in absolute row columns.
    pub right_status_segments: Vec<FrameStatusSegment<K>>,
}

/// Renders caller-owned status values with one separating cell.
pub fn render_frame_status<K: Clone>(values: &[FrameStatusValue<K>]) -> RenderedFrameStatus<K> {
    let mut text = String::new();
    let mut segments = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        let start = fitted_text_width(&text, usize::MAX);
        text.push_str(&value.display);
        let width = fitted_text_width(&value.display, usize::MAX);
        if width > 0 {
            segments.push(FrameStatusSegment {
                start,
                width,
                key: value.key.clone(),
                value: value.value.clone(),
            });
        }
    }
    RenderedFrameStatus {
        text: sanitize_frame_text(&text),
        segments,
    }
}

/// Places a rendered status at the right edge of an authoritative frame row.
pub fn position_frame_status<K>(
    status: RenderedFrameStatus<K>,
    width: usize,
) -> Option<PositionedFrameStatus<K>> {
    let (start, status_width) = right_aligned_status_bounds(&status.text, width)?;
    let segments = status
        .segments
        .into_iter()
        .filter_map(|segment| {
            clip_style_span(
                TerminalStyleSpan {
                    start: segment.start,
                    length: segment.width,
                    rendition: GraphicRendition::default(),
                },
                status_width,
            )
            .map(|span| FrameStatusSegment {
                start: start.saturating_add(span.start),
                width: span.length,
                key: segment.key,
                value: segment.value,
            })
        })
        .collect();
    Some(PositionedFrameStatus {
        text: status.text,
        start,
        width: status_width,
        segments,
    })
}

/// Composes a pane title and optional right status into one exact-width row.
pub fn compose_pane_frame_row<K>(
    left_text: &str,
    right_status: Option<RenderedFrameStatus<K>>,
    width: usize,
    fill: char,
) -> PaneFrameRowLayout<K> {
    if width == 0 {
        return PaneFrameRowLayout {
            text: String::new(),
            left_text_width: 0,
            right_status_segments: Vec::new(),
        };
    }
    let Some(right_status) = right_status else {
        let (text, left_text_width) = pane_frame_text_with_fill(left_text, width, fill);
        return PaneFrameRowLayout {
            text,
            left_text_width,
            right_status_segments: Vec::new(),
        };
    };
    let mut row = blank_render_row(width, fill);
    let Some((status_start, status_width)) = right_aligned_status_bounds(&right_status.text, width)
    else {
        let (text, left_text_width) = pane_frame_text_with_fill(left_text, width, fill);
        return PaneFrameRowLayout {
            text,
            left_text_width,
            right_status_segments: Vec::new(),
        };
    };
    let left_width = status_start.saturating_sub(1);
    let written_left_text_width = write_text_cells_with_width(&mut row, 0, left_width, left_text);
    let left_text_width = pane_frame_left_pill_style_width(written_left_text_width, left_width);
    write_text_cells_with_width(&mut row, status_start, status_width, &right_status.text);
    let right_status_segments = right_status
        .segments
        .into_iter()
        .filter_map(|segment| {
            clip_style_span(
                TerminalStyleSpan {
                    start: segment.start,
                    length: segment.width,
                    rendition: GraphicRendition::default(),
                },
                status_width,
            )
            .map(|span| FrameStatusSegment {
                start: status_start.saturating_add(span.start),
                width: span.length,
                key: segment.key,
                value: segment.value,
            })
        })
        .collect();
    PaneFrameRowLayout {
        text: collect_text_cells(row),
        left_text_width,
        right_status_segments,
    }
}

fn right_aligned_status_bounds(text: &str, width: usize) -> Option<(usize, usize)> {
    let status_limit = width.saturating_sub(usize::from(width > 1));
    let status_width = fitted_text_width(text, status_limit);
    if status_width == 0 {
        return None;
    }
    let trailing_padding = usize::from(width > status_width);
    Some((
        width.saturating_sub(status_width.saturating_add(trailing_padding)),
        status_width,
    ))
}

/// Fits terminal text to an exact display width, padding with spaces.
pub fn fit_width(value: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    for grapheme in terminal_graphemes(value) {
        let grapheme_width = active_grapheme_width(grapheme);
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

/// Returns the display width retained by bounded terminal-row fitting.
pub fn fitted_text_width(value: &str, max_width: usize) -> usize {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(value) {
        let grapheme_width = active_grapheme_width(grapheme);
        if used.saturating_add(grapheme_width) > max_width {
            break;
        }
        used = used.saturating_add(grapheme_width);
    }
    used
}

/// Fits a styled terminal line and clips spans to the retained width.
pub fn fit_styled_width(line: &TerminalStyledLine, width: usize) -> TerminalStyledLine {
    let text = fit_width(&line.text, width);
    let retained_width = fitted_text_width(&line.text, width);
    let style_spans = line
        .style_spans
        .iter()
        .filter_map(|span| clip_style_span(*span, retained_width))
        .collect::<Vec<_>>();
    TerminalStyledLine {
        text,
        style_spans,
        copy_text: line.copy_text.clone(),
    }
}

/// Replaces one fixed-width range with clipped source style spans.
pub fn overlay_fixed_column_style_spans(
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
pub fn style_span_overlaps_columns(span: TerminalStyleSpan, start: usize, end: usize) -> bool {
    span.start < end && span.start.saturating_add(span.length) > start
}

/// Keeps the parts of a style span outside a replaced column range.
pub fn style_span_segments_outside_range(
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
pub fn offset_style_span(span: TerminalStyleSpan, column_offset: usize) -> TerminalStyleSpan {
    TerminalStyleSpan {
        start: span.start.saturating_add(column_offset),
        length: span.length,
        rendition: span.rendition,
    }
}

/// Clips a style span to the given terminal row width.
pub fn clip_style_span(span: TerminalStyleSpan, width: usize) -> Option<TerminalStyleSpan> {
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

/// Returns a display-column slice from one terminal line.
pub fn line_slice(line: &str, start: usize, end: usize) -> String {
    let mut output = String::new();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = active_grapheme_width(grapheme);
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
pub fn char_count(value: &str) -> usize {
    terminal_text_width(value, terminal_emoji_width())
}

fn active_grapheme_width(grapheme: &str) -> usize {
    terminal_grapheme_width(grapheme, terminal_emoji_width())
}

#[cfg(test)]
mod tests {
    use super::{
        FramePillboxEntry, FrameStatusValue, blank_render_row, collect_text_cells,
        compose_pane_frame_row, display_overlay_targets, fit_styled_width,
        frame_pillbox_segment_columns, frame_style_rendition, line_slice, overlay_display_lines,
        overlay_fixed_column_style_spans, pane_frame_left_pill_style_width,
        pane_frame_text_with_fill, position_frame_status, render_frame_pillbox_segments,
        render_frame_pillbox_text, render_frame_status, sanitize_frame_text,
        styled_frame_line_with_rendition, write_single_width_cell, write_text_cells,
        write_text_cells_with_width,
    };
    use crate::presentation::TerminalFrameStyle;
    use mez_terminal::{GraphicRendition, TerminalStyleSpan, TerminalStyledLine};

    /// Verifies wide-glyph canvas writes preserve display columns when a
    /// divider overwrites the continuation half of a grapheme.
    #[test]
    fn render_canvas_clears_overwritten_wide_glyphs() {
        let mut row = blank_render_row(4, ' ');
        write_text_cells(&mut row, 0, 4, "界x");
        write_single_width_cell(&mut row, 1, '│');

        assert_eq!(collect_text_cells(row), " │x ");
    }

    /// Verifies styled fitting clips spans and terminal slicing never returns
    /// half of a wide grapheme cluster.
    #[test]
    fn styled_rows_clip_and_slice_on_display_columns() {
        let rendition = GraphicRendition::default();
        let line = TerminalStyledLine {
            text: "a界b".to_string(),
            style_spans: vec![TerminalStyleSpan {
                start: 1,
                length: 2,
                rendition,
            }],
            copy_text: None,
        };

        let fitted = fit_styled_width(&line, 2);
        assert_eq!(fitted.text, "a ");
        assert!(fitted.style_spans.is_empty());
        assert_eq!(line_slice("a界b", 1, 3), "界");
    }

    /// Verifies a fixed-column style overlay preserves span fragments on both
    /// sides and shifts the replacement span into absolute columns.
    #[test]
    fn style_overlay_preserves_outside_segments() {
        let rendition = GraphicRendition::default();
        let mut spans = vec![TerminalStyleSpan {
            start: 0,
            length: 6,
            rendition,
        }];
        overlay_fixed_column_style_spans(
            &mut spans,
            2,
            2,
            &[TerminalStyleSpan {
                start: 0,
                length: 2,
                rendition,
            }],
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| (span.start, span.length))
                .collect::<Vec<_>>(),
            vec![(0, 2), (4, 2), (2, 2)]
        );
    }

    /// Verifies frame-row composition applies mux frame-style flags while
    /// preserving exact terminal width and control-free display text.
    #[test]
    fn frame_rows_apply_neutral_style_and_text_policy() {
        let rendition = frame_style_rendition(TerminalFrameStyle::Underline)
            .expect("underline frame style should produce a rendition");
        let line = styled_frame_line_with_rendition("status", 8, Some(rendition));

        assert_eq!(line.text, "status  ");
        assert_eq!(line.style_spans.len(), 1);
        assert!(line.style_spans[0].rendition.underline);
        assert_eq!(sanitize_frame_text("ok\u{1b}bad\n"), "okbad");
    }

    /// Verifies pane-title composition preserves wide-cell accounting and
    /// extends a non-empty title pill over its separator before right status.
    #[test]
    fn pane_frame_text_composition_tracks_display_width() {
        let (text, written) = pane_frame_text_with_fill("界", 4, '─');
        let mut row = blank_render_row(4, ' ');
        let direct = write_text_cells_with_width(&mut row, 0, 4, "界x");

        assert_eq!(text, "界──");
        assert_eq!(written, 2);
        assert_eq!(direct, 3);
        assert_eq!(pane_frame_left_pill_style_width(written, 4), 3);
    }

    /// Verifies neutral frame-status composition right-aligns bounded values
    /// while retaining semantic keys and raw values for product adapters.
    #[test]
    fn pane_frame_status_composition_preserves_semantic_segments() {
        let status = render_frame_status(&[
            FrameStatusValue {
                key: "model",
                value: "gpt-5".to_string(),
                display: " gpt-5 ".to_string(),
            },
            FrameStatusValue {
                key: "state",
                value: "running".to_string(),
                display: " run ".to_string(),
            },
        ]);
        let layout = compose_pane_frame_row(" 1 shell ", Some(status), 24, '─');

        assert_eq!(layout.text.chars().count(), 24);
        assert_eq!(layout.right_status_segments.len(), 2);
        assert_eq!(layout.right_status_segments[0].key, "model");
        assert_eq!(layout.right_status_segments[0].value, "gpt-5");
        assert_eq!(layout.right_status_segments[1].key, "state");
        assert_eq!(layout.right_status_segments[1].value, "running");
        assert!(layout.right_status_segments[0].start > layout.left_text_width);
    }

    /// Verifies generic frame-status placement clips semantic segments and
    /// translates retained targets into authoritative row columns.
    #[test]
    fn frame_status_placement_clips_and_offsets_segments() {
        let status = render_frame_status(&[FrameStatusValue {
            key: "action",
            value: "open".to_string(),
            display: " open ".to_string(),
        }]);
        let positioned = position_frame_status(status, 5)
            .expect("non-empty status should fit within the frame row");

        assert_eq!(positioned.text, " open ");
        assert_eq!((positioned.start, positioned.width), (0, 4));
        assert_eq!(positioned.segments.len(), 1);
        assert_eq!(positioned.segments[0].start, 0);
        assert_eq!(positioned.segments[0].width, 4);
        assert_eq!(positioned.segments[0].key, "action");
    }

    /// Verifies generic window/group pill composition remains Unicode-width
    /// aware and preserves caller-owned targets for styling and hit testing.
    #[test]
    fn frame_pillbox_composition_preserves_targets_and_columns() {
        let entries = vec![
            FramePillboxEntry {
                target: "first",
                text: " 1 shell ".to_string(),
                active: true,
                subagent: false,
            },
            FramePillboxEntry {
                target: "second",
                text: " 界 ".to_string(),
                active: false,
                subagent: true,
            },
        ];
        let segments = render_frame_pillbox_segments(&entries);

        assert_eq!(render_frame_pillbox_text(&entries), " 1 shell   界 ");
        assert_eq!(segments[0].target, "first");
        assert_eq!(segments[1].target, "second");
        assert_eq!(segments[1].start, segments[0].width + 1);
        assert_eq!(segments[1].width, 4);
        assert_eq!(
            frame_pillbox_segment_columns(segments[1].start, segments[1].width, 12)
                .collect::<Vec<_>>(),
            vec![10, 11]
        );
    }

    /// Verifies display overlays prefer blank bottom rows, fall back to
    /// occupied rows when needed, and preserve the caller's row type.
    #[test]
    fn display_overlays_choose_bottom_rows_without_resizing() {
        let mut lines = vec!["body", "", "tail"];
        assert_eq!(
            display_overlay_targets(&lines, 0, 3, 2, |line| line.is_empty()),
            [1, 2]
        );

        overlay_display_lines(&mut lines, 0, 3, &["first", "second"], |line| {
            line.is_empty()
        });

        assert_eq!(lines, ["body", "first", "second"]);
    }
}
