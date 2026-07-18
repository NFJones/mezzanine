//! Dependency-neutral terminal canvas and styled-row composition.
//!
//! This module owns display-cell storage, Unicode-width-aware text placement,
//! terminal-row fitting and slicing, and style-span clipping/overlay behavior.
//! It deliberately excludes product themes, agent transcript wrapping, prompt
//! policy, overlays, and host terminal encoding.

use mez_terminal::{
    GraphicRendition, TerminalSize, TerminalStyleSpan, TerminalStyledLine, terminal_emoji_width,
    terminal_grapheme_width, terminal_graphemes, terminal_text_width,
};

use crate::layout::PaneGeometry;
use crate::presentation::{
    PaneDividerCell, TerminalFrameStyle, pane_canvas_placements, pane_divider_cells,
};

mod diff;
mod overlay;
mod prompt;
mod rich_text;
mod style;
mod wrap;

pub use diff::{
    DiffDisplayLine, DiffDisplaySection, SyntaxHighlighter, SyntaxTheme, SyntaxThemePalette,
    append_syntax_spans, clean_diff_label, diff_highlighter_for_path, diff_section_path,
    diff_syntax_for_path, format_diff_display_line, parse_diff_hunk_header, parse_diff_range_start,
    parse_unified_diff_sections, syntax_highlighter_for_extension, syntax_theme,
};
pub use overlay::{
    NormalizedOverlayCanvas, compose_bottom_overlay_lines, compose_modal_overlay_lines,
    modal_overlay_max_scroll, modal_overlay_page_rows, normalize_overlay_canvas,
    normalize_overlay_style_spans, overlay_text_style_width,
};
pub use prompt::{
    PromptRegionPresentation, PromptRegionRenderOptions, PromptShadowSpan, WrappedPromptLayout,
    clipped_prompt_region, compose_prompt_region, layout_wrapped_prompt,
    wrap_prompt_line_with_cursor_and_shadow, write_line_segment,
};
pub use rich_text::{
    MARKDOWN_BLOCK_DIVIDER_GLYPH, MARKDOWN_DARK_MUTED_FOREGROUND, MARKDOWN_DARK_NEUTRAL_FOREGROUND,
    MARKDOWN_LIGHT_NEUTRAL_FOREGROUND, RichTextLine, RichTextLineKind, RichTextTheme,
    WrappedRichTextLine, frame_markdown_lines, insert_blank_lines_above_markdown_headings,
    markdown_blank_line, markdown_block_copy_lines, markdown_link_display_ranges,
    markdown_local_continuation_indent_width, markdown_rendered_line_copy_text,
    markdown_rendered_line_is_heading, markdown_rendered_line_is_table_row, prefix_rich_text_lines,
    render_markdown, rendered_line_continuation_indent, rendered_line_is_numbered_diff_row,
    style_spans_for_rich_text_segment, take_rich_text_display_segment,
    wrap_rich_text_line_to_width, wrap_rich_text_line_to_width_with_source_ranges,
    wrap_rich_text_line_to_width_with_source_ranges_hard, wrap_rich_text_lines_to_width,
};
pub use style::{
    agent_status_running_gradient_palette, animated_scan_background, blend_terminal_color,
    contrasting_binary_foreground, gradient_highlight_for_offset, neutral_surface_step,
    push_or_extend_style_span, shifted_channel, srgb_channel_to_linear,
    terminal_color_contrast_ratio, terminal_color_luminance, terminal_color_relative_luminance,
    terminal_color_rgb,
};
pub use wrap::{wrap_lines, wrap_text};

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

/// Composes plain pane rows into one mux-owned window-body canvas.
///
/// The callback may add caller-owned frame content after pane text and
/// dividers have been placed but before the canvas is flattened to strings.
pub fn compose_plain_pane_rows(
    size: TerminalSize,
    geometries: &[PaneGeometry],
    rendered_panes: &[Vec<String>],
    overlay: impl FnOnce(&mut [Vec<TerminalRenderCell>]),
) -> Vec<String> {
    let mut canvas = blank_render_cells(usize::from(size.rows), usize::from(size.columns), ' ');
    for placement in pane_canvas_placements(size, geometries) {
        let Some(pane) = rendered_panes.get(placement.source_index) else {
            continue;
        };
        for row_offset in 0..placement.pane_rows {
            if let Some(line) = pane.get(row_offset) {
                write_text_cells(
                    &mut canvas[placement.row_start + row_offset],
                    placement.column_start,
                    placement.pane_columns,
                    line,
                );
            }
        }
    }
    draw_pane_dividers(&mut canvas, geometries, true);
    overlay(&mut canvas);
    canvas.into_iter().map(collect_text_cells).collect()
}

/// Composes styled pane rows into one mux-owned window-body canvas.
///
/// Divider colors remain caller policy. The callback may add caller-owned
/// frame text and spans before the canvases are flattened into styled rows.
pub fn compose_styled_pane_rows(
    size: TerminalSize,
    geometries: &[PaneGeometry],
    rendered_panes: &[Vec<TerminalStyledLine>],
    active_pane_index: usize,
    active_rendition: GraphicRendition,
    divider_rendition: GraphicRendition,
    overlay: impl FnOnce(&mut [Vec<TerminalRenderCell>], &mut [Vec<TerminalStyleSpan>]),
) -> Vec<TerminalStyledLine> {
    let rows = usize::from(size.rows);
    let mut text_canvas = blank_render_cells(rows, usize::from(size.columns), ' ');
    let mut style_canvas = vec![Vec::new(); rows];
    for placement in pane_canvas_placements(size, geometries) {
        let Some(pane) = rendered_panes.get(placement.source_index) else {
            continue;
        };
        for row_offset in 0..placement.pane_rows {
            let Some(line) = pane.get(row_offset) else {
                continue;
            };
            write_text_cells(
                &mut text_canvas[placement.row_start + row_offset],
                placement.column_start,
                placement.pane_columns,
                &line.text,
            );
            style_canvas[placement.row_start + row_offset].extend(
                line.style_spans
                    .iter()
                    .filter_map(|span| clip_style_span(*span, placement.pane_columns))
                    .map(|span| offset_style_span(span, placement.column_start)),
            );
        }
    }
    draw_styled_pane_dividers(
        &mut text_canvas,
        &mut style_canvas,
        geometries,
        true,
        active_pane_index,
        active_rendition,
        divider_rendition,
    );
    overlay(&mut text_canvas, &mut style_canvas);
    text_canvas
        .into_iter()
        .zip(style_canvas)
        .map(|(row, style_spans)| TerminalStyledLine {
            text: collect_text_cells(row),
            style_spans,
            copy_text: None,
        })
        .collect()
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

/// Writes mux-owned pane-divider glyphs into a plain text canvas.
pub fn draw_pane_dividers(
    canvas: &mut [Vec<TerminalRenderCell>],
    geometries: &[PaneGeometry],
    include_horizontal: bool,
) {
    for cell in pane_divider_cells(geometries, include_horizontal) {
        let row = usize::from(cell.row);
        let column = usize::from(cell.column);
        if let Some(line) = canvas.get_mut(row) {
            write_single_width_cell(line, column, cell.glyph);
        }
    }
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

/// Compacts a home-relative or absolute display path to its final segments.
///
/// Root markers (`/`, `~`, and `~/`) are preserved. Relative paths retain no
/// synthetic prefix, while paths deeper than `max_segments` use an ellipsis.
pub fn compact_display_path(value: &str, max_segments: usize) -> String {
    let max_segments = max_segments.max(1);
    let (prefix, path) = if let Some(rest) = value.strip_prefix("~/") {
        ("~/", rest)
    } else if value == "~" || value == "/" {
        return value.to_string();
    } else if let Some(rest) = value.strip_prefix('/') {
        ("/", rest)
    } else {
        ("", value)
    };
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() <= max_segments {
        return format!("{prefix}{path}");
    }
    format!(
        "…/{}",
        segments[segments.len().saturating_sub(max_segments)..].join("/")
    )
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

/// One terminal cell occupied by a semantic frame segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHitCell<K> {
    /// Terminal column occupied by the segment.
    pub column: u16,
    /// Terminal row occupied by the segment.
    pub row: u16,
    /// Caller-owned semantic target used to adapt the hit into an action.
    pub target: K,
}

/// Expands clipped frame-pill segments into terminal hit cells.
pub fn frame_pillbox_hit_cells<K: Clone>(
    segments: &[FramePillboxSegment<K>],
    row: u16,
    frame_width: u16,
) -> Vec<FrameHitCell<K>> {
    segments
        .iter()
        .flat_map(|segment| {
            frame_pillbox_segment_columns(segment.start, segment.width, usize::from(frame_width))
                .filter_map(|column| u16::try_from(column).ok())
                .map(|column| FrameHitCell {
                    column,
                    row,
                    target: segment.target.clone(),
                })
        })
        .collect()
}

/// Expands clipped right-status segments into terminal hit cells.
pub fn frame_status_hit_cells<K: Clone>(
    segments: &[FrameStatusSegment<K>],
    row: u16,
    frame_width: u16,
) -> Vec<FrameHitCell<K>> {
    segments
        .iter()
        .flat_map(|segment| {
            frame_pillbox_segment_columns(segment.start, segment.width, usize::from(frame_width))
                .filter_map(|column| u16::try_from(column).ok())
                .map(|column| FrameHitCell {
                    column,
                    row,
                    target: segment.key.clone(),
                })
        })
        .collect()
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

/// Exact-width window or group frame row with semantic pill and status placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePillboxRowLayout<P, S> {
    /// Exact-width rendered frame text.
    pub text: String,
    /// Left-side pill segments clipped before the right-aligned status.
    pub pillbox_segments: Vec<FramePillboxSegment<P>>,
    /// Right-status segments in absolute row columns.
    pub right_status_segments: Vec<FrameStatusSegment<S>>,
}

/// Expands a frame template through a caller-owned field resolver.
///
/// Unknown fields and product-specific formatting remain the caller's
/// responsibility. Unterminated field markers are preserved literally, and
/// the completed row is sanitized for terminal-safe display.
pub fn render_frame_template(
    template: &str,
    mut resolve_field: impl FnMut(&str) -> String,
) -> String {
    let mut rendered = String::new();
    let mut remaining = template;
    loop {
        let Some(start) = remaining.find("#{") else {
            rendered.push_str(remaining);
            break;
        };
        rendered.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find('}') else {
            rendered.push_str(&remaining[start..]);
            break;
        };
        rendered.push_str(&resolve_field(&after_start[..end]));
        remaining = &after_start[end + 1..];
    }
    sanitize_frame_text(&rendered)
}

/// Expands a semantic status template through a caller-owned field resolver.
///
/// Each resolved component carries text plus segments relative to that text.
/// This function concatenates the components, translates every segment into
/// template-relative columns, and sanitizes the final display text.
pub fn render_frame_status_template<K>(
    template: &str,
    mut resolve_field: impl FnMut(&str) -> RenderedFrameStatus<K>,
) -> RenderedFrameStatus<K> {
    let mut text = String::new();
    let mut segments = Vec::new();
    let mut remaining = template;
    loop {
        let Some(start) = remaining.find("#{") else {
            text.push_str(remaining);
            break;
        };
        text.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find('}') else {
            text.push_str(&remaining[start..]);
            break;
        };
        let component = resolve_field(&after_start[..end]);
        let value_start = fitted_text_width(&text, usize::MAX);
        text.push_str(&component.text);
        segments.extend(component.segments.into_iter().map(|mut segment| {
            segment.start = value_start.saturating_add(segment.start);
            segment
        }));
        remaining = &after_start[end + 1..];
    }
    RenderedFrameStatus {
        text: sanitize_frame_text(&text),
        segments,
    }
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
pub fn compose_frame_text_row<K>(
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

/// Composes caller-owned frame pills and an optional right status into one row.
pub fn compose_frame_pillbox_row<P: Clone, S>(
    entries: &[FramePillboxEntry<P>],
    right_status: Option<RenderedFrameStatus<S>>,
    width: usize,
    fill: char,
) -> FramePillboxRowLayout<P, S> {
    let left_text = render_frame_pillbox_text(entries);
    let mut row = blank_render_row(width, fill);
    let positioned_status = right_status.and_then(|status| position_frame_status(status, width));
    let left_width = positioned_status
        .as_ref()
        .map(|status| status.start)
        .unwrap_or(width);
    write_text_cells_with_width(&mut row, 0, left_width, &left_text);
    if let Some(status) = positioned_status.as_ref() {
        write_text_cells_with_width(&mut row, status.start, status.width, &status.text);
    }
    let pillbox_segments = render_frame_pillbox_segments(entries)
        .into_iter()
        .filter_map(|mut segment| {
            let span = clip_style_span(
                TerminalStyleSpan {
                    start: segment.start,
                    length: segment.width,
                    rendition: GraphicRendition::default(),
                },
                left_width,
            )?;
            segment.start = span.start;
            segment.width = span.length;
            Some(segment)
        })
        .collect();
    FramePillboxRowLayout {
        text: collect_text_cells(row),
        pillbox_segments,
        right_status_segments: positioned_status
            .map(|status| status.segments)
            .unwrap_or_default(),
    }
}

/// Composes a pane title and optional right status into one exact-width row.
pub fn compose_pane_frame_row<K>(
    left_text: &str,
    right_status: Option<RenderedFrameStatus<K>>,
    width: usize,
    fill: char,
) -> PaneFrameRowLayout<K> {
    compose_frame_text_row(left_text, right_status, width, fill)
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

/// Clips style spans to one half-open viewport column range.
pub fn clip_style_spans(
    spans: &[TerminalStyleSpan],
    column_offset: usize,
    width: usize,
) -> Vec<TerminalStyleSpan> {
    let end = column_offset.saturating_add(width);
    spans
        .iter()
        .filter_map(|span| {
            let clipped_start = span.start.max(column_offset);
            let clipped_end = span.start.saturating_add(span.length).min(end);
            (clipped_start < clipped_end).then(|| TerminalStyleSpan {
                start: clipped_start.saturating_sub(column_offset),
                length: clipped_end.saturating_sub(clipped_start),
                rendition: span.rendition,
            })
        })
        .collect()
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

/// Replaces a fixed-width terminal-cell range in one rendered text row.
///
/// The replacement is clipped and padded to `columns`, while graphemes outside
/// the target range retain their original display-cell positions.
pub fn overlay_text_cells(row: &mut String, column_start: usize, columns: usize, text: &str) {
    if columns == 0 {
        return;
    }
    let target_end = column_start.saturating_add(columns);
    let fitted = fit_width(text, columns);
    let mut output = String::new();
    let mut current_column = 0usize;
    let mut inserted = false;

    for grapheme in terminal_graphemes(row.as_str()) {
        let grapheme_width = active_grapheme_width(grapheme);
        let next_column = current_column.saturating_add(grapheme_width);
        if next_column <= column_start {
            output.push_str(grapheme);
        } else if !inserted {
            let reached = char_count(&output);
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
        let reached = char_count(&output);
        if reached < column_start {
            output.push_str(&" ".repeat(column_start.saturating_sub(reached)));
        }
        output.push_str(&fitted);
    }
    *row = output;
}

/// Clips one local style span into a destination overlay range.
pub fn clipped_overlay_style_span(
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
mod decomposition_tests {
    use super::*;

    /// Verifies generic frame path fitting preserves roots and bounds deep
    /// paths without requiring pane or product context.
    #[test]
    fn compact_display_path_preserves_roots_and_bounds_depth() {
        assert_eq!(compact_display_path("/", 3), "/");
        assert_eq!(compact_display_path("~/one/two", 3), "~/one/two");
        assert_eq!(
            compact_display_path("/one/two/three/four", 3),
            "…/two/three/four"
        );
    }
}

fn active_grapheme_width(grapheme: &str) -> usize {
    terminal_grapheme_width(grapheme, terminal_emoji_width())
}

#[cfg(test)]
mod overlay_cell_tests {
    use super::{char_count, overlay_text_cells};

    /// Verifies overlay replacement follows display cells across wide text.
    #[test]
    fn overlay_replaces_wide_cell_range() {
        let mut row = String::from("ＡＢＣ");
        overlay_text_cells(&mut row, 4, 2, "XY");
        assert_eq!(row, "ＡＢXY");
        assert_eq!(char_count(&row), 6);
    }

    /// Verifies overlay replacement pads ranges beyond existing row content.
    #[test]
    fn overlay_after_row_end_pads_to_destination() {
        let mut row = String::from("Ａ");
        overlay_text_cells(&mut row, 4, 2, "XY");
        assert_eq!(char_count(&row), 6);
        assert!(row.ends_with("  XY"));
    }

    /// Verifies a zero-width overlay leaves the source row unchanged.
    #[test]
    fn zero_width_overlay_is_noop() {
        let mut row = String::from("ＡＢ");
        overlay_text_cells(&mut row, 0, 0, "X");
        assert_eq!(row, "ＡＢ");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FramePillboxEntry, FrameStatusValue, blank_render_row, char_count, collect_text_cells,
        compose_frame_pillbox_row, compose_pane_frame_row, display_overlay_targets,
        fit_styled_width, frame_pillbox_hit_cells, frame_pillbox_segment_columns,
        frame_status_hit_cells, frame_style_rendition, line_slice, overlay_display_lines,
        overlay_fixed_column_style_spans, pane_frame_left_pill_style_width,
        pane_frame_text_with_fill, position_frame_status, render_frame_pillbox_segments,
        render_frame_pillbox_text, render_frame_status, render_frame_status_template,
        render_frame_template, sanitize_frame_text, styled_frame_line_with_rendition,
        write_single_width_cell, write_text_cells, write_text_cells_with_width,
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

    /// Verifies frame hit-cell expansion clips semantic pill and status
    /// targets to the visible frame width while preserving the terminal row.
    #[test]
    fn frame_hit_cells_clip_and_preserve_semantic_targets() {
        let entries = vec![FramePillboxEntry {
            target: "window",
            text: " window ".to_string(),
            active: true,
            subagent: false,
        }];
        let pill_cells = frame_pillbox_hit_cells(&render_frame_pillbox_segments(&entries), 3, 4);
        let status = position_frame_status(
            render_frame_status(&[FrameStatusValue {
                key: "action",
                value: "open".to_string(),
                display: " open ".to_string(),
            }]),
            5,
        )
        .expect("status should fit the requested frame width");
        let status_cells = frame_status_hit_cells(&status.segments, 7, 3);

        assert_eq!(
            pill_cells
                .iter()
                .map(|cell| (cell.column, cell.row, cell.target))
                .collect::<Vec<_>>(),
            vec![
                (0, 3, "window"),
                (1, 3, "window"),
                (2, 3, "window"),
                (3, 3, "window")
            ]
        );
        assert_eq!(
            status_cells
                .iter()
                .map(|cell| (cell.column, cell.row, cell.target))
                .collect::<Vec<_>>(),
            vec![(0, 7, "action"), (1, 7, "action"), (2, 7, "action")]
        );
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

    /// Verifies generic frame template expansion preserves unterminated
    /// markers while sanitizing resolved and literal display text.
    #[test]
    fn frame_template_expansion_is_terminal_safe() {
        let rendered = render_frame_template("#{name} #{missing} #{open", |field| match field {
            "name" => "shell\u{1b}".to_string(),
            _ => String::new(),
        });

        assert_eq!(rendered, "shell  #{open");
    }

    /// Verifies semantic status-template expansion translates component
    /// segments across literal text and preceding resolved fields.
    #[test]
    fn frame_status_template_offsets_semantic_segments() {
        let rendered = render_frame_status_template("x#{first}/#{second}", |field| {
            let display = format!(" {field} ");
            super::RenderedFrameStatus {
                text: display.clone(),
                segments: vec![super::FrameStatusSegment {
                    start: 1,
                    width: field.len(),
                    key: field.to_string(),
                    value: field.to_string(),
                }],
            }
        });

        assert_eq!(rendered.text, "x first / second ");
        assert_eq!(rendered.segments[0].start, 2);
        assert_eq!(rendered.segments[1].start, 10);
        assert_eq!(rendered.segments[1].key, "second");
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

    /// Verifies exact-width frame rows clip left pills before a right-aligned
    /// status while preserving semantic pill and status targets.
    #[test]
    fn frame_pillbox_rows_preserve_clipped_semantic_segments() {
        let entries = vec![FramePillboxEntry {
            target: "window",
            text: " 1 shell ".to_string(),
            active: true,
            subagent: false,
        }];
        let status = render_frame_status(&[FrameStatusValue {
            key: "status",
            value: "ready".to_string(),
            display: " ready ".to_string(),
        }]);
        let row = compose_frame_pillbox_row(&entries, Some(status), 12, ' ');

        assert_eq!(char_count(&row.text), 12);
        assert_eq!(row.pillbox_segments.len(), 1);
        assert_eq!(row.pillbox_segments[0].target, "window");
        assert_eq!(row.pillbox_segments[0].width, 4);
        assert_eq!(row.right_status_segments.len(), 1);
        assert_eq!(row.right_status_segments[0].key, "status");
        assert_eq!(row.right_status_segments[0].value, "ready");
        assert_eq!(row.right_status_segments[0].start, 4);
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
