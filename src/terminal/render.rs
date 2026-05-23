//! Terminal Render implementation.
//!
//! This module owns the terminal render boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS, BTreeMap, ClientStatusKind, ClientStatusLine,
    ClientViewRole, CopyPosition, GraphicRendition, MezError, MouseBorderCell,
    MousePaneAgentStatusCell, MouseWindowActionFrameCell, MouseWindowFrameCell,
    MouseWindowGroupFrameCell, PaneAgentStatusField, PaneGeometry, PaneRenderInput, ReadlinePrompt,
    ReadlinePromptClientPresentation, ReadlinePromptRegion, ReadlinePromptStatusRow,
    RenderedClientView, Result, Size, TerminalClientLoopConfig, TerminalColor,
    TerminalFrameContext, TerminalFramePosition, TerminalFrameStyle, TerminalPaneFrameContext,
    TerminalScreen, TerminalStyleSpan, TerminalStyledLine, TerminalWindowFrameContext,
    TerminalWindowGroupFrameContext, TerminalWindowStatusContext, UiColorPair, UiTheme,
    UnicodeWidthChar, UnicodeWidthStr, Window, WindowFrameAction,
};
use crate::readline::ReadlinePromptKind;
use unicode_segmentation::UnicodeSegmentation;

// Client view composition and pane/window rendering.

const MIN_PROMPT_SHADOW_CONTRAST_RATIO: f64 = 4.5;

/// Defines the DEFAULT PANE FRAME TEMPLATE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PANE_FRAME_TEMPLATE: &str = " #{pane.index} #{pane.title} ";
/// Defines the DEFAULT PANE FRAME VISIBLE FIELDS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PANE_FRAME_VISIBLE_FIELDS: &[&str] = &[
    "pane.index",
    "pane.title",
    "pane.id",
    "history.position",
    "agent.model",
    "agent.reasoning",
    "agent.auto_reasoning",
    "agent.name",
    "agent.context_usage",
    "agent.status",
    "policy.mode",
];

/// Pane frame fields that can occupy the right side of the standard pane bar.
/// Scrollback position takes over this slot while copy-mode is away from bottom.
pub const DEFAULT_PANE_FRAME_RIGHT_ALIGNED: &[&str] = &[
    "history.position",
    "agent.model",
    "agent.reasoning",
    "agent.auto_reasoning",
    "agent.latency",
    "agent.preset",
    "policy.mode",
    "agent.context_usage",
    "agent.status",
];
/// Defines the DEFAULT WINDOW FRAME TEMPLATE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_WINDOW_FRAME_TEMPLATE: &str = "#{window.list}";
/// Defines the DEFAULT WINDOW FRAME RIGHT STATUS TEMPLATE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE: &str = "#{pane.pwd} #{button:-|terminal|split-window -h} #{button:+|terminal|split-window} #{button:□|terminal|new-window} #{button:⊕|terminal|new-group} #{button:λ|terminal|agent-shell} #{system.uptime} #{datetime.local}";
/// Defines the DEFAULT WINDOW FRAME VISIBLE FIELDS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS: &[&str] = &[
    "window.list",
    "window.index",
    "window.name",
    "window.id",
    "pane.index",
    "pane.title",
    "pane.id",
    "window.pane_count",
    "window.buttons",
    "pane.pwd",
    "system.uptime",
    "datetime.local",
];

/// Returns true when the top window-group bar should be visible.
pub fn group_frame_visible(frame_context: &TerminalFrameContext) -> bool {
    frame_context.groups.len() > 1
}

/// Frame rendering choices for a window or pane region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalFrameRenderOptions<'a> {
    /// Whether the frame row should be rendered.
    pub enabled: bool,
    /// Named-field template used to build the frame row.
    pub template: &'a str,
    /// Placement of the frame row inside its owning region.
    pub position: TerminalFramePosition,
    /// Style applied to the frame row for styled attached-terminal output.
    pub style: TerminalFrameStyle,
}

impl<'a> TerminalFrameRenderOptions<'a> {
    /// Builds frame options with default styling for plain text rendering.
    pub const fn plain(enabled: bool, template: &'a str, position: TerminalFramePosition) -> Self {
        Self {
            enabled,
            template,
            position,
            style: TerminalFrameStyle::Default,
        }
    }

    /// Builds frame options with an explicit attached-terminal style.
    pub const fn styled(
        enabled: bool,
        template: &'a str,
        position: TerminalFramePosition,
        style: TerminalFrameStyle,
    ) -> Self {
        Self {
            enabled,
            template,
            position,
            style,
        }
    }
}

/// Runs the render attached client view operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn render_attached_client_view(
    role: ClientViewRole,
    window: &Window,
    screens: &BTreeMap<String, TerminalScreen>,
    config: &TerminalClientLoopConfig,
    client_size: Size,
) -> Result<Option<RenderedClientView>> {
    if role == ClientViewRole::PendingObserver {
        return Ok(None);
    }
    let render_window = window_with_group_frame_space(window, config)?;
    let styled_lines = draw_styled_window_from_screens(window, screens, config)?;
    let mut lines = Vec::with_capacity(styled_lines.len());
    let mut line_style_spans = Vec::with_capacity(styled_lines.len());
    for line in styled_lines {
        lines.push(line.text);
        line_style_spans.push(line.style_spans);
    }
    let (cursor_row, cursor_column, cursor_visible) =
        rendered_cursor(&render_window, screens, config, role)?;
    let group_offset = group_frame_top_offset(config);
    let cursor_row = cursor_row.saturating_add(group_offset);
    let agent_prompt_region =
        active_agent_prompt_region(&render_window, config, role)?.map(|mut region| {
            region.row = region.row.saturating_add(group_offset);
            region
        });
    align_active_agent_prompt_block_to_region(
        &render_window,
        config,
        role,
        agent_prompt_region,
        &mut lines,
        &mut line_style_spans,
    );
    let requires_client_scroll = role == ClientViewRole::Observer
        && (client_size.columns < window.size.columns || client_size.rows < window.size.rows);
    Ok(Some(RenderedClientView {
        role,
        authoritative_size: window.size,
        client_size,
        lines,
        line_style_spans,
        requires_client_scroll,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row,
        cursor_column,
        cursor_visible,
        cursor_style: config.cursor_style,
        cursor_blink: config.cursor_blink,
        cursor_blink_interval_ms: config.cursor_blink_interval_ms,
        application_keypad: config.mouse_policy.pane_application_keypad_mode,
        bracketed_paste: config.pane_bracketed_paste_mode,
        animation_refresh_interval_ms: if config.frame_context.animation_tick_ms > 0 {
            AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS
        } else {
            0
        },
        ui_theme: config.ui_theme.clone(),
        agent_prompt_region,
        primary_prompt_active: false,
    }))
}

/// Returns the number of rows reserved above the window for the group bar.
fn group_frame_top_offset(config: &TerminalClientLoopConfig) -> usize {
    usize::from(group_frame_visible(&config.frame_context))
}

/// Returns a display window whose body is reduced by the conditional group bar.
fn window_with_group_frame_space(
    window: &Window,
    config: &TerminalClientLoopConfig,
) -> Result<Window> {
    if !group_frame_visible(&config.frame_context) {
        return Ok(window.clone());
    }
    let mut window = window.clone();
    window.size = Size::new(
        window.size.columns,
        window.size.rows.saturating_sub(1).max(1),
    )?;
    Ok(window)
}

/// Runs the rendered cursor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn rendered_cursor(
    window: &Window,
    screens: &BTreeMap<String, TerminalScreen>,
    config: &TerminalClientLoopConfig,
    role: ClientViewRole,
) -> Result<(usize, usize, bool)> {
    if role != ClientViewRole::Primary {
        return Ok((0, 0, false));
    }
    let active_index = window.active_pane_index();
    let Some(active_pane) = window.panes().get(active_index) else {
        return Ok((0, 0, false));
    };
    let body_size = window_body_size(window.size, config.window_frames_enabled)?;
    let geometries = if window.zoomed_pane_id() == Some(&active_pane.id) {
        vec![zoomed_pane_geometry(body_size)]
    } else {
        rendered_pane_geometries(window, config.window_frames_enabled)?
    };
    let geometry = geometries
        .iter()
        .copied()
        .find(|geometry| geometry.index == active_index)
        .unwrap_or(PaneGeometry {
            index: active_index,
            column: 0,
            row: 0,
            columns: active_pane.size.columns,
            rows: active_pane.size.rows,
        });
    let window_frame_top_offset = usize::from(
        config.window_frames_enabled && config.window_frame_position == TerminalFramePosition::Top,
    );
    let pane_frame_top_offset = usize::from(
        config.pane_frames_enabled
            && config.pane_frame_position == TerminalFramePosition::Top
            && !pane_frame_merges_into_divider(&geometry, &geometries, config.pane_frame_position),
    );
    let content_size = pane_content_size_for_geometry(
        &geometry,
        &geometries,
        config.pane_frames_enabled,
        config.pane_frame_position,
    )?;
    let content_rows = usize::from(content_size.rows);
    let content_columns = usize::from(content_size.columns);
    if pane_agent_shell_visible(&config.frame_context, active_pane.id.as_str()) {
        let block = render_agent_prompt_block(
            content_columns,
            content_rows,
            config.frame_context.panes.get(active_pane.id.as_str()),
        );
        let window_frame_top_offset = usize::from(
            config.window_frames_enabled
                && config.window_frame_position == TerminalFramePosition::Top,
        );
        let body_row = window_frame_top_offset
            .saturating_add(usize::from(geometry.row))
            .saturating_add(pane_frame_top_offset);
        let prompt_row_start =
            body_row.saturating_add(content_rows.saturating_sub(block.prompt_lines.len()));
        return Ok((
            prompt_row_start.saturating_add(block.cursor_row),
            usize::from(geometry.column).saturating_add(block.cursor_column),
            block.cursor_visible,
        ));
    }
    let max_cursor_row = content_rows.saturating_sub(1);
    let max_cursor_column = content_columns.saturating_sub(1);
    let screen = screens.get(&active_pane.id.to_string());
    let cursor = screen
        .map(TerminalScreen::cursor_state)
        .unwrap_or(crate::terminal::TerminalCursorState { row: 0, column: 0 });
    let cursor_visible = screen.map(TerminalScreen::cursor_visible).unwrap_or(true);
    let row = window_frame_top_offset
        .saturating_add(usize::from(geometry.row))
        .saturating_add(pane_frame_top_offset)
        .saturating_add(cursor.row.min(max_cursor_row));
    let column = usize::from(geometry.column).saturating_add(cursor.column.min(max_cursor_column));
    Ok((row, column, cursor_visible))
}

/// Runs the active agent prompt region operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn active_agent_prompt_region(
    window: &Window,
    config: &TerminalClientLoopConfig,
    role: ClientViewRole,
) -> Result<Option<ReadlinePromptRegion>> {
    let active_index = window.active_pane_index();
    let Some(active_pane) = window.panes().get(active_index) else {
        return Ok(None);
    };
    if !pane_agent_shell_visible(&config.frame_context, active_pane.id.as_str()) {
        return Ok(None);
    }
    active_pane_render_region(window, config, role)
}

/// Runs the active pane render region operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn active_pane_render_region(
    window: &Window,
    config: &TerminalClientLoopConfig,
    role: ClientViewRole,
) -> Result<Option<ReadlinePromptRegion>> {
    if role != ClientViewRole::Primary {
        return Ok(None);
    }
    let active_index = window.active_pane_index();
    let Some(active_pane) = window.panes().get(active_index) else {
        return Ok(None);
    };
    let body_size = window_body_size(window.size, config.window_frames_enabled)?;
    let geometries = if window.zoomed_pane_id() == Some(&active_pane.id) {
        vec![zoomed_pane_geometry(body_size)]
    } else {
        rendered_pane_geometries(window, config.window_frames_enabled)?
    };
    let geometry = geometries
        .iter()
        .copied()
        .find(|geometry| geometry.index == active_index)
        .unwrap_or(PaneGeometry {
            index: active_index,
            column: 0,
            row: 0,
            columns: active_pane.size.columns,
            rows: active_pane.size.rows,
        });
    let pane_frame_top_offset = usize::from(
        config.pane_frames_enabled
            && config.pane_frame_position == TerminalFramePosition::Top
            && !pane_frame_merges_into_divider(&geometry, &geometries, config.pane_frame_position),
    );
    let content_size = pane_content_size_for_geometry(
        &geometry,
        &geometries,
        config.pane_frames_enabled,
        config.pane_frame_position,
    )?;
    let rows = usize::from(content_size.rows);
    let columns = usize::from(content_size.columns);
    if rows == 0 || columns == 0 {
        return Ok(None);
    }
    let window_frame_top_offset = usize::from(
        config.window_frames_enabled && config.window_frame_position == TerminalFramePosition::Top,
    );
    Ok(Some(ReadlinePromptRegion {
        row: window_frame_top_offset
            .saturating_add(usize::from(geometry.row))
            .saturating_add(pane_frame_top_offset),
        column: usize::from(geometry.column),
        columns,
        rows,
    }))
}

/// Reconciles the active agent prompt text with the authoritative prompt
/// region used by cursor placement.
///
/// The default attach path can resize the window before every render layer has
/// observed the new pane dimensions. The cursor path computes from the window
/// geometry, while pane-line composition may still contain a stale prompt block
/// from the prior pane height. This pass treats the computed prompt region as
/// authoritative, removes any previously styled agent prompt cells inside that
/// pane body, and overlays the current prompt block at the bottom of the active
/// pane.
fn align_active_agent_prompt_block_to_region(
    window: &Window,
    config: &TerminalClientLoopConfig,
    role: ClientViewRole,
    agent_prompt_region: Option<ReadlinePromptRegion>,
    lines: &mut [String],
    line_style_spans: &mut [Vec<TerminalStyleSpan>],
) {
    if role != ClientViewRole::Primary {
        return;
    }
    let Some(region) = agent_prompt_region else {
        return;
    };
    let Some(region) = clipped_prompt_region(region, usize::from(window.size.columns), lines.len())
    else {
        return;
    };
    let active_index = window.active_pane_index();
    let Some(active_pane) = window.panes().get(active_index) else {
        return;
    };
    if !pane_agent_shell_visible(&config.frame_context, active_pane.id.as_str()) {
        return;
    }

    let pane_context = config.frame_context.panes.get(active_pane.id.as_str());
    let block = render_agent_prompt_block(region.columns, region.rows, pane_context);
    let transparent = pane_agent_prompt_transparent(&config.frame_context, active_pane.id.as_str());
    let prompt_lines = if transparent {
        block.transparent_prompt_styled_lines(region.columns)
    } else {
        block.prompt_styled_lines(
            region.columns,
            &config.ui_theme,
            config.frame_context.animation_tick_ms,
        )
    };
    let display_lines = if transparent {
        Vec::new()
    } else {
        block.display_styled_lines(
            region.columns,
            &config.ui_theme,
            config.frame_context.animation_tick_ms,
        )
    };
    if prompt_lines.is_empty() && display_lines.is_empty() {
        return;
    }

    clear_stale_agent_prompt_segments(lines, line_style_spans, region, &config.ui_theme);
    let visible_prompt_count = prompt_lines.len().min(region.rows);
    let prompt_start_row = region
        .row
        .saturating_add(region.rows.saturating_sub(visible_prompt_count));
    let display_targets = active_agent_display_overlay_targets(
        lines,
        region.row,
        prompt_start_row,
        &display_lines,
        |line| line_segment_is_blank(line, region.column, region.columns),
    );
    let display_source_start = display_lines.len().saturating_sub(display_targets.len());
    for (row, styled_line) in display_targets
        .into_iter()
        .zip(display_lines[display_source_start..].iter())
    {
        overlay_styled_prompt_line(
            lines,
            line_style_spans,
            row,
            region.column,
            region.columns,
            styled_line,
        );
    }
    for (offset, styled_line) in prompt_lines
        .iter()
        .skip(prompt_lines.len().saturating_sub(visible_prompt_count))
        .enumerate()
    {
        overlay_styled_prompt_line(
            lines,
            line_style_spans,
            prompt_start_row.saturating_add(offset),
            region.column,
            region.columns,
            styled_line,
        );
    }
}

/// Clears stale prompt and display-overlay cells from a pane body without
/// disturbing normal terminal content in the same region.
///
/// The live footer uses theme-relative animated foreground spans instead of the
/// normal display-overlay rendition, so it is identified by its owned text
/// shape rather than by style alone.
fn clear_stale_agent_prompt_segments(
    lines: &mut [String],
    line_style_spans: &mut [Vec<TerminalStyleSpan>],
    region: ReadlinePromptRegion,
    ui_theme: &UiTheme,
) {
    let prompt_rendition = agent_prompt_input_rendition(ui_theme);
    let display_rendition = display_overlay_text_rendition(ui_theme);
    let region_start = region.column;
    let region_end = region.column.saturating_add(region.columns);
    for row in region.row..region.row.saturating_add(region.rows) {
        let clear_live_footer_segment = lines.get(row).is_some_and(|line| {
            line_segment_is_agent_live_footer(line, region.column, region.columns)
        });
        if clear_live_footer_segment && let Some(line) = lines.get_mut(row) {
            write_line_segment(line, region_start, region.columns, "");
        }
        let Some(spans) = line_style_spans.get_mut(row) else {
            continue;
        };
        let mut retained = Vec::with_capacity(spans.len());
        for span in std::mem::take(spans) {
            if (!clear_live_footer_segment
                && !style_span_is_agent_prompt_block(span, prompt_rendition, display_rendition))
                || !style_span_overlaps_columns(span, region_start, region_end)
            {
                retained.push(span);
                continue;
            }
            let span_end = span.start.saturating_add(span.length);
            let overlap_start = span.start.max(region_start);
            let overlap_end = span_end.min(region_end);
            if overlap_start < overlap_end
                && let Some(line) = lines.get_mut(row)
            {
                write_line_segment(
                    line,
                    overlap_start,
                    overlap_end.saturating_sub(overlap_start),
                    "",
                );
            }
            retained.extend(style_span_segments_outside_range(
                span,
                overlap_start,
                overlap_end,
            ));
        }
        *spans = retained;
    }
}

/// Reports whether a rendered line segment is Mezzanine-owned live footer text.
fn line_segment_is_agent_live_footer(line: &str, column: usize, width: usize) -> bool {
    let segment = line_slice(line, column, column.saturating_add(width));
    agent_live_footer_state_label(segment.trim_end()).is_some()
}

/// Overlays one styled prompt line at the requested row and column range.
fn overlay_styled_prompt_line(
    lines: &mut [String],
    line_style_spans: &mut [Vec<TerminalStyleSpan>],
    row: usize,
    column: usize,
    width: usize,
    styled_line: &TerminalStyledLine,
) {
    let Some(line) = lines.get_mut(row) else {
        return;
    };
    write_line_segment(line, column, width, &styled_line.text);
    let Some(spans) = line_style_spans.get_mut(row) else {
        return;
    };
    let region_end = column.saturating_add(width);
    let mut retained =
        Vec::with_capacity(spans.len().saturating_add(styled_line.style_spans.len()));
    for span in std::mem::take(spans) {
        if style_span_overlaps_columns(span, column, region_end) {
            retained.extend(style_span_segments_outside_range(span, column, region_end));
        } else {
            retained.push(span);
        }
    }
    retained.extend(
        styled_line
            .style_spans
            .iter()
            .filter_map(|span| clip_style_span(*span, width))
            .map(|span| offset_style_span(span, column)),
    );
    *spans = retained;
}

/// Chooses active-pane display overlay rows while keeping the live footer at
/// the bottom edge of the prompt region.
fn active_agent_display_overlay_targets(
    lines: &[String],
    content_start: usize,
    content_end: usize,
    display_lines: &[TerminalStyledLine],
    is_blank: impl Fn(&String) -> bool,
) -> Vec<usize> {
    if display_lines.is_empty() || content_start >= content_end {
        return Vec::new();
    }
    let Some(last_display) = display_lines.last() else {
        return Vec::new();
    };
    if agent_live_footer_state_label(last_display.text.trim_end()).is_none() {
        return agent_display_overlay_targets(
            lines,
            content_start,
            content_end,
            display_lines.len(),
            is_blank,
        );
    }

    let footer_row = content_end.saturating_sub(1);
    let preceding_targets = agent_display_overlay_targets(
        lines,
        content_start,
        footer_row,
        display_lines.len().saturating_sub(1),
        is_blank,
    );
    preceding_targets
        .into_iter()
        .chain(std::iter::once(footer_row))
        .collect()
}

/// Reports whether a rendered line segment contains only blank cells.
fn line_segment_is_blank(line: &str, column: usize, width: usize) -> bool {
    line_slice(line, column, column.saturating_add(width))
        .chars()
        .all(char::is_whitespace)
}

/// Returns whether a style span touches a half-open column range.
fn style_span_overlaps_columns(span: TerminalStyleSpan, start: usize, end: usize) -> bool {
    span.start < end && span.start.saturating_add(span.length) > start
}

/// Identifies prompt-block styles that should not survive a resize mismatch.
fn style_span_is_agent_prompt_block(
    span: TerminalStyleSpan,
    prompt_rendition: GraphicRendition,
    display_rendition: GraphicRendition,
) -> bool {
    span.rendition == prompt_rendition || span.rendition == display_rendition
}

/// Keeps the parts of a style span that fall outside a replaced column range.
fn style_span_segments_outside_range(
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

/// Runs the compose client presentation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_client_presentation(
    view: &RenderedClientView,
    status: Option<&ClientStatusLine>,
) -> Vec<String> {
    compose_client_presentation_with_styles(view, status).0
}

/// Runs the compose client presentation with styles operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_client_presentation_with_styles(
    view: &RenderedClientView,
    status: Option<&ClientStatusLine>,
) -> (Vec<String>, Vec<Vec<TerminalStyleSpan>>) {
    let (target_rows, target_columns) = if view.requires_client_scroll {
        (
            usize::from(view.client_size.rows),
            usize::from(view.client_size.columns),
        )
    } else {
        (
            usize::from(view.authoritative_size.rows),
            usize::from(view.authoritative_size.columns),
        )
    };
    let row_offset = if view.requires_client_scroll {
        view.viewport_row.min(max_viewport_row(view))
    } else {
        0
    };
    let column_offset = if view.requires_client_scroll {
        view.viewport_column.min(max_viewport_column(view))
    } else {
        0
    };
    let mut lines: Vec<String> = view
        .lines
        .iter()
        .skip(row_offset)
        .take(target_rows)
        .map(|line| slice_terminal_line(line, column_offset, target_columns))
        .collect();
    let mut line_style_spans: Vec<Vec<TerminalStyleSpan>> = view
        .line_style_spans
        .iter()
        .skip(row_offset)
        .take(target_rows)
        .map(|spans| clipped_style_spans(spans, column_offset, target_columns))
        .collect();
    while lines.len() < target_rows {
        lines.push(" ".repeat(target_columns));
        line_style_spans.push(Vec::new());
    }
    if let Some(status) = status
        && target_rows > 0
    {
        let prefix = match status.kind {
            ClientStatusKind::Plain => "",
            ClientStatusKind::CopyMode => "copy: ",
            ClientStatusKind::PendingObserver => "observer: ",
            ClientStatusKind::Diagnostic => "status: ",
        };
        lines[target_rows - 1] = fit_width(&format!("{prefix}{}", status.text), target_columns);
        line_style_spans[target_rows - 1].clear();
        if target_columns > 0 {
            line_style_spans[target_rows - 1].push(TerminalStyleSpan {
                start: 0,
                length: target_columns,
                rendition: status_line_rendition(status.kind, &view.ui_theme),
            });
        }
    }
    (lines, line_style_spans)
}

/// Runs the apply client view offset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn apply_client_view_offset(view: &mut RenderedClientView, row: usize, column: usize) {
    if view.requires_client_scroll {
        view.viewport_row = row.min(max_viewport_row(view));
        view.viewport_column = column.min(max_viewport_column(view));
    } else {
        view.viewport_row = 0;
        view.viewport_column = 0;
    }
}

/// Runs the max viewport row operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn max_viewport_row(view: &RenderedClientView) -> usize {
    usize::from(view.authoritative_size.rows).saturating_sub(usize::from(view.client_size.rows))
}

/// Runs the max viewport column operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn max_viewport_column(view: &RenderedClientView) -> usize {
    usize::from(view.authoritative_size.columns)
        .saturating_sub(usize::from(view.client_size.columns))
}

/// Runs the slice terminal line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn slice_terminal_line(line: &str, column_offset: usize, width: usize) -> String {
    line_slice(line, column_offset, column_offset.saturating_add(width))
}

/// Runs the clipped style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn clipped_style_spans(
    spans: &[TerminalStyleSpan],
    column_offset: usize,
    width: usize,
) -> Vec<TerminalStyleSpan> {
    let end = column_offset.saturating_add(width);
    spans
        .iter()
        .filter_map(|span| {
            let span_start = span.start;
            let span_end = span.start.saturating_add(span.length);
            let clipped_start = span_start.max(column_offset);
            let clipped_end = span_end.min(end);
            (clipped_start < clipped_end).then(|| TerminalStyleSpan {
                start: clipped_start.saturating_sub(column_offset),
                length: clipped_end.saturating_sub(clipped_start),
                rendition: span.rendition,
            })
        })
        .collect()
}

/// Visual prefix applied to Mezzanine-owned UI lines (status bars, prompts,
/// command overlays) so users can distinguish them from agent-controlled
/// terminal output. Terminal content never receives this prefix.
const MEZ_UI_PREFIX: &str = "▐ ";

/// Clamps a zero-based visible cursor column into the addressable cells of a
/// rendered row. Terminal cursor addressing is one-based and cannot represent a
/// visible insertion point after the final cell without relying on emulator
/// autowrap behavior.
fn clamp_visible_cursor_column(column: usize, width: usize) -> usize {
    column.min(width.saturating_sub(1))
}

/// Runs the render readline prompt status row operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn render_readline_prompt_status_row(
    prompt: &ReadlinePrompt,
    width: usize,
) -> ReadlinePromptStatusRow {
    let raw_cursor_column = prompt.rendered_cursor_column();
    let cursor_column = raw_cursor_column
        .saturating_add(2)
        .min(width.saturating_sub(1));
    ReadlinePromptStatusRow {
        status: ClientStatusLine {
            kind: ClientStatusKind::Plain,
            text: format!(
                "{MEZ_UI_PREFIX}{}",
                fit_width(&prompt.render_with_shadow_hint(), width.saturating_sub(2))
            ),
        },
        cursor_column,
        cursor_visible: width > 0 && raw_cursor_column <= width.saturating_sub(2),
    }
}

/// Runs the compose readline prompt client presentation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_readline_prompt_client_presentation(
    view: &RenderedClientView,
    prompt: &ReadlinePrompt,
) -> ReadlinePromptClientPresentation {
    let width = usize::from(view.authoritative_size.columns);
    let row = render_readline_prompt_status_row(prompt, width);
    let (lines, mut line_style_spans) =
        compose_client_presentation_with_styles(view, Some(&row.status));
    if let Some(last) = line_style_spans.last_mut() {
        let presentation_width = lines
            .last()
            .map(|line| line.chars().count())
            .unwrap_or(width);
        if prompt.kind == ReadlinePromptKind::Agent && presentation_width > 0 {
            last.clear();
            last.push(TerminalStyleSpan {
                start: 0,
                length: presentation_width,
                rendition: agent_prompt_input_rendition(&view.ui_theme),
            });
        }
        if let Some(span) =
            prompt_shadow_hint_style_span(prompt, 2, presentation_width, &view.ui_theme)
        {
            last.push(span);
        }
    }
    ReadlinePromptClientPresentation {
        lines,
        line_style_spans,
        cursor_row: usize::from(view.authoritative_size.rows.saturating_sub(1)),
        cursor_column: row.cursor_column,
        cursor_visible: row.cursor_visible,
    }
}

/// Runs the compose prompt overlay lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_overlay_lines(
    base_lines: &[String],
    prompt: &ReadlinePrompt,
    client_size: Size,
) -> Vec<String> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let status_row = render_readline_prompt_status_row(prompt, width);
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
    if let Some(last) = lines.last_mut() {
        *last = fit_width(&status_row.status.text, width);
    }
    lines
}

/// Runs the compose prompt overlay presentation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_overlay_presentation(
    base_lines: &[String],
    prompt: &ReadlinePrompt,
    client_size: Size,
) -> ReadlinePromptClientPresentation {
    compose_prompt_overlay_presentation_with_styles(
        base_lines,
        &[],
        prompt,
        client_size,
        &UiTheme::default(),
    )
}

/// Runs the compose prompt overlay presentation with styles operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_overlay_presentation_with_styles(
    base_lines: &[String],
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    prompt: &ReadlinePrompt,
    client_size: Size,
    ui_theme: &UiTheme,
) -> ReadlinePromptClientPresentation {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let status_row = render_readline_prompt_status_row(prompt, width);
    let lines = compose_prompt_overlay_lines(base_lines, prompt, client_size);
    let mut line_style_spans = normalize_overlay_style_spans(base_line_style_spans, rows, width);
    line_style_spans.truncate(rows);
    while line_style_spans.len() < rows {
        line_style_spans.push(Vec::new());
    }
    if let Some(last) = line_style_spans.last_mut() {
        last.clear();
        if width > 0 {
            last.push(TerminalStyleSpan {
                start: 0,
                length: width,
                rendition: prompt_region_rendition(prompt, ui_theme),
            });
            if let Some(span) = prompt_shadow_hint_style_span(prompt, 2, width, ui_theme) {
                last.push(span);
            }
        }
    }
    ReadlinePromptClientPresentation {
        lines,
        line_style_spans,
        cursor_row: rows.saturating_sub(1),
        cursor_column: status_row.cursor_column,
        cursor_visible: status_row.cursor_visible,
    }
}

/// Runs the compose prompt region presentation with styles operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_prompt_region_presentation_with_styles(
    base_lines: &[String],
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    prompt: &ReadlinePrompt,
    client_size: Size,
    region: ReadlinePromptRegion,
    ui_theme: &UiTheme,
) -> ReadlinePromptClientPresentation {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
    let mut line_style_spans = normalize_overlay_style_spans(base_line_style_spans, rows, width);
    line_style_spans.truncate(rows);
    while line_style_spans.len() < rows {
        line_style_spans.push(Vec::new());
    }

    let region = clipped_prompt_region(region, width, rows);
    let Some(region) = region else {
        return ReadlinePromptClientPresentation {
            lines,
            line_style_spans,
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        };
    };
    let layout = render_wrapped_prompt_layout(prompt, region.columns, region.rows.clamp(1, 6));
    let prompt_row_start = if prompt.kind == ReadlinePromptKind::Agent && layout.lines.len() > 1 {
        region.row
    } else {
        region
            .row
            .saturating_add(region.rows.saturating_sub(layout.lines.len()))
    };
    for (offset, prompt_line) in layout.lines.iter().enumerate() {
        let row = prompt_row_start.saturating_add(offset);
        if row >= lines.len() {
            continue;
        }
        write_line_segment(&mut lines[row], region.column, region.columns, prompt_line);
        line_style_spans[row].retain(|span| {
            span.start.saturating_add(span.length) <= region.column
                || span.start >= region.column.saturating_add(region.columns)
        });
        line_style_spans[row].push(TerminalStyleSpan {
            start: region.column,
            length: region.columns,
            rendition: prompt_region_rendition(prompt, ui_theme),
        });
        for shadow_span in layout.shadow_spans.get(offset).into_iter().flatten() {
            if shadow_span.start >= region.columns {
                continue;
            }
            let length = shadow_span
                .length
                .min(region.columns.saturating_sub(shadow_span.start));
            if length == 0 {
                continue;
            }
            line_style_spans[row].push(TerminalStyleSpan {
                start: region.column.saturating_add(shadow_span.start),
                length,
                rendition: prompt_shadow_hint_rendition(prompt, ui_theme),
            });
        }
    }
    ReadlinePromptClientPresentation {
        lines,
        line_style_spans,
        cursor_row: prompt_row_start.saturating_add(layout.cursor_row),
        cursor_column: region.column.saturating_add(layout.cursor_column),
        cursor_visible: layout.cursor_visible,
    }
}

/// Returns the number of pane body rows reserved by the pane-local agent prompt.
pub fn agent_prompt_reserved_line_count(
    width: usize,
    body_rows: usize,
    pane_context: Option<&TerminalPaneFrameContext>,
) -> usize {
    if !pane_agent_prompt_space_reserved(pane_context) {
        return 0;
    }
    render_agent_prompt_block(width, body_rows, pane_context).reserved_line_count()
}

/// Runs the prompt region rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prompt_region_rendition(prompt: &ReadlinePrompt, ui_theme: &UiTheme) -> GraphicRendition {
    if prompt.kind == ReadlinePromptKind::Agent {
        agent_prompt_input_rendition(ui_theme)
    } else {
        ui_theme.colors.prompt.rendition()
    }
}

/// Runs the prompt shadow hint rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prompt_shadow_hint_rendition(prompt: &ReadlinePrompt, ui_theme: &UiTheme) -> GraphicRendition {
    let mut rendition = prompt_region_rendition(prompt, ui_theme);
    rendition.foreground = Some(prompt_shadow_foreground(prompt, ui_theme));
    rendition.dim = true;
    rendition
}

/// Returns the contrast-managed shadow-hint rendition for pane-local agent prompts.
fn agent_prompt_shadow_hint_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    let background = ui_theme.colors.agent_prompt.background;
    let mut rendition = agent_prompt_input_rendition(ui_theme);
    rendition.foreground = Some(
        readable_prompt_shadow_gray(background).unwrap_or(ui_theme.colors.agent_prompt.foreground),
    );
    rendition.dim = true;
    rendition
}

/// Returns the contrast-managed rendition for pane-local agent prompt input.
fn agent_prompt_input_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    let background = ui_theme.colors.agent_prompt.background;
    GraphicRendition {
        foreground: Some(contrasting_binary_foreground(background)),
        background: Some(background),
        ..GraphicRendition::default()
    }
}

/// Returns a readable shaded foreground for completion shadow text.
fn prompt_shadow_foreground(prompt: &ReadlinePrompt, ui_theme: &UiTheme) -> TerminalColor {
    let background = if prompt.kind == ReadlinePromptKind::Agent {
        ui_theme.colors.agent_prompt.background
    } else {
        ui_theme.colors.prompt.background
    };
    readable_prompt_shadow_gray(background).unwrap_or_else(|| {
        if prompt.kind == ReadlinePromptKind::Agent {
            ui_theme.colors.agent_prompt.foreground
        } else {
            ui_theme.colors.prompt.foreground
        }
    })
}

/// Returns the lowest-emphasis grey that still reads against a prompt surface.
fn readable_prompt_shadow_gray(background: TerminalColor) -> Option<TerminalColor> {
    let background_luminance = terminal_color_relative_luminance(background)?;
    if background_luminance >= 0.5 {
        for level in (0..=255).rev() {
            let candidate = terminal_gray(level);
            if terminal_color_contrast_ratio(candidate, background)
                .is_some_and(|ratio| ratio >= MIN_PROMPT_SHADOW_CONTRAST_RATIO)
            {
                return Some(candidate);
            }
        }
    } else {
        for level in 0..=255 {
            let candidate = terminal_gray(level);
            if terminal_color_contrast_ratio(candidate, background)
                .is_some_and(|ratio| ratio >= MIN_PROMPT_SHADOW_CONTRAST_RATIO)
            {
                return Some(candidate);
            }
        }
    }
    None
}

/// Returns a text-only rendition for Mezzanine-authored pane and overlay text.
///
/// These surfaces should color foreground glyphs without painting a background
/// over terminal content. Interactive controls such as prompts, status bars,
/// buttons, and selectors keep using their full color pair renditions.
fn text_foreground_rendition(pair: UiColorPair) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(pair.foreground),
        ..GraphicRendition::default()
    }
}

/// Returns the display-overlay foreground rendition used for non-interactive
/// command output, help text, and pane-local reference output.
fn display_overlay_text_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    text_foreground_rendition(ui_theme.colors.display_overlay)
}

/// Runs the prompt shadow hint style span operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prompt_shadow_hint_style_span(
    prompt: &ReadlinePrompt,
    rendered_column_offset: usize,
    width: usize,
    ui_theme: &UiTheme,
) -> Option<TerminalStyleSpan> {
    let (start, length) = prompt.rendered_shadow_hint_columns()?;
    let start = start.saturating_add(rendered_column_offset);
    let end = start.saturating_add(length).min(width);
    (start < end).then_some(TerminalStyleSpan {
        start,
        length: end.saturating_sub(start),
        rendition: prompt_shadow_hint_rendition(prompt, ui_theme),
    })
}

/// Runs the compose display region overlay lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_display_region_overlay_lines(
    base_lines: &[String],
    display_lines: &[String],
    client_size: Size,
    region: ReadlinePromptRegion,
) -> Vec<String> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
    let Some(region) = clipped_prompt_region(region, width, rows) else {
        return lines;
    };
    let display_capacity = region.rows.saturating_sub(1).max(1);
    let visible_count = display_lines.len().min(display_capacity);
    let start = display_lines.len().saturating_sub(visible_count);
    let row_start = region
        .row
        .saturating_add(region.rows.saturating_sub(visible_count.saturating_add(1)));
    for (offset, line) in display_lines
        .iter()
        .skip(start)
        .take(visible_count)
        .enumerate()
    {
        let row = row_start.saturating_add(offset);
        if row < lines.len() {
            write_line_segment(&mut lines[row], region.column, region.columns, line);
        }
    }
    lines
}

/// Runs the compose display region overlay line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_display_region_overlay_line_style_spans(
    base_line_style_spans: &[Vec<TerminalStyleSpan>],
    display_lines: &[String],
    client_size: Size,
    region: ReadlinePromptRegion,
    ui_theme: &UiTheme,
) -> Vec<Vec<TerminalStyleSpan>> {
    let width = usize::from(client_size.columns);
    let rows = usize::from(client_size.rows);
    let mut line_style_spans = normalize_overlay_style_spans(base_line_style_spans, rows, width);
    let Some(region) = clipped_prompt_region(region, width, rows) else {
        return line_style_spans;
    };
    let display_capacity = region.rows.saturating_sub(1).max(1);
    let visible_count = display_lines.len().min(display_capacity);
    let start = display_lines.len().saturating_sub(visible_count);
    let row_start = region
        .row
        .saturating_add(region.rows.saturating_sub(visible_count.saturating_add(1)));
    for offset in 0..visible_count {
        let row = row_start.saturating_add(offset);
        if row >= line_style_spans.len() {
            continue;
        }
        line_style_spans[row].retain(|span| {
            span.start.saturating_add(span.length) <= region.column
                || span.start >= region.column.saturating_add(region.columns)
        });
        let display_line = &display_lines[start + offset];
        let footer_spans =
            agent_live_footer_style_spans(display_line, region.columns, 0, ui_theme, None);
        if footer_spans.is_empty() {
            line_style_spans[row].push(TerminalStyleSpan {
                start: region.column,
                length: overlay_text_style_width(display_line, region.columns),
                rendition: display_overlay_text_rendition(ui_theme),
            });
        } else {
            line_style_spans[row].extend(
                footer_spans
                    .into_iter()
                    .map(|span| offset_style_span(span, region.column)),
            );
        }
    }
    line_style_spans
}

/// Carries Wrapped Prompt Layout state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WrappedPromptLayout {
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    lines: Vec<String>,
    /// Stores the shadow spans value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    shadow_spans: Vec<Vec<PromptShadowSpan>>,
    /// Stores the cursor row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_row: usize,
    /// Stores the cursor column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_column: usize,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_visible: bool,
}

/// Carries Prompt Shadow Span state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PromptShadowSpan {
    /// Stores the start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    start: usize,
    /// Stores the length value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    length: usize,
}

/// Runs the render wrapped prompt layout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_wrapped_prompt_layout(
    prompt: &ReadlinePrompt,
    width: usize,
    max_rows: usize,
) -> WrappedPromptLayout {
    if width == 0 || max_rows == 0 {
        return WrappedPromptLayout {
            lines: Vec::new(),
            shadow_spans: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        };
    }
    let raw_line = format!("{MEZ_UI_PREFIX}{}", prompt.render_with_shadow_hint());
    let raw_cursor_index = prompt.rendered_cursor_column().saturating_add(2);
    let raw_shadow_range = prompt
        .rendered_shadow_hint_columns()
        .map(|(start, length)| (start.saturating_add(2), start.saturating_add(2 + length)));
    let continuation_indent =
        if prompt.kind == ReadlinePromptKind::Agent && !prompt.reverse_search_active() {
            terminal_text_width(&format!("{MEZ_UI_PREFIX}agent> ")).min(width.saturating_sub(1))
        } else {
            0
        };
    let (chunks, chunk_shadow_spans, cursor_row, cursor_column) =
        wrap_prompt_line_with_cursor_and_shadow(
            &raw_line,
            raw_cursor_index,
            raw_shadow_range,
            width,
            continuation_indent,
        );
    let first_visible_chunk = chunks.len().saturating_sub(max_rows);
    let visible_chunks = chunks
        .iter()
        .skip(first_visible_chunk)
        .take(max_rows)
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    let mut visible_shadow_spans = chunk_shadow_spans
        .iter()
        .skip(first_visible_chunk)
        .take(max_rows)
        .cloned()
        .collect::<Vec<_>>();
    let cursor_visible = cursor_row >= first_visible_chunk
        && cursor_row < first_visible_chunk + visible_chunks.len();
    let mut lines = visible_chunks;
    let mut cursor_column = cursor_column;
    if should_show_prompt_length_note(prompt, width, max_rows)
        && let Some(first) = lines.first_mut()
    {
        let note = format!(
            "{MEZ_UI_PREFIX}agent> [{} chars pasted]",
            prompt.buffer.line().chars().count()
        );
        *first = fit_width(&note, width);
        if let Some(first_spans) = visible_shadow_spans.first_mut() {
            first_spans.clear();
        }
        cursor_column = width;
    }
    let cursor_column = clamp_visible_cursor_column(cursor_column, width);
    WrappedPromptLayout {
        lines,
        shadow_spans: visible_shadow_spans,
        cursor_row: cursor_row.saturating_sub(first_visible_chunk),
        cursor_column,
        cursor_visible,
    }
}

/// Runs the should show prompt length note operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn should_show_prompt_length_note(prompt: &ReadlinePrompt, width: usize, max_rows: usize) -> bool {
    prompt.kind == ReadlinePromptKind::Agent
        && char_count(prompt.buffer.line()) > width.saturating_mul(max_rows).max(160)
}

/// Runs the wrap prompt line with cursor and shadow operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wrap_prompt_line_with_cursor_and_shadow(
    value: &str,
    cursor_index: usize,
    shadow_range: Option<(usize, usize)>,
    width: usize,
    continuation_indent: usize,
) -> (Vec<String>, Vec<Vec<PromptShadowSpan>>, usize, usize) {
    let mut chunks = Vec::new();
    let mut chunk_shadow_spans = Vec::new();
    let mut current = String::new();
    let mut current_shadow_spans = Vec::new();
    let mut used = 0usize;
    let mut cursor = None;
    let mut last_space_break: Option<(usize, usize, Vec<PromptShadowSpan>)> = None;
    let continuation_prefix = " ".repeat(continuation_indent);
    for (index, ch) in value.chars().enumerate() {
        if ch == '\n' {
            if cursor.is_none() && index == cursor_index {
                cursor = Some((chunks.len(), used));
            }
            chunks.push(current);
            chunk_shadow_spans.push(current_shadow_spans);
            current = continuation_prefix.clone();
            current_shadow_spans = Vec::new();
            used = continuation_indent;
            last_space_break = None;
            continue;
        }
        let ch_width = terminal_char_width(ch).max(1);
        if used > 0 && used.saturating_add(ch_width) > width {
            if let Some((text_break, consumed_break, spans_at_break)) = last_space_break.take() {
                let consumed_columns = terminal_text_width(&current[..consumed_break]);
                if consumed_columns > continuation_indent {
                    let wrapped = current[..text_break].to_string();
                    let remainder = current[consumed_break..].to_string();
                    chunks.push(wrapped);
                    chunk_shadow_spans.push(spans_at_break);
                    current = format!("{continuation_prefix}{remainder}");
                    current_shadow_spans = prompt_shadow_spans_after_consumed(
                        &current_shadow_spans,
                        consumed_columns,
                        continuation_indent,
                    );
                    used = terminal_text_width(&current);
                } else {
                    chunks.push(current);
                    chunk_shadow_spans.push(current_shadow_spans);
                    current = continuation_prefix.clone();
                    current_shadow_spans = Vec::new();
                    used = continuation_indent;
                }
            } else {
                chunks.push(current);
                chunk_shadow_spans.push(current_shadow_spans);
                current = continuation_prefix.clone();
                current_shadow_spans = Vec::new();
                used = continuation_indent;
            }
        }
        if cursor.is_none() && index == cursor_index {
            cursor = Some((chunks.len(), used));
        }
        let current_byte_len = current.len();
        current.push(ch);
        if shadow_range.is_some_and(|(start, end)| index >= start && index < end) {
            push_prompt_shadow_cell(&mut current_shadow_spans, used, ch_width);
        }
        used = used.saturating_add(ch_width);
        if ch.is_whitespace() && used > 0 {
            last_space_break = Some((
                current_byte_len,
                current.len(),
                current_shadow_spans.clone(),
            ));
        }
    }
    if cursor.is_none() && value.chars().count() == cursor_index {
        cursor = Some((chunks.len(), used));
    }
    chunks.push(current);
    chunk_shadow_spans.push(current_shadow_spans);
    let (cursor_row, cursor_column) = cursor.unwrap_or((chunks.len().saturating_sub(1), 0));
    (chunks, chunk_shadow_spans, cursor_row, cursor_column)
}

/// Returns prompt-shadow spans after one wrapped prefix is consumed.
fn prompt_shadow_spans_after_consumed(
    spans: &[PromptShadowSpan],
    consumed_columns: usize,
    shift_columns: usize,
) -> Vec<PromptShadowSpan> {
    spans
        .iter()
        .filter_map(|span| {
            let end = span.start.saturating_add(span.length);
            if end <= consumed_columns {
                None
            } else {
                Some(PromptShadowSpan {
                    start: span
                        .start
                        .saturating_sub(consumed_columns)
                        .saturating_add(shift_columns),
                    length: end.saturating_sub(consumed_columns.max(span.start)),
                })
            }
        })
        .collect()
}

/// Runs the push prompt shadow cell operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_prompt_shadow_cell(
    current_shadow_spans: &mut Vec<PromptShadowSpan>,
    start: usize,
    length: usize,
) {
    if let Some(last) = current_shadow_spans.last_mut()
        && last.start.saturating_add(last.length) == start
    {
        last.length = last.length.saturating_add(length);
        return;
    }
    current_shadow_spans.push(PromptShadowSpan { start, length });
}

/// Runs the clipped prompt region operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn clipped_prompt_region(
    region: ReadlinePromptRegion,
    client_width: usize,
    client_rows: usize,
) -> Option<ReadlinePromptRegion> {
    if region.row >= client_rows || region.column >= client_width {
        return None;
    }
    let columns = region
        .columns
        .min(client_width.saturating_sub(region.column));
    let rows = region.rows.min(client_rows.saturating_sub(region.row));
    (columns > 0 && rows > 0).then_some(ReadlinePromptRegion {
        row: region.row,
        column: region.column,
        columns,
        rows,
    })
}

/// Runs the write line segment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_line_segment(line: &mut String, column: usize, width: usize, value: &str) {
    if width == 0 {
        return;
    }
    let target_end = column.saturating_add(width);
    let original = line.clone();
    let mut output = String::new();
    let mut current_column = 0usize;
    for grapheme in terminal_graphemes(&original) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        let next_column = current_column.saturating_add(grapheme_width);
        if next_column <= column {
            output.push_str(grapheme);
            current_column = next_column;
            continue;
        }
        break;
    }
    let output_width = terminal_text_width(&output);
    if output_width < column {
        output.push_str(&" ".repeat(column.saturating_sub(output_width)));
    }
    let fitted = fit_width(value, width);
    output.push_str(&fitted);
    current_column = 0;
    for grapheme in terminal_graphemes(&original) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        if current_column >= target_end {
            output.push_str(grapheme);
        }
        current_column = current_column.saturating_add(grapheme_width);
    }
    *line = output;
}

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
    let mut lines = base_lines
        .iter()
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    lines.truncate(rows);
    while lines.len() < rows {
        lines.push(" ".repeat(width));
    }
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
fn overlay_text_style_width(value: &str, max_width: usize) -> usize {
    fitted_text_width(value.trim_end_matches(' '), max_width)
}

/// Runs the status line rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn status_line_rendition(kind: ClientStatusKind, ui_theme: &UiTheme) -> GraphicRendition {
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
fn normalize_overlay_style_spans(
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

/// Runs the draw window from screens operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn draw_window_from_screens(
    window: &Window,
    screens: &BTreeMap<String, TerminalScreen>,
    config: &TerminalClientLoopConfig,
) -> Result<Vec<String>> {
    let render_window = window_with_group_frame_space(window, config)?;
    let pane_inputs = window
        .panes()
        .iter()
        .map(|pane| PaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: screens
                .get(&pane.id.to_string())
                .map(TerminalScreen::visible_lines)
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    render_window_with_pane_frame_template(
        &render_window,
        &pane_inputs,
        &config.frame_context,
        TerminalFrameRenderOptions::plain(
            config.window_frames_enabled,
            &config.window_frame_template,
            config.window_frame_position,
        ),
        TerminalFrameRenderOptions::plain(
            config.pane_frames_enabled,
            &config.pane_frame_template,
            config.pane_frame_position,
        ),
    )
    .map(|mut lines| {
        if let Some(frame) =
            group_frame_text(&config.frame_context, usize::from(window.size.columns))
        {
            place_group_frame(&mut lines, frame, window.size.rows);
        }
        lines
    })
}

/// Renders a window while preserving pane SGR style spans in terminal-cell coordinates.
pub fn draw_styled_window_from_screens(
    window: &Window,
    screens: &BTreeMap<String, TerminalScreen>,
    config: &TerminalClientLoopConfig,
) -> Result<Vec<TerminalStyledLine>> {
    let render_window = window_with_group_frame_space(window, config)?;
    let pane_inputs = window
        .panes()
        .iter()
        .map(|pane| StyledPaneRenderInput {
            pane_id: pane.id.to_string(),
            lines: screens
                .get(&pane.id.to_string())
                .map(TerminalScreen::visible_styled_lines)
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let mut lines = render_styled_window_with_pane_frame_template(
        &render_window,
        &pane_inputs,
        &config.frame_context,
        TerminalFrameRenderOptions::styled(
            config.window_frames_enabled,
            &config.window_frame_template,
            config.window_frame_position,
            config.window_frame_style,
        ),
        TerminalFrameRenderOptions::styled(
            config.pane_frames_enabled,
            &config.pane_frame_template,
            config.pane_frame_position,
            config.pane_frame_style,
        ),
        &config.ui_theme,
    )?;
    if let Some(frame) = styled_group_frame_line(
        &config.frame_context,
        usize::from(window.size.columns),
        config.window_frame_style,
        &config.ui_theme,
    ) {
        place_group_frame(&mut lines, frame, window.size.rows);
    }
    Ok(lines)
}

/// Runs the render window operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn render_window(
    window: &Window,
    pane_inputs: &[PaneRenderInput],
    pane_frames_enabled: bool,
) -> Result<Vec<String>> {
    render_window_with_pane_frame_template(
        window,
        pane_inputs,
        &TerminalFrameContext::default(),
        TerminalFrameRenderOptions::plain(
            false,
            DEFAULT_WINDOW_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
        TerminalFrameRenderOptions::plain(
            pane_frames_enabled,
            DEFAULT_PANE_FRAME_TEMPLATE,
            TerminalFramePosition::Top,
        ),
    )
}

/// Runs the render window with pane frame template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn render_window_with_pane_frame_template(
    window: &Window,
    pane_inputs: &[PaneRenderInput],
    frame_context: &TerminalFrameContext,
    window_frame: TerminalFrameRenderOptions<'_>,
    pane_frame: TerminalFrameRenderOptions<'_>,
) -> Result<Vec<String>> {
    if window.panes().is_empty() {
        return Err(MezError::invalid_state(
            "cannot render a window with no panes",
        ));
    }
    let body_size = window_body_size(window.size, window_frame.enabled)?;

    if let Some(rendered) =
        zoomed_pane_render_input(window, pane_inputs, frame_context, pane_frame, body_size)
    {
        let mut lines = render_panes_by_geometry(
            body_size,
            &[zoomed_pane_geometry(body_size)],
            &[rendered],
            window,
            frame_context,
            pane_frame,
        );
        if window_frame.enabled {
            let frame = fit_width(
                &render_window_frame_text(
                    window,
                    frame_context,
                    window_frame.template,
                    usize::from(window.size.columns),
                ),
                usize::from(window.size.columns),
            );
            place_window_frame(&mut lines, frame, window_frame.position, window.size.rows);
        }
        return Ok(lines);
    }

    let geometries = rendered_pane_geometries(window, window_frame.enabled)?;
    let rendered_panes = geometries
        .iter()
        .map(|geometry| {
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == geometry.index)
                .unwrap_or_else(|| window.active_pane());
            let lines = pane_inputs
                .iter()
                .find(|input| input.pane_id == pane.id.to_string())
                .map(|input| input.lines.as_slice())
                .unwrap_or(&[]);
            let mut display_pane = pane.clone();
            display_pane.size = pane_render_region_size_for_geometry(geometry, &geometries)?;
            let merges = pane_frame.enabled
                && pane_frame_merges_into_divider(geometry, &geometries, pane_frame.position);
            Ok(render_pane_lines(
                window,
                &display_pane,
                frame_context,
                lines,
                pane_frame,
                merges,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lines = render_panes_by_geometry(
        body_size,
        &geometries,
        &rendered_panes,
        window,
        frame_context,
        pane_frame,
    );
    if window_frame.enabled {
        let frame = fit_width(
            &render_window_frame_text(
                window,
                frame_context,
                window_frame.template,
                usize::from(window.size.columns),
            ),
            usize::from(window.size.columns),
        );
        place_window_frame(&mut lines, frame, window_frame.position, window.size.rows);
    }
    Ok(lines)
}

/// Carries Styled Pane Render Input state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StyledPaneRenderInput {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    lines: Vec<TerminalStyledLine>,
}

/// Runs the render styled window with pane frame template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_styled_window_with_pane_frame_template(
    window: &Window,
    pane_inputs: &[StyledPaneRenderInput],
    frame_context: &TerminalFrameContext,
    window_frame: TerminalFrameRenderOptions<'_>,
    pane_frame: TerminalFrameRenderOptions<'_>,
    ui_theme: &UiTheme,
) -> Result<Vec<TerminalStyledLine>> {
    if window.panes().is_empty() {
        return Err(MezError::invalid_state(
            "cannot render a window with no panes",
        ));
    }
    let body_size = window_body_size(window.size, window_frame.enabled)?;

    if let Some(rendered) = zoomed_styled_pane_render_input(
        window,
        pane_inputs,
        frame_context,
        pane_frame,
        body_size,
        ui_theme,
    ) {
        let mut lines = render_styled_panes_by_geometry(
            body_size,
            &[zoomed_pane_geometry(body_size)],
            &[rendered],
            window,
            frame_context,
            pane_frame,
            ui_theme,
        );
        if window_frame.enabled {
            let frame = styled_window_frame_line(
                window,
                frame_context,
                window_frame.template,
                usize::from(window.size.columns),
                window_frame.style,
                ui_theme,
            );
            place_window_frame(&mut lines, frame, window_frame.position, window.size.rows);
        }
        return Ok(lines);
    }

    let geometries = rendered_pane_geometries(window, window_frame.enabled)?;
    let rendered_panes = geometries
        .iter()
        .map(|geometry| {
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == geometry.index)
                .unwrap_or_else(|| window.active_pane());
            let lines = pane_inputs
                .iter()
                .find(|input| input.pane_id == pane.id.to_string())
                .map(|input| input.lines.as_slice())
                .unwrap_or(&[]);
            let mut display_pane = pane.clone();
            display_pane.size = pane_render_region_size_for_geometry(geometry, &geometries)?;
            let merges = pane_frame.enabled
                && pane_frame_merges_into_divider(geometry, &geometries, pane_frame.position);
            Ok(render_styled_pane_lines(
                window,
                &display_pane,
                frame_context,
                lines,
                pane_frame,
                merges,
                ui_theme,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut lines = render_styled_panes_by_geometry(
        body_size,
        &geometries,
        &rendered_panes,
        window,
        frame_context,
        pane_frame,
        ui_theme,
    );
    if window_frame.enabled {
        let frame = styled_window_frame_line(
            window,
            frame_context,
            window_frame.template,
            usize::from(window.size.columns),
            window_frame.style,
            ui_theme,
        );
        place_window_frame(&mut lines, frame, window_frame.position, window.size.rows);
    }
    Ok(lines)
}

/// Runs the zoomed pane render input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn zoomed_pane_render_input(
    window: &Window,
    pane_inputs: &[PaneRenderInput],
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    body_size: Size,
) -> Option<Vec<String>> {
    let zoomed_id = window.zoomed_pane_id()?;
    let pane = window
        .panes()
        .iter()
        .find(|pane| pane.id.as_str() == zoomed_id.as_str())?;
    let lines = pane_inputs
        .iter()
        .find(|input| input.pane_id == pane.id.to_string())
        .map(|input| input.lines.as_slice())
        .unwrap_or(&[]);
    let mut display_pane = pane.clone();
    display_pane.size = body_size;
    Some(render_pane_lines(
        window,
        &display_pane,
        frame_context,
        lines,
        pane_frame,
        false,
    ))
}

/// Runs the zoomed styled pane render input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn zoomed_styled_pane_render_input(
    window: &Window,
    pane_inputs: &[StyledPaneRenderInput],
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    body_size: Size,
    ui_theme: &UiTheme,
) -> Option<Vec<TerminalStyledLine>> {
    let zoomed_id = window.zoomed_pane_id()?;
    let pane = window
        .panes()
        .iter()
        .find(|pane| pane.id.as_str() == zoomed_id.as_str())?;
    let lines = pane_inputs
        .iter()
        .find(|input| input.pane_id == pane.id.to_string())
        .map(|input| input.lines.as_slice())
        .unwrap_or(&[]);
    let mut display_pane = pane.clone();
    display_pane.size = body_size;
    Some(render_styled_pane_lines(
        window,
        &display_pane,
        frame_context,
        lines,
        pane_frame,
        false,
        ui_theme,
    ))
}

/// Runs the zoomed pane geometry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn zoomed_pane_geometry(size: Size) -> PaneGeometry {
    PaneGeometry {
        index: 0,
        column: 0,
        row: 0,
        columns: size.columns,
        rows: size.rows,
    }
}

/// Returns the drawable window body after reserving mux-managed window frames.
pub fn rendered_window_body_size(size: Size, window_frames_enabled: bool) -> Result<Size> {
    let rows = if window_frames_enabled {
        size.rows.saturating_sub(1)
    } else {
        size.rows
    };
    Size::new(size.columns, rows.max(1))
}

/// Runs the window body size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_body_size(size: Size, window_frames_enabled: bool) -> Result<Size> {
    rendered_window_body_size(size, window_frames_enabled)
}

/// Returns pane rectangles apportioned within the rendered window body.
pub fn rendered_pane_geometries(
    window: &Window,
    window_frames_enabled: bool,
) -> Result<Vec<PaneGeometry>> {
    let body_size = rendered_window_body_size(window.size, window_frames_enabled)?;
    Ok(window.pane_geometries_for_size(body_size))
}

/// Returns the visible pane region after reserving shared divider cells.
pub fn pane_render_region_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
) -> Result<Size> {
    let columns = geometry
        .columns
        .saturating_sub(u16::from(geometry_has_right_divider(geometry, geometries)))
        .max(1);
    let rows = geometry
        .rows
        .saturating_sub(u16::from(geometry_has_bottom_divider(geometry, geometries)))
        .max(1);
    Size::new(columns, rows)
}

/// Returns the pane body size available to the pane's PTY primary process.
///
/// When `pane_frames_enabled` is true and the frame is adjacent to a shared
/// divider, the frame is rendered in the divider row instead of consuming a
/// separate pane row, so the content size does not subtract that frame row.
pub fn pane_content_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    pane_frames_enabled: bool,
    pane_frame_position: TerminalFramePosition,
) -> Result<Size> {
    let render_region = pane_render_region_size_for_geometry(geometry, geometries)?;
    let frame_rows = if pane_frames_enabled
        && !pane_frame_merges_into_divider(geometry, geometries, pane_frame_position)
    {
        1
    } else {
        0
    };
    let rows = render_region.rows.saturating_sub(frame_rows).max(1);
    Size::new(render_region.columns, rows)
}

/// Runs the place window frame operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn place_window_frame<T>(
    lines: &mut Vec<T>,
    frame: T,
    position: TerminalFramePosition,
    authoritative_rows: u16,
) {
    let rows = usize::from(authoritative_rows);
    match position {
        TerminalFramePosition::Top => {
            lines.insert(0, frame);
            lines.truncate(rows);
        }
        TerminalFramePosition::Bottom => {
            lines.truncate(rows.saturating_sub(1));
            lines.push(frame);
            lines.truncate(rows);
        }
    }
}

/// Places the conditional top window-group frame above the rendered window.
fn place_group_frame<T>(lines: &mut Vec<T>, frame: T, authoritative_rows: u16) {
    let rows = usize::from(authoritative_rows);
    lines.insert(0, frame);
    lines.truncate(rows);
}

/// Renders the unstyled top group bar when more than one group exists.
fn group_frame_text(frame_context: &TerminalFrameContext, width: usize) -> Option<String> {
    group_frame_visible(frame_context).then(|| {
        fit_width(
            &window_frame_pillbox_text_from_entries(&group_frame_pillbox_entries(frame_context)),
            width,
        )
    })
}

/// Renders the styled top group bar when more than one group exists.
fn styled_group_frame_line(
    frame_context: &TerminalFrameContext,
    width: usize,
    frame_style: TerminalFrameStyle,
    ui_theme: &UiTheme,
) -> Option<TerminalStyledLine> {
    if !group_frame_visible(frame_context) {
        return None;
    }
    let entries = group_frame_pillbox_entries(frame_context);
    let mut row = vec![' '; width];
    write_frame_text_cells(
        &mut row,
        0,
        width,
        &window_frame_pillbox_text_from_entries(&entries),
    );
    let style_spans = subtle_frame_fill_span(width, frame_style, ui_theme)
        .into_iter()
        .chain(
            window_frame_pillbox_segments(&entries)
                .into_iter()
                .filter_map(|segment| {
                    Some(TerminalStyleSpan {
                        start: segment.start,
                        length: segment.width.min(width.saturating_sub(segment.start)),
                        rendition: window_pillbox_rendition(
                            segment.active,
                            segment.subagent,
                            frame_style,
                            ui_theme,
                        ),
                    })
                    .filter(|span| span.length > 0 && span.start < width)
                }),
        )
        .collect::<Vec<_>>();
    Some(TerminalStyledLine {
        text: row.into_iter().collect(),
        style_spans,
        copy_text: None,
    })
}

/// Runs the styled window frame line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn styled_window_frame_line(
    window: &Window,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
    frame_style: TerminalFrameStyle,
    ui_theme: &UiTheme,
) -> TerminalStyledLine {
    if template == DEFAULT_WINDOW_FRAME_TEMPLATE {
        return styled_window_pillbox_line(window, frame_context, width, frame_style, ui_theme);
    }
    let text = render_window_frame_text(window, frame_context, template, width);
    let rendition = Some(themed_frame_rendition(
        ui_theme.colors.window_frame,
        frame_style,
        true,
    ));
    let mut line = styled_frame_line_with_rendition(&text, width, rendition);
    if let Some(status) = window_right_status_layout(frame_context, width) {
        line.style_spans
            .extend(window_status_style_spans(&status, ui_theme));
    }
    line
}

/// Runs the styled window pillbox line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn styled_window_pillbox_line(
    window: &Window,
    frame_context: &TerminalFrameContext,
    width: usize,
    frame_style: TerminalFrameStyle,
    ui_theme: &UiTheme,
) -> TerminalStyledLine {
    let entries = window_frame_pillbox_entries(window, frame_context);
    let right_status = window_right_status_layout(frame_context, width);
    let left_width = right_status
        .as_ref()
        .map(|status| status.start.saturating_sub(1))
        .unwrap_or(width);
    let mut row = vec![' '; width];
    write_frame_text_cells(
        &mut row,
        0,
        left_width,
        &window_frame_pillbox_text_from_entries(&entries),
    );
    if let Some(status) = right_status.as_ref() {
        write_frame_text_cells(&mut row, status.start, status.width, &status.text);
    }
    let style_spans = subtle_frame_fill_span(width, frame_style, ui_theme)
        .into_iter()
        .chain(
            window_frame_pillbox_segments(&entries)
                .into_iter()
                .filter_map(|segment| {
                    clip_style_span(
                        TerminalStyleSpan {
                            start: segment.start,
                            length: segment.width,
                            rendition: window_pillbox_rendition(
                                segment.active,
                                segment.subagent,
                                frame_style,
                                ui_theme,
                            ),
                        },
                        left_width,
                    )
                }),
        )
        .chain(
            right_status
                .as_ref()
                .into_iter()
                .flat_map(|status| window_status_style_spans(status, ui_theme)),
        )
        .collect::<Vec<_>>();
    TerminalStyledLine {
        text: row.into_iter().collect(),
        style_spans,
        copy_text: None,
    }
}

/// Runs the window pillbox rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_pillbox_rendition(
    active: bool,
    subagent: bool,
    frame_style: TerminalFrameStyle,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    let pair = if active {
        ui_theme.colors.window_active
    } else if subagent {
        ui_theme.colors.agent_status_idle
    } else {
        ui_theme.colors.window_inactive
    };
    let mut rendition = themed_frame_rendition(pair, frame_style, active);
    if active {
        rendition.bold = true;
    }
    rendition
}

/// Runs the styled pane frame line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn styled_pane_frame_line(
    window: &Window,
    width: usize,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    frame_style: TerminalFrameStyle,
    ui_theme: &UiTheme,
) -> TerminalStyledLine {
    let layout = pane_frame_row_layout(window, pane, frame_context, template, width, ' ');
    let rendition = pane_frame_rendition(pane, frame_style, ui_theme);
    let style_spans: Vec<TerminalStyleSpan> = if layout.left_text_width == 0 {
        subtle_frame_fill_span(width, frame_style, ui_theme)
            .into_iter()
            .collect()
    } else {
        subtle_frame_fill_span(width, frame_style, ui_theme)
            .into_iter()
            .chain(std::iter::once(TerminalStyleSpan {
                start: 0,
                length: layout.left_text_width,
                rendition,
            }))
            .collect()
    };
    let mut style_spans = style_spans;
    style_spans.extend(pane_frame_right_status_style_spans(
        &layout,
        0,
        frame_context,
        ui_theme,
    ));
    TerminalStyledLine {
        text: layout.text,
        style_spans,
        copy_text: None,
    }
}

/// Runs the pane frame right status style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_right_status_style_spans(
    layout: &PaneFrameRowLayout,
    column_offset: usize,
    frame_context: &TerminalFrameContext,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    layout
        .right_status_segments
        .iter()
        .flat_map(|segment| {
            pane_frame_right_status_segment_style_spans(
                segment,
                column_offset,
                frame_context,
                ui_theme,
            )
        })
        .collect()
}

/// Builds style spans for one pane right-status segment.
fn pane_frame_right_status_segment_style_spans(
    segment: &PaneFrameRightStatusSegment,
    column_offset: usize,
    frame_context: &TerminalFrameContext,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    if segment.field == "agent.status"
        && pane_frame_agent_status_is_active(&segment.value)
        && !frame_context.reduced_motion
    {
        return pane_frame_agent_status_scan_spans(
            column_offset.saturating_add(segment.start),
            segment.width,
            frame_context.animation_tick_ms,
            ui_theme,
        );
    }
    vec![TerminalStyleSpan {
        start: column_offset.saturating_add(segment.start),
        length: segment.width,
        rendition: pane_frame_right_status_rendition(segment, ui_theme),
    }]
}

/// Runs the pane frame right status rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_right_status_rendition(
    segment: &PaneFrameRightStatusSegment,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    match segment.field {
        "history.position" => ui_theme.colors.scroll_indicator.rendition(),
        "pane.pwd" => ui_theme.colors.pane_pwd.rendition(),
        "agent.model" => ui_theme.colors.agent_model.rendition(),
        "agent.reasoning" => ui_theme.colors.agent_reasoning.rendition(),
        "agent.auto_reasoning" => {
            pane_frame_agent_auto_reasoning_rendition(&segment.value, ui_theme)
        }
        "agent.latency" => pane_frame_latency_rendition(&segment.value, ui_theme),
        "agent.preset" => ui_theme.colors.agent_model.rendition(),
        "agent.name" => ui_theme.colors.agent_model.rendition(),
        "agent.context_usage" => pane_frame_agent_context_usage_rendition(&segment.value, ui_theme),
        "agent.status" => pane_frame_agent_status_rendition(&segment.value, ui_theme),
        "policy.mode" => pane_frame_policy_mode_rendition(&segment.value, ui_theme),
        _ => ui_theme.colors.scroll_indicator.rendition(),
    }
}

/// Returns the auto-reasoning pill rendition for a pane-local value.
fn pane_frame_agent_auto_reasoning_rendition(value: &str, ui_theme: &UiTheme) -> GraphicRendition {
    match value {
        "auto:on" => ui_theme.colors.agent_reasoning.rendition(),
        "auto:off" => ui_theme.colors.agent_status_idle.rendition(),
        _ => ui_theme.colors.scroll_indicator.rendition(),
    }
}

/// Returns the latency-preference pill rendition for a pane-local value.
fn pane_frame_latency_rendition(value: &str, ui_theme: &UiTheme) -> GraphicRendition {
    match value {
        "slow" => ui_theme.colors.agent_status_idle.rendition(),
        "fast" => ui_theme.colors.agent_status_running.rendition(),
        _ => ui_theme.colors.agent_model.rendition(),
    }
}

/// Returns the approval-policy pill rendition for a pane-local value.
fn pane_frame_policy_mode_rendition(value: &str, ui_theme: &UiTheme) -> GraphicRendition {
    if value == "full-access" {
        ui_theme.colors.agent_status_running.rendition()
    } else if value == "auto-allow" {
        ui_theme.colors.agent_reasoning.rendition()
    } else if value == "ask" {
        ui_theme.colors.agent_status_blocked.rendition()
    } else {
        ui_theme.colors.scroll_indicator.rendition()
    }
}

/// Returns the context-usage pill rendition for one percentage value.
fn pane_frame_agent_context_usage_rendition(value: &str, ui_theme: &UiTheme) -> GraphicRendition {
    let percentage = value.trim_end_matches('%').parse::<u16>().unwrap_or(0);
    let surface = ui_theme.colors.frame_fill.background;
    let background = if percentage >= 100 {
        blend_terminal_color(
            surface,
            ui_theme.colors.agent_status_failed.background,
            5,
            8,
        )
    } else if percentage >= 85 {
        blend_terminal_color(surface, ui_theme.colors.agent_reasoning.background, 5, 8)
    } else {
        neutral_surface_step(surface)
    };
    GraphicRendition {
        foreground: Some(contrasting_binary_foreground(background)),
        background: Some(background),
        ..GraphicRendition::default()
    }
}

/// Runs the pane frame agent status rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_agent_status_rendition(status: &str, ui_theme: &UiTheme) -> GraphicRendition {
    match status {
        "queued" | "running" | "thinking" | "executing" | "waiting" | "compacting" => {
            ui_theme.colors.agent_status_running.rendition()
        }
        "blocked" | "waiting_approval" => ui_theme.colors.agent_status_blocked.rendition(),
        "failed" => ui_theme.colors.agent_status_failed.rendition(),
        "interrupted" | "stopped" => ui_theme.colors.agent_status_idle.rendition(),
        _ => ui_theme.colors.agent_status_idle.rendition(),
    }
}

/// Returns whether an agent status should render with active-work animation.
fn pane_frame_agent_status_is_active(status: &str) -> bool {
    matches!(
        status,
        "queued" | "running" | "thinking" | "executing" | "waiting" | "compacting"
    )
}

/// Shared scan width for active agent status animations.
const AGENT_STATUS_SCAN_BAND_WIDTH: usize = 9;

/// Builds the animated scan background for an active agent status pill.
fn pane_frame_agent_status_scan_spans(
    start: usize,
    width: usize,
    tick_ms: u64,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    if width == 0 {
        return Vec::new();
    }
    let base_pair = ui_theme.colors.agent_status_running;
    let palette = agent_status_running_gradient_palette(ui_theme);
    let phase = ((tick_ms / AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS) as usize)
        % width.saturating_add(AGENT_STATUS_SCAN_BAND_WIDTH);
    let center = phase as isize - (AGENT_STATUS_SCAN_BAND_WIDTH as isize / 2);
    let mut spans = Vec::with_capacity(width);
    for column in 0..width {
        let offset = column as isize - center;
        let distance = offset.unsigned_abs();
        let intensity = AGENT_STATUS_SCAN_BAND_WIDTH.saturating_sub(distance);
        let highlight = gradient_highlight_for_offset(&palette, offset);
        let background = animated_scan_background(
            base_pair.background,
            highlight,
            intensity,
            AGENT_STATUS_SCAN_BAND_WIDTH,
        );
        push_or_extend_style_span(
            &mut spans,
            TerminalStyleSpan {
                start: start.saturating_add(column),
                length: 1,
                rendition: GraphicRendition {
                    foreground: Some(base_pair.foreground),
                    background: Some(background),
                    ..GraphicRendition::default()
                },
            },
        );
    }
    spans
}

/// Returns a theme-relative harmonious ramp for active agent status animation.
fn agent_status_running_gradient_palette(ui_theme: &UiTheme) -> [TerminalColor; 3] {
    let base = ui_theme.colors.agent_status_running.background;
    let Some((red, green, blue)) = terminal_color_rgb(base) else {
        return [
            base,
            ui_theme.colors.agent_model.background,
            ui_theme.colors.agent_reasoning.background,
        ];
    };
    let hsl = rgb_to_hsl(red, green, blue);
    let saturation = (hsl.saturation * 1.12 + 0.04).clamp(0.0, 1.0);
    let lightness_offset = if hsl.lightness > 0.62 { -0.08 } else { 0.08 };
    let lightness = (hsl.lightness + lightness_offset).clamp(0.18, 0.82);
    [
        hsl_to_terminal_color(HslColor {
            hue: hsl.hue - 30.0,
            saturation,
            lightness,
        }),
        hsl_to_terminal_color(HslColor {
            hue: hsl.hue,
            saturation: (saturation * 0.92).clamp(0.0, 1.0),
            lightness: (lightness + lightness_offset / 2.0).clamp(0.18, 0.82),
        }),
        hsl_to_terminal_color(HslColor {
            hue: hsl.hue + 30.0,
            saturation,
            lightness,
        }),
    ]
}

/// Chooses a scan highlight from the left, center, or right side of the ramp.
fn gradient_highlight_for_offset(palette: &[TerminalColor; 3], offset: isize) -> TerminalColor {
    if offset < -1 {
        palette[0]
    } else if offset > 1 {
        palette[2]
    } else {
        palette[1]
    }
}

/// Blends one scan-band cell between the base and highlight colors.
fn animated_scan_background(
    base: TerminalColor,
    highlight: TerminalColor,
    intensity: usize,
    max_intensity: usize,
) -> TerminalColor {
    let Some((base_r, base_g, base_b)) = terminal_color_rgb(base) else {
        return base;
    };
    let Some((highlight_r, highlight_g, highlight_b)) = terminal_color_rgb(highlight) else {
        return base;
    };
    let numerator = intensity.min(max_intensity) as u16;
    let denominator = max_intensity.max(1) as u16;
    TerminalColor::Rgb(
        blend_channel(base_r, highlight_r, numerator, denominator),
        blend_channel(base_g, highlight_g, numerator, denominator),
        blend_channel(base_b, highlight_b, numerator, denominator),
    )
}

/// HSL representation used for theme-derived color harmonies.
#[derive(Debug, Clone, Copy)]
struct HslColor {
    /// Hue in degrees.
    hue: f32,
    /// Saturation in the range 0.0..=1.0.
    saturation: f32,
    /// Lightness in the range 0.0..=1.0.
    lightness: f32,
}

/// Converts RGB components to HSL so neighboring hues can be derived.
fn rgb_to_hsl(red: u8, green: u8, blue: u8) -> HslColor {
    let red = f32::from(red) / 255.0;
    let green = f32::from(green) / 255.0;
    let blue = f32::from(blue) / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let chroma = max - min;
    let lightness = (max + min) / 2.0;
    if chroma <= f32::EPSILON {
        return HslColor {
            hue: 0.0,
            saturation: 0.0,
            lightness,
        };
    }
    let saturation = chroma / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if red >= green && red >= blue {
        60.0 * ((green - blue) / chroma).rem_euclid(6.0)
    } else if green >= blue {
        60.0 * ((blue - red) / chroma + 2.0)
    } else {
        60.0 * ((red - green) / chroma + 4.0)
    };
    HslColor {
        hue,
        saturation,
        lightness,
    }
}

/// Converts HSL components into an RGB terminal color.
fn hsl_to_terminal_color(color: HslColor) -> TerminalColor {
    let hue = color.hue.rem_euclid(360.0);
    let saturation = color.saturation.clamp(0.0, 1.0);
    let lightness = color.lightness.clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_prime = hue / 60.0;
    let second = chroma * (1.0 - (hue_prime.rem_euclid(2.0) - 1.0).abs());
    let (red1, green1, blue1) = if hue_prime < 1.0 {
        (chroma, second, 0.0)
    } else if hue_prime < 2.0 {
        (second, chroma, 0.0)
    } else if hue_prime < 3.0 {
        (0.0, chroma, second)
    } else if hue_prime < 4.0 {
        (0.0, second, chroma)
    } else if hue_prime < 5.0 {
        (second, 0.0, chroma)
    } else {
        (chroma, 0.0, second)
    };
    let match_lightness = lightness - chroma / 2.0;
    TerminalColor::Rgb(
        unit_float_to_u8(red1 + match_lightness),
        unit_float_to_u8(green1 + match_lightness),
        unit_float_to_u8(blue1 + match_lightness),
    )
}

/// Converts a normalized floating color channel to an integer byte.
fn unit_float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Returns RGB components for true-color values.
fn terminal_color_rgb(color: TerminalColor) -> Option<(u8, u8, u8)> {
    match color {
        TerminalColor::Rgb(red, green, blue) => Some((red, green, blue)),
        TerminalColor::Indexed(_) => None,
    }
}

/// Returns a simple perceptual luminance approximation for true-color values.
fn terminal_color_luminance(color: TerminalColor) -> Option<u32> {
    let (red, green, blue) = terminal_color_rgb(color)?;
    Some((u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000)
}

/// Returns WCAG-style contrast ratio for two true-color values.
fn terminal_color_contrast_ratio(
    foreground: TerminalColor,
    background: TerminalColor,
) -> Option<f64> {
    let foreground_luminance = terminal_color_relative_luminance(foreground)?;
    let background_luminance = terminal_color_relative_luminance(background)?;
    let lighter = foreground_luminance.max(background_luminance);
    let darker = foreground_luminance.min(background_luminance);
    Some((lighter + 0.05) / (darker + 0.05))
}

/// Returns the relative luminance of a true-color value.
fn terminal_color_relative_luminance(color: TerminalColor) -> Option<f64> {
    let (red, green, blue) = terminal_color_rgb(color)?;
    Some(
        0.2126 * srgb_channel_to_linear(red)
            + 0.7152 * srgb_channel_to_linear(green)
            + 0.0722 * srgb_channel_to_linear(blue),
    )
}

/// Converts one sRGB channel to linear-light space.
fn srgb_channel_to_linear(channel: u8) -> f64 {
    let normalized = f64::from(channel) / 255.0;
    if normalized <= 0.03928 {
        normalized / 12.92
    } else {
        ((normalized + 0.055) / 1.055).powf(2.4)
    }
}

/// Chooses black or white text for one themed background.
fn contrasting_binary_foreground(background: TerminalColor) -> TerminalColor {
    match terminal_color_luminance(background) {
        Some(luminance) if luminance >= 140 => TerminalColor::Rgb(0x00, 0x00, 0x00),
        Some(_) => TerminalColor::Rgb(0xff, 0xff, 0xff),
        None => TerminalColor::Rgb(0xff, 0xff, 0xff),
    }
}

/// Blends two true-color values, falling back to the base for indexed colors.
fn blend_terminal_color(
    base: TerminalColor,
    highlight: TerminalColor,
    numerator: u16,
    denominator: u16,
) -> TerminalColor {
    let Some((base_r, base_g, base_b)) = terminal_color_rgb(base) else {
        return base;
    };
    let Some((highlight_r, highlight_g, highlight_b)) = terminal_color_rgb(highlight) else {
        return base;
    };
    let denominator = denominator.max(1);
    TerminalColor::Rgb(
        blend_channel(base_r, highlight_r, numerator, denominator),
        blend_channel(base_g, highlight_g, numerator, denominator),
        blend_channel(base_b, highlight_b, numerator, denominator),
    )
}

/// Builds a quiet neutral context-usage background from the frame surface.
fn neutral_surface_step(surface: TerminalColor) -> TerminalColor {
    let Some((red, green, blue)) = terminal_color_rgb(surface) else {
        return surface;
    };
    let luminance = terminal_color_luminance(surface).unwrap_or(0);
    let shift: i16 = if luminance >= 140 { -28 } else { 34 };
    TerminalColor::Rgb(
        shifted_channel(red, shift),
        shifted_channel(green, shift),
        shifted_channel(blue, shift),
    )
}

/// Shifts a color channel by a signed amount.
fn shifted_channel(value: u8, shift: i16) -> u8 {
    (i16::from(value) + shift).clamp(0, 255) as u8
}

/// Linearly blends one color channel with integer arithmetic.
fn blend_channel(base: u8, highlight: u8, numerator: u16, denominator: u16) -> u8 {
    let base = u16::from(base);
    let highlight = u16::from(highlight);
    let value = base
        .saturating_mul(denominator.saturating_sub(numerator))
        .saturating_add(highlight.saturating_mul(numerator))
        / denominator.max(1);
    value.min(u16::from(u8::MAX)) as u8
}

/// Appends a span, merging with the previous span when possible.
fn push_or_extend_style_span(spans: &mut Vec<TerminalStyleSpan>, span: TerminalStyleSpan) {
    if let Some(last) = spans.last_mut()
        && last.start.saturating_add(last.length) == span.start
        && last.rendition == span.rendition
    {
        last.length = last.length.saturating_add(span.length);
        return;
    }
    spans.push(span);
}

/// Runs the subtle frame fill span operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn subtle_frame_fill_span(
    width: usize,
    frame_style: TerminalFrameStyle,
    ui_theme: &UiTheme,
) -> Option<TerminalStyleSpan> {
    if width == 0 {
        return None;
    }
    Some(TerminalStyleSpan {
        start: 0,
        length: width,
        rendition: themed_frame_rendition(ui_theme.colors.frame_fill, frame_style, false),
    })
}

/// Runs the styled frame line with rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn styled_frame_line_with_rendition(
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

/// Runs the pane frame rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_rendition(
    pane: &crate::layout::Pane,
    frame_style: TerminalFrameStyle,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    let pair = if pane.active {
        ui_theme.colors.pane_frame_active
    } else {
        ui_theme.colors.pane_frame_inactive
    };
    let mut rendition = themed_frame_rendition(pair, frame_style, pane.active);
    if pane.active {
        rendition.bold = true;
    }
    rendition
}

/// Runs the pane border rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_border_rendition(active: bool, ui_theme: &UiTheme) -> GraphicRendition {
    let pair = if active {
        ui_theme.colors.pane_border_active
    } else {
        ui_theme.colors.pane_border_inactive
    };
    GraphicRendition {
        foreground: Some(pair.foreground),
        background: None,
        bold: active,
        ..GraphicRendition::default()
    }
}

/// Carries Agent Prompt Block state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentPromptBlock {
    /// Stores the display lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    display_lines: Vec<String>,
    /// Stores the prompt lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    prompt_lines: Vec<String>,
    /// Stores shadow-completion style spans for each prompt line.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    prompt_shadow_spans: Vec<Vec<PromptShadowSpan>>,
    /// Stores the cursor row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_row: usize,
    /// Stores the cursor column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_column: usize,
    /// Stores the cursor visible value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    cursor_visible: bool,
}

impl AgentPromptBlock {
    /// Returns the persistent number of pane rows reserved for prompt input.
    fn reserved_line_count(&self) -> usize {
        self.prompt_lines.len()
    }

    /// Returns styled transient display lines for the prompt block.
    fn display_styled_lines(
        &self,
        width: usize,
        ui_theme: &UiTheme,
        animation_tick_ms: u64,
    ) -> Vec<TerminalStyledLine> {
        let mut lines = Vec::with_capacity(self.display_lines.len());
        for line in &self.display_lines {
            if agent_live_footer_state_label(line).is_some() {
                lines.push(agent_live_footer_styled_line(
                    line,
                    width,
                    animation_tick_ms,
                    ui_theme,
                ));
            } else {
                lines.push(themed_text_line(
                    line,
                    width,
                    display_overlay_text_rendition(ui_theme),
                ));
            }
        }
        lines
    }

    /// Returns styled persistent prompt-input lines for the prompt block.
    fn prompt_styled_lines(
        &self,
        width: usize,
        ui_theme: &UiTheme,
        animation_tick_ms: u64,
    ) -> Vec<TerminalStyledLine> {
        let mut lines = Vec::with_capacity(self.prompt_lines.len());
        for (line_index, line) in self.prompt_lines.iter().enumerate() {
            let mut styled_line =
                themed_full_width_line(line, width, agent_prompt_input_rendition(ui_theme));
            for shadow_span in self
                .prompt_shadow_spans
                .get(line_index)
                .into_iter()
                .flatten()
            {
                if shadow_span.start >= width {
                    continue;
                }
                let length = shadow_span
                    .length
                    .min(width.saturating_sub(shadow_span.start));
                if length == 0 {
                    continue;
                }
                styled_line.style_spans.push(TerminalStyleSpan {
                    start: shadow_span.start,
                    length,
                    rendition: agent_prompt_shadow_hint_rendition(ui_theme),
                });
            }
            if let Some((footer_start, footer_text)) = agent_prompt_line_live_footer_suffix(line) {
                styled_line.style_spans.extend(
                    agent_live_footer_style_spans(
                        footer_text,
                        width.saturating_sub(footer_start),
                        animation_tick_ms,
                        ui_theme,
                        Some(ui_theme.colors.agent_prompt.background),
                    )
                    .into_iter()
                    .map(|span| offset_style_span(span, footer_start)),
                );
            }
            lines.push(styled_line);
        }
        lines
    }

    /// Runs the transparent styled lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn transparent_prompt_styled_lines(&self, width: usize) -> Vec<TerminalStyledLine> {
        (0..self.reserved_line_count())
            .map(|_| TerminalStyledLine::plain(" ".repeat(width)))
            .collect()
    }

    /// Returns plain transient display lines for the prompt block.
    fn display_plain_lines(&self) -> Vec<String> {
        self.display_lines.clone()
    }

    /// Returns plain persistent prompt-input lines for the prompt block.
    fn prompt_plain_lines(&self) -> Vec<String> {
        self.prompt_lines.clone()
    }

    /// Runs the transparent plain lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn transparent_prompt_plain_lines(&self, width: usize) -> Vec<String> {
        (0..self.reserved_line_count())
            .map(|_| " ".repeat(width))
            .collect()
    }
}

/// Runs the render agent prompt block operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_agent_prompt_block(
    width: usize,
    body_rows: usize,
    pane_context: Option<&TerminalPaneFrameContext>,
) -> AgentPromptBlock {
    if width == 0 || body_rows == 0 {
        return AgentPromptBlock {
            display_lines: Vec::new(),
            prompt_lines: Vec::new(),
            prompt_shadow_spans: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        };
    }
    let prompt = pane_context
        .and_then(|context| context.agent_prompt.clone())
        .unwrap_or_else(|| ReadlinePrompt::new(ReadlinePromptKind::Agent));
    let display_source = pane_context
        .map(|context| context.agent_display_lines.as_slice())
        .unwrap_or(&[]);
    let (display_source, live_footer) = split_agent_live_footer_display_source(display_source);
    let prompt_layout = if prompt_can_show_agent_live_footer(&prompt) {
        live_footer
            .map(|footer| render_agent_live_footer_prompt_layout(&prompt, footer, width))
            .unwrap_or_else(|| render_wrapped_prompt_layout(&prompt, width, body_rows.clamp(1, 6)))
    } else {
        render_wrapped_prompt_layout(&prompt, width, body_rows.clamp(1, 6))
    };
    let display_capacity = body_rows.saturating_sub(prompt_layout.lines.len());
    let display_count = display_source.len().min(display_capacity);
    let display_start = display_source.len().saturating_sub(display_count);
    let display_lines = display_source
        .iter()
        .skip(display_start)
        .take(display_count)
        .map(|line| fit_width(line, width))
        .collect::<Vec<_>>();
    AgentPromptBlock {
        display_lines,
        prompt_lines: prompt_layout.lines,
        prompt_shadow_spans: prompt_layout.shadow_spans,
        cursor_row: prompt_layout.cursor_row,
        cursor_column: prompt_layout.cursor_column,
        cursor_visible: prompt_layout.cursor_visible,
    }
}

/// Separates the live agent footer from regular pane-local display rows.
fn split_agent_live_footer_display_source(lines: &[String]) -> (&[String], Option<&str>) {
    match lines.split_last() {
        Some((last, rest)) if agent_live_footer_state_label(last).is_some() => {
            (rest, Some(last.as_str()))
        }
        _ => (lines, None),
    }
}

/// Returns whether the empty prompt row may be used as live status space.
fn prompt_can_show_agent_live_footer(prompt: &ReadlinePrompt) -> bool {
    prompt.kind == ReadlinePromptKind::Agent
        && prompt.buffer.line().is_empty()
        && prompt.selector.is_none()
}

/// Builds a one-row prompt layout that renders live agent status as placeholder text.
fn render_agent_live_footer_prompt_layout(
    prompt: &ReadlinePrompt,
    footer: &str,
    width: usize,
) -> WrappedPromptLayout {
    let prompt_prefix = format!("{MEZ_UI_PREFIX}{}", prompt.render());
    let line = format!("{prompt_prefix}{footer}");
    let cursor_column = prompt.rendered_cursor_column().saturating_add(2);
    WrappedPromptLayout {
        lines: vec![fit_width(&line, width)],
        shadow_spans: vec![Vec::new()],
        cursor_row: 0,
        cursor_column: clamp_visible_cursor_column(cursor_column, width),
        cursor_visible: cursor_column < width,
    }
}

/// Finds a live footer suffix embedded after the agent prompt prefix.
fn agent_prompt_line_live_footer_suffix(line: &str) -> Option<(usize, &str)> {
    for (byte_index, _) in line.char_indices() {
        let suffix = &line[byte_index..];
        if agent_live_footer_state_label(suffix)
            .is_some_and(|label| !label.contains('>') && !label.contains(MEZ_UI_PREFIX.trim()))
        {
            return Some((terminal_text_width(&line[..byte_index]), suffix));
        }
    }
    None
}

/// Runs the themed full width line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn themed_full_width_line(
    line: &str,
    width: usize,
    rendition: GraphicRendition,
) -> TerminalStyledLine {
    TerminalStyledLine {
        text: fit_width(line, width),
        style_spans: (width > 0)
            .then_some(TerminalStyleSpan {
                start: 0,
                length: width,
                rendition,
            })
            .into_iter()
            .collect(),
        copy_text: None,
    }
}

/// Returns a styled line whose Mezzanine-owned text is colored but whose
/// padding cells remain transparent to the terminal background.
fn themed_text_line(line: &str, width: usize, rendition: GraphicRendition) -> TerminalStyledLine {
    let text = fit_width(line, width);
    let length = overlay_text_style_width(line, width);
    TerminalStyledLine {
        text,
        style_spans: (length > 0)
            .then_some(TerminalStyleSpan {
                start: 0,
                length,
                rendition,
            })
            .into_iter()
            .collect(),
        copy_text: None,
    }
}

/// Builds the live agent-turn footer using foreground-only grayscale motion.
fn agent_live_footer_styled_line(
    line: &str,
    width: usize,
    animation_tick_ms: u64,
    ui_theme: &UiTheme,
) -> TerminalStyledLine {
    let text = fit_width(line, width);
    let style_spans =
        agent_live_footer_style_spans(&text, width, animation_tick_ms, ui_theme, None);
    TerminalStyledLine {
        text,
        style_spans,
        copy_text: None,
    }
}

/// Builds foreground-only style spans for the state label and hint in a live footer.
fn agent_live_footer_style_spans(
    line: &str,
    width: usize,
    animation_tick_ms: u64,
    ui_theme: &UiTheme,
    background: Option<TerminalColor>,
) -> Vec<TerminalStyleSpan> {
    let text = fit_width(line, width);
    let mut style_spans = Vec::new();
    let visible_width = overlay_text_style_width(&text, width);
    let state_label_width = agent_live_footer_state_label(&text)
        .map(terminal_text_width)
        .unwrap_or(0);
    if state_label_width == 0 || visible_width == 0 {
        return style_spans;
    }
    let base = agent_live_footer_base_gray(ui_theme);
    let palette = agent_live_footer_grayscale_palette(ui_theme);
    let parenthetical_rendition = agent_live_footer_parenthetical_rendition(ui_theme);
    let phase = ((animation_tick_ms / AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS) as usize)
        % state_label_width.saturating_add(AGENT_STATUS_SCAN_BAND_WIDTH);
    let center = phase as isize - (AGENT_STATUS_SCAN_BAND_WIDTH as isize / 2);
    let mut column = 0usize;
    for grapheme in terminal_graphemes(&text) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        if grapheme_width == 0 {
            continue;
        }
        if column < state_label_width && !grapheme.chars().all(char::is_whitespace) {
            let offset = column as isize - center;
            let distance = offset.unsigned_abs();
            let intensity = AGENT_STATUS_SCAN_BAND_WIDTH.saturating_sub(distance);
            let highlight = gradient_highlight_for_offset(&palette, offset);
            let foreground =
                animated_scan_background(base, highlight, intensity, AGENT_STATUS_SCAN_BAND_WIDTH);
            push_or_extend_style_span(
                &mut style_spans,
                TerminalStyleSpan {
                    start: column,
                    length: grapheme_width,
                    rendition: GraphicRendition {
                        foreground: Some(foreground),
                        background,
                        ..GraphicRendition::default()
                    },
                },
            );
        } else if column < visible_width && column >= state_label_width {
            let mut rendition = parenthetical_rendition;
            rendition.background = background;
            push_or_extend_style_span(
                &mut style_spans,
                TerminalStyleSpan {
                    start: column,
                    length: grapheme_width.min(visible_width.saturating_sub(column)),
                    rendition,
                },
            );
        }
        column = column.saturating_add(grapheme_width);
    }
    style_spans
}

/// Returns the active state label at the front of a live agent footer.
fn agent_live_footer_state_label(line: &str) -> Option<&str> {
    let line = line.trim_end();
    let (state, rest) = line.split_once(" (")?;
    (!state.is_empty() && rest.ends_with(" • esc to interrupt)")).then_some(state)
}

/// Returns the muted rendition used for the timer and interrupt hint.
fn agent_live_footer_parenthetical_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(agent_live_footer_parenthetical_gray(ui_theme)),
        ..GraphicRendition::default()
    }
}

/// Returns a dim neutral gray for the non-animated footer parenthetical.
fn agent_live_footer_parenthetical_gray(ui_theme: &UiTheme) -> TerminalColor {
    let level = i16::from(agent_live_footer_gray_level(ui_theme));
    let background_is_light = agent_live_footer_background_is_light(ui_theme);
    let shift = if background_is_light { 34 } else { -30 };
    let lower = if background_is_light { 0x58 } else { 0x78 };
    let upper = if background_is_light { 0x98 } else { 0xb8 };
    terminal_gray((level + shift).clamp(lower, upper) as u8)
}

/// Returns the theme-relative neutral gray used as the live footer baseline.
fn agent_live_footer_base_gray(ui_theme: &UiTheme) -> TerminalColor {
    let level = agent_live_footer_gray_level(ui_theme);
    terminal_gray(level)
}

/// Returns a grayscale scan palette that mirrors active pane-pill motion.
fn agent_live_footer_grayscale_palette(ui_theme: &UiTheme) -> [TerminalColor; 3] {
    let base = i16::from(agent_live_footer_gray_level(ui_theme));
    if agent_live_footer_background_is_light(ui_theme) {
        [
            terminal_gray((base + 34).clamp(0x30, 0xa8) as u8),
            terminal_gray((base - 18).clamp(0x30, 0xa8) as u8),
            terminal_gray((base - 46).clamp(0x30, 0xa8) as u8),
        ]
    } else {
        [
            terminal_gray((base - 34).clamp(0x68, 0xe8) as u8),
            terminal_gray((base + 22).clamp(0x68, 0xe8) as u8),
            terminal_gray((base + 50).clamp(0x68, 0xe8) as u8),
        ]
    }
}

/// Derives a readable neutral gray from the active display surface.
fn agent_live_footer_gray_level(ui_theme: &UiTheme) -> u8 {
    if agent_live_footer_background_is_light(ui_theme) {
        0x54
    } else {
        0xb0
    }
}

/// Returns whether the footer should use dark grayscale text.
fn agent_live_footer_background_is_light(ui_theme: &UiTheme) -> bool {
    terminal_color_luminance(ui_theme.colors.display_overlay.background)
        .or_else(|| terminal_color_luminance(ui_theme.colors.frame_fill.background))
        .is_some_and(|luminance| luminance >= 140)
}

/// Builds an RGB gray terminal color.
fn terminal_gray(level: u8) -> TerminalColor {
    TerminalColor::Rgb(level, level, level)
}

/// Runs the themed frame rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn themed_frame_rendition(
    pair: UiColorPair,
    frame_style: TerminalFrameStyle,
    bold: bool,
) -> GraphicRendition {
    let mut rendition = pair.rendition();
    if let Some(style) = frame_style_rendition(frame_style) {
        rendition.bold |= style.bold;
        rendition.underline |= style.underline;
        rendition.inverse |= style.inverse;
    }
    rendition.bold |= bold;
    rendition
}

/// Runs the frame style rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn frame_style_rendition(frame_style: TerminalFrameStyle) -> Option<GraphicRendition> {
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

/// Overlays transient agent display lines without changing pane content height.
fn overlay_agent_display_lines<T: Clone>(
    lines: &mut [T],
    content_start: usize,
    content_end: usize,
    display_lines: &[T],
    is_blank: impl Fn(&T) -> bool,
) {
    let targets = agent_display_overlay_targets(
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

/// Chooses pane-content rows for transient agent display overlays.
fn agent_display_overlay_targets<T>(
    lines: &[T],
    content_start: usize,
    content_end: usize,
    display_line_count: usize,
    is_blank: impl Fn(&T) -> bool,
) -> Vec<usize> {
    if display_line_count == 0 || content_start >= content_end {
        return Vec::new();
    }
    let content_len = content_end.saturating_sub(content_start);
    let display_count = display_line_count.min(content_len);
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

/// Runs the render styled pane lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_styled_pane_lines(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    content: &[TerminalStyledLine],
    pane_frame: TerminalFrameRenderOptions<'_>,
    merges_with_divider: bool,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyledLine> {
    let width = usize::from(pane.size.columns);
    let height = usize::from(pane.size.rows);
    let frame_rows = if pane_frame.enabled && !merges_with_divider {
        1
    } else {
        0
    };
    let body_rows = height.saturating_sub(frame_rows);
    let agent_block = if pane_agent_prompt_space_reserved(frame_context.panes.get(pane.id.as_str()))
    {
        render_agent_prompt_block(width, body_rows, frame_context.panes.get(pane.id.as_str()))
    } else {
        AgentPromptBlock {
            display_lines: Vec::new(),
            prompt_lines: Vec::new(),
            prompt_shadow_spans: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        }
    };
    let agent_display_lines = if pane_agent_prompt_transparent(frame_context, pane.id.as_str()) {
        Vec::new()
    } else {
        agent_block.display_styled_lines(width, ui_theme, frame_context.animation_tick_ms)
    };
    let agent_prompt_lines = if pane_agent_prompt_transparent(frame_context, pane.id.as_str()) {
        agent_block.transparent_prompt_styled_lines(width)
    } else {
        agent_block.prompt_styled_lines(width, ui_theme, frame_context.animation_tick_ms)
    };
    let content_rows = body_rows.saturating_sub(agent_block.reserved_line_count());
    let mut lines = Vec::with_capacity(height);

    let frame = (pane_frame.enabled && !merges_with_divider).then(|| {
        styled_pane_frame_line(
            window,
            width,
            pane,
            frame_context,
            pane_frame.template,
            pane_frame.style,
            ui_theme,
        )
    });

    let start = if content.len() > content_rows {
        content.len().saturating_sub(content_rows)
    } else {
        0
    };
    if pane_frame.position == TerminalFramePosition::Top
        && let Some(frame) = frame.clone()
    {
        lines.push(frame);
    }
    let content_start = lines.len();
    for line in content.iter().skip(start).take(content_rows) {
        lines.push(fit_styled_width(line, width));
    }
    let content_end = lines.len();
    overlay_agent_display_lines(
        &mut lines,
        content_start,
        content_end,
        &agent_display_lines,
        |line| line.text.trim().is_empty(),
    );
    lines.extend(agent_prompt_lines);
    if pane_frame.position == TerminalFramePosition::Bottom
        && let Some(frame) = frame
    {
        lines.push(frame);
    }
    while lines.len() < height {
        lines.push(TerminalStyledLine::plain(" ".repeat(width)));
    }
    lines
}

/// Runs the render pane lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_pane_lines(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    content: &[String],
    pane_frame: TerminalFrameRenderOptions<'_>,
    merges_with_divider: bool,
) -> Vec<String> {
    let width = usize::from(pane.size.columns);
    let height = usize::from(pane.size.rows);
    let frame_rows = if pane_frame.enabled && !merges_with_divider {
        1
    } else {
        0
    };
    let body_rows = height.saturating_sub(frame_rows);
    let agent_block = if pane_agent_prompt_space_reserved(frame_context.panes.get(pane.id.as_str()))
    {
        render_agent_prompt_block(width, body_rows, frame_context.panes.get(pane.id.as_str()))
    } else {
        AgentPromptBlock {
            display_lines: Vec::new(),
            prompt_lines: Vec::new(),
            prompt_shadow_spans: Vec::new(),
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
        }
    };
    let agent_display_lines = if pane_agent_prompt_transparent(frame_context, pane.id.as_str()) {
        Vec::new()
    } else {
        agent_block.display_plain_lines()
    };
    let agent_prompt_lines = if pane_agent_prompt_transparent(frame_context, pane.id.as_str()) {
        agent_block.transparent_prompt_plain_lines(width)
    } else {
        agent_block.prompt_plain_lines()
    };
    let content_rows = body_rows.saturating_sub(agent_block.reserved_line_count());
    let mut lines = Vec::with_capacity(height);

    let frame = (pane_frame.enabled && !merges_with_divider)
        .then(|| render_pane_frame_text(window, pane, frame_context, pane_frame.template, width));

    let start = if content.len() > content_rows {
        content.len().saturating_sub(content_rows)
    } else {
        0
    };
    if pane_frame.position == TerminalFramePosition::Top
        && let Some(frame) = frame.clone()
    {
        lines.push(frame);
    }
    let content_start = lines.len();
    for line in content.iter().skip(start).take(content_rows) {
        lines.push(fit_width(line, width));
    }
    let content_end = lines.len();
    overlay_agent_display_lines(
        &mut lines,
        content_start,
        content_end,
        &agent_display_lines,
        |line| line.trim().is_empty(),
    );
    lines.extend(agent_prompt_lines);
    if pane_frame.position == TerminalFramePosition::Bottom
        && let Some(frame) = frame
    {
        lines.push(frame);
    }
    while lines.len() < height {
        lines.push(" ".repeat(width));
    }
    lines
}

/// Runs the render pane frame template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_pane_frame_template(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
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
        let field = &after_start[..end];
        rendered.push_str(&pane_frame_field_value(window, pane, frame_context, field));
        remaining = &after_start[end + 1..];
    }
    sanitize_frame_text(&rendered)
}

/// Runs the render pane frame text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_pane_frame_text(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
) -> String {
    pane_frame_row_layout(window, pane, frame_context, template, width, ' ').text
}

/// Carries Pane Frame Row Layout state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneFrameRowLayout {
    /// Stores the text value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    text: String,
    /// Stores the left text width value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    left_text_width: usize,
    /// Stores the right status segments value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    right_status_segments: Vec<PaneFrameRightStatusSegment>,
}

/// Carries Pane Frame Right Status Segment state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneFrameRightStatusSegment {
    /// Stores the start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    start: usize,
    /// Stores the width value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    width: usize,
    /// Stores the field value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    field: &'static str,
    /// Stores the value value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    value: String,
}

/// Carries Pane Frame Right Value state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneFrameRightValue {
    /// Stores the field value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    field: &'static str,
    /// Stores the value value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    value: String,
    /// Stores the display value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    display: String,
}

/// Carries Rendered Pane Frame Right Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedPaneFrameRightStatus {
    /// Stores the text value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    text: String,
    /// Stores the segments value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    segments: Vec<PaneFrameRightStatusSegment>,
}

/// Runs the pane frame row layout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_row_layout(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
    fill: char,
) -> PaneFrameRowLayout {
    if width == 0 {
        return PaneFrameRowLayout {
            text: String::new(),
            left_text_width: 0,
            right_status_segments: Vec::new(),
        };
    }
    let text = render_pane_frame_template(window, pane, frame_context, template);
    let right_status = pane_frame_right_status(window, pane, frame_context, template);
    let Some(right_status) = right_status else {
        let (text, left_text_width) = pane_frame_text_with_fill(&text, width, fill);
        return PaneFrameRowLayout {
            text,
            left_text_width,
            right_status_segments: Vec::new(),
        };
    };
    let mut row = vec![fill; width];
    let Some((status_start, status_width)) = right_aligned_status_bounds(&right_status.text, width)
    else {
        let (text, left_text_width) = pane_frame_text_with_fill(&text, width, fill);
        return PaneFrameRowLayout {
            text,
            left_text_width,
            right_status_segments: Vec::new(),
        };
    };
    let left_width = status_start.saturating_sub(1);
    let written_left_text_width = write_frame_text_cells(&mut row, 0, left_width, &text);
    let left_text_width = pane_frame_left_pill_style_width(written_left_text_width, left_width);
    write_frame_text_cells(&mut row, status_start, status_width, &right_status.text);
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
            .map(|span| PaneFrameRightStatusSegment {
                start: status_start.saturating_add(span.start),
                width: span.length,
                field: segment.field,
                value: segment.value,
            })
        })
        .collect();
    PaneFrameRowLayout {
        text: row.into_iter().collect(),
        left_text_width,
        right_status_segments,
    }
}

/// Returns the background fill glyph for a pane frame template.
fn pane_frame_fill_char(template: &str) -> char {
    if template == DEFAULT_PANE_FRAME_TEMPLATE {
        '─'
    } else {
        ' '
    }
}

/// Renders pane-frame title text over horizontal border fill.
fn pane_frame_text_with_fill(text: &str, width: usize, fill: char) -> (String, usize) {
    let mut row = vec![fill; width];
    let written_width = write_frame_text_cells(&mut row, 0, width, text);
    (row.into_iter().collect(), written_width)
}

/// Extends the pane title pill over the blank separator before right status.
///
/// The row text already reserves this separator so right-aligned status stays
/// readable. Including it in the title style span makes the right-side padding
/// visible as part of the pane title pill instead of leaving a bare gap.
fn pane_frame_left_pill_style_width(text_width: usize, available_width: usize) -> usize {
    if text_width > 0 && text_width < available_width {
        text_width.saturating_add(1)
    } else {
        text_width
    }
}

/// Builds the pane-frame right status, prioritizing scrollback position before
/// pane-local agent state.
fn pane_frame_right_status(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
) -> Option<RenderedPaneFrameRightStatus> {
    let history_field = "history.position";
    let history_value = pane_frame_field_value(window, pane, frame_context, history_field);
    if !history_value.is_empty() && !template.contains("#{history.position}") {
        return Some(render_pane_frame_right_status(&[PaneFrameRightValue {
            field: history_field,
            value: history_value.clone(),
            display: history_value,
        }]));
    }

    let right_fields = pane_frame_right_aligned_values(window, pane, frame_context, template);
    (!right_fields.is_empty()).then(|| render_pane_frame_right_status(&right_fields))
}

/// Runs the pane frame right aligned values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_right_aligned_values(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
) -> Vec<PaneFrameRightValue> {
    let agent_mode = pane_agent_shell_visible(frame_context, pane.id.as_str());
    DEFAULT_PANE_FRAME_RIGHT_ALIGNED
        .iter()
        .filter(|field| **field != "history.position")
        .filter(|field| agent_mode || (!field.starts_with("agent.") && **field != "policy.mode"))
        .filter(|field| !template.contains(&format!("#{{{field}}}")))
        .filter_map(|field| {
            let value = pane_frame_field_value(window, pane, frame_context, field);
            if value.is_empty() {
                None
            } else {
                let segment_value = pane_frame_right_aligned_segment_value(field, &value);
                if segment_value.is_empty() {
                    return None;
                }
                Some(PaneFrameRightValue {
                    field,
                    display: pane_frame_right_aligned_display_value(field, segment_value),
                    value,
                })
            }
        })
        .collect()
}

/// Runs the render pane frame right status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_pane_frame_right_status(values: &[PaneFrameRightValue]) -> RenderedPaneFrameRightStatus {
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
            segments.push(PaneFrameRightStatusSegment {
                start,
                width,
                field: value.field,
                value: value.value.clone(),
            });
        }
    }
    RenderedPaneFrameRightStatus {
        text: sanitize_frame_text(&text),
        segments,
    }
}

/// Runs the pane agent shell visible operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_agent_shell_visible(frame_context: &TerminalFrameContext, pane_id: &str) -> bool {
    frame_context
        .panes
        .get(pane_id)
        .and_then(|context| context.mode.as_deref())
        == Some("agent")
}

/// Runs the pane agent prompt space reserved operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_agent_prompt_space_reserved(pane_context: Option<&TerminalPaneFrameContext>) -> bool {
    pane_context.is_some_and(|context| {
        context.agent_prompt.is_some() || context.mode.as_deref() == Some("agent")
    })
}

/// Runs the pane agent prompt transparent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_agent_prompt_transparent(frame_context: &TerminalFrameContext, pane_id: &str) -> bool {
    frame_context
        .panes
        .get(pane_id)
        .and_then(|context| context.mode.as_deref())
        == Some("copy")
}

/// Runs the pane frame right aligned display value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_right_aligned_display_value(field: &str, value: String) -> String {
    if matches!(
        field,
        "pane.pwd"
            | "agent.model"
            | "agent.reasoning"
            | "agent.auto_reasoning"
            | "agent.latency"
            | "agent.preset"
            | "agent.name"
            | "agent.context_usage"
            | "agent.status"
            | "policy.mode"
    ) {
        format!(" {value} ")
    } else {
        value
    }
}

/// Returns right-status display text while retaining raw values for style
/// selection, animation, and mouse hitbox semantics.
fn pane_frame_right_aligned_segment_value(field: &str, value: &str) -> String {
    if field == "agent.name" && value.trim() == "manager" {
        return String::new();
    }
    value.to_string()
}

/// Runs the render window frame template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_window_frame_template(
    window: &Window,
    frame_context: &TerminalFrameContext,
    template: &str,
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
        let field = &after_start[..end];
        rendered.push_str(&window_frame_field_value(window, frame_context, field));
        remaining = &after_start[end + 1..];
    }
    sanitize_frame_text(&rendered)
}

/// Runs the render window frame text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_window_frame_text(
    window: &Window,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
) -> String {
    if width == 0 {
        return String::new();
    }
    let text = render_window_frame_template(window, frame_context, template);
    let Some(status) = window_right_status_layout(frame_context, width) else {
        return fit_width(&text, width);
    };
    let mut row = vec![' '; width];
    let left_width = status.start.saturating_sub(1);
    write_frame_text_cells(&mut row, 0, left_width, &text);
    write_frame_text_cells(&mut row, status.start, status.width, &status.text);
    row.into_iter().collect()
}

/// Carries Window Status Segment Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowStatusSegmentKind {
    /// Represents a clickable built-in window action pill.
    Action {
        /// Action selected by the pill.
        action: WindowFrameAction,
        /// Whether the action pill is currently pressed.
        pressed: bool,
    },
    /// Represents the Uptime case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Uptime,
    /// Represents the Date Time case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DateTime,
}

impl WindowStatusSegmentKind {
    /// Returns the window action associated with this segment, if any.
    fn action(&self) -> Option<&WindowFrameAction> {
        match self {
            Self::Action { action, .. } => Some(action),
            Self::Uptime | Self::DateTime => None,
        }
    }
}

/// Carries Window Status Segment state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowStatusSegment {
    /// Stores the start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    start: usize,
    /// Stores the width value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    width: usize,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    kind: WindowStatusSegmentKind,
}

/// Carries Window Right Status Layout state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowRightStatusLayout {
    /// Stores the text value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    text: String,
    /// Stores the start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    start: usize,
    /// Stores the width value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    width: usize,
    /// Stores the segments value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    segments: Vec<WindowStatusSegment>,
}

/// Runs the window right status layout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_right_status_layout(
    frame_context: &TerminalFrameContext,
    width: usize,
) -> Option<WindowRightStatusLayout> {
    let status = frame_context.window_status.as_ref()?;
    if status.template.trim().is_empty() || width == 0 {
        return None;
    }
    let rendered = render_window_status_template(frame_context, status);
    let text = rendered.text.trim_end().to_string();
    let status_width = fitted_text_width(&text, width);
    if status_width == 0 {
        return None;
    }
    let start = width.saturating_sub(status_width);
    let segments = rendered
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
            .map(|span| WindowStatusSegment {
                start: start.saturating_add(span.start),
                width: span.length,
                kind: segment.kind,
            })
        })
        .collect();
    Some(WindowRightStatusLayout {
        text,
        start,
        width: status_width,
        segments,
    })
}

/// Carries Rendered Window Status Template state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedWindowStatusTemplate {
    /// Stores the text value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    text: String,
    /// Stores the segments value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    segments: Vec<WindowStatusSegment>,
}

/// Carries one expanded window-status template field.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowStatusFieldComponent {
    /// Rendered field text.
    text: String,
    /// Style/action segments relative to the field text.
    segments: Vec<WindowStatusSegment>,
}

/// Runs the render window status template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_window_status_template(
    frame_context: &TerminalFrameContext,
    status: &TerminalWindowStatusContext,
) -> RenderedWindowStatusTemplate {
    let mut text = String::new();
    let mut segments = Vec::new();
    let mut remaining = status.template.as_str();
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
        let field = &after_start[..end];
        let component = window_status_field_component(frame_context, status, field);
        let value_start = fitted_text_width(&text, usize::MAX);
        text.push_str(&component.text);
        segments.extend(component.segments.into_iter().map(|mut segment| {
            segment.start = value_start.saturating_add(segment.start);
            segment
        }));
        remaining = &after_start[end + 1..];
    }
    RenderedWindowStatusTemplate {
        text: sanitize_frame_text(&text),
        segments,
    }
}

/// Expands one window status template field into text and relative segments.
fn window_status_field_component(
    frame_context: &TerminalFrameContext,
    status: &TerminalWindowStatusContext,
    field: &str,
) -> WindowStatusFieldComponent {
    if field == "window.buttons" || field == "window.actions" {
        return window_actions_status_component(frame_context);
    }
    if let Some(action) = window_status_template_button_action(field) {
        return window_action_status_component(frame_context, action);
    }
    let (value, kind) = window_status_field_value(status, field);
    let text = if kind.is_some() && !value.is_empty() {
        format!(" {value} ")
    } else {
        value
    };
    let segments = kind
        .filter(|_| !text.is_empty())
        .map(|kind| WindowStatusSegment {
            start: 0,
            width: fitted_text_width(&text, usize::MAX),
            kind,
        })
        .into_iter()
        .filter(|segment| segment.width > 0)
        .collect();
    WindowStatusFieldComponent { text, segments }
}

/// Expands the built-in action pill group for status templates.
fn window_actions_status_component(
    frame_context: &TerminalFrameContext,
) -> WindowStatusFieldComponent {
    let entries = window_action_pillbox_entries(frame_context);
    let text = window_frame_pillbox_text_from_entries(&entries);
    let segments = window_frame_pillbox_segments(&entries)
        .into_iter()
        .filter_map(|segment| {
            let WindowFramePillboxTarget::Action(action) = segment.target else {
                return None;
            };
            Some(WindowStatusSegment {
                start: segment.start,
                width: segment.width,
                kind: WindowStatusSegmentKind::Action {
                    action,
                    pressed: segment.active,
                },
            })
        })
        .collect();
    WindowStatusFieldComponent { text, segments }
}

/// Expands one command-backed button field for a status template.
fn window_action_status_component(
    frame_context: &TerminalFrameContext,
    action: WindowFrameAction,
) -> WindowStatusFieldComponent {
    let entries = vec![WindowFramePillboxEntry::action(action, frame_context)];
    let text = window_frame_pillbox_text_from_entries(&entries);
    let segments = window_frame_pillbox_segments(&entries)
        .into_iter()
        .filter_map(|segment| {
            let WindowFramePillboxTarget::Action(action) = segment.target else {
                return None;
            };
            Some(WindowStatusSegment {
                start: segment.start,
                width: segment.width,
                kind: WindowStatusSegmentKind::Action {
                    action,
                    pressed: segment.active,
                },
            })
        })
        .collect();
    WindowStatusFieldComponent { text, segments }
}

/// Parses a generalized `#{button:<icon>|<kind>|<command>}` status field.
fn window_status_template_button_action(field: &str) -> Option<WindowFrameAction> {
    let rest = field.strip_prefix("button:")?;
    let mut parts = rest.splitn(3, '|');
    let icon = parts.next()?.trim();
    let kind = parts.next()?.trim();
    let command = parts.next()?.trim();
    if icon.is_empty() || command.is_empty() {
        return None;
    }
    match kind {
        "terminal" | ":" => Some(WindowFrameAction::terminal_button(icon, command)),
        "agent" | "/" => Some(WindowFrameAction::agent_button(icon, command)),
        _ => None,
    }
}

/// Runs the window status field value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_status_field_value(
    status: &TerminalWindowStatusContext,
    field: &str,
) -> (String, Option<WindowStatusSegmentKind>) {
    match field {
        "system.uptime" => (
            sanitize_frame_text(&status.system_uptime),
            Some(WindowStatusSegmentKind::Uptime),
        ),
        "datetime.local" => (
            sanitize_frame_text(&status.datetime_local),
            Some(WindowStatusSegmentKind::DateTime),
        ),
        "pane.pwd" => (
            sanitize_frame_text(
                status
                    .active_pane_working_directory
                    .as_deref()
                    .unwrap_or_default(),
            ),
            Some(WindowStatusSegmentKind::DateTime),
        ),
        _ => (String::new(), None),
    }
}

/// Runs the window status style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_status_style_spans(
    status: &WindowRightStatusLayout,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    status
        .segments
        .iter()
        .map(|segment| TerminalStyleSpan {
            start: segment.start,
            length: segment.width,
            rendition: match &segment.kind {
                WindowStatusSegmentKind::Action { pressed, .. } => {
                    window_pillbox_rendition(*pressed, false, TerminalFrameStyle::Default, ui_theme)
                }
                WindowStatusSegmentKind::Uptime => ui_theme.colors.window_status_uptime.rendition(),
                WindowStatusSegmentKind::DateTime => {
                    ui_theme.colors.window_status_datetime.rendition()
                }
            },
        })
        .collect()
}

/// Returns rendered cells occupied by each default window-frame pill.
pub fn window_frame_pillbox_cells(
    frame_context: &TerminalFrameContext,
    row: u16,
    width: u16,
) -> Vec<MouseWindowFrameCell> {
    let entries = window_frame_pillbox_entries_from_context(frame_context);
    window_frame_pillbox_segments(&entries)
        .into_iter()
        .flat_map(|segment| {
            let WindowFramePillboxTarget::Window(window_index) = segment.target else {
                return Vec::new();
            };
            let start = segment.start.min(usize::from(width));
            let end = segment
                .start
                .saturating_add(segment.width)
                .min(usize::from(width));
            (start..end)
                .filter_map(move |column| {
                    Some(MouseWindowFrameCell {
                        column: u16::try_from(column).ok()?,
                        row,
                        window_index,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Returns rendered cells occupied by each window-frame status action pill.
pub fn window_frame_action_pillbox_cells(
    frame_context: &TerminalFrameContext,
    row: u16,
    width: u16,
) -> Vec<MouseWindowActionFrameCell> {
    let Some(status) = window_right_status_layout(frame_context, usize::from(width)) else {
        return Vec::new();
    };
    status
        .segments
        .into_iter()
        .flat_map(|segment| {
            let Some(action) = segment.kind.action().cloned() else {
                return Vec::new();
            };
            let start = segment.start.min(usize::from(width));
            let end = segment
                .start
                .saturating_add(segment.width)
                .min(usize::from(width));
            (start..end)
                .filter_map(move |column| {
                    Some(MouseWindowActionFrameCell {
                        column: u16::try_from(column).ok()?,
                        row,
                        action: action.clone(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Returns rendered cells occupied by pane-frame model and reasoning pills.
///
/// The caller supplies pane geometries in rendered-window body coordinates and
/// a row offset for conditional group/window frames. This keeps hit testing
/// aligned with the same layout routine that renders the pane status text.
pub fn pane_frame_agent_status_pillbox_cells(
    window: &Window,
    frame_context: &TerminalFrameContext,
    template: &str,
    position: TerminalFramePosition,
    row_offset: u16,
    geometries: &[PaneGeometry],
) -> Vec<MousePaneAgentStatusCell> {
    geometries
        .iter()
        .flat_map(|geometry| {
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == geometry.index)
                .unwrap_or_else(|| window.active_pane());
            let width = usize::from(
                pane_render_region_size_for_geometry(geometry, geometries)
                    .map(|size| size.columns)
                    .unwrap_or(geometry.columns),
            );
            let row = pane_frame_row_for_geometry(geometry, geometries, position, row_offset);
            let fill = if pane_frame_merges_into_divider(geometry, geometries, position) {
                pane_frame_fill_char(template)
            } else {
                ' '
            };
            pane_frame_row_layout(window, pane, frame_context, template, width, fill)
                .right_status_segments
                .into_iter()
                .flat_map(move |segment| {
                    let Some(field) = pane_agent_status_field_from_frame_field(segment.field)
                    else {
                        return Vec::new();
                    };
                    let start = segment.start.min(width);
                    let end = segment.start.saturating_add(segment.width).min(width);
                    (start..end)
                        .filter_map(move |column| {
                            Some(MousePaneAgentStatusCell {
                                column: geometry.column.checked_add(u16::try_from(column).ok()?)?,
                                row,
                                pane_index: geometry.index,
                                field,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Maps an internal pane-frame field name to a clickable selector field.
fn pane_agent_status_field_from_frame_field(field: &str) -> Option<PaneAgentStatusField> {
    match field {
        "agent.model" => Some(PaneAgentStatusField::Model),
        "agent.reasoning" => Some(PaneAgentStatusField::Reasoning),
        "agent.auto_reasoning" => Some(PaneAgentStatusField::AutoReasoning),
        "agent.latency" => Some(PaneAgentStatusField::Latency),
        "agent.preset" => Some(PaneAgentStatusField::Preset),
        "policy.mode" => Some(PaneAgentStatusField::ApprovalPolicy),
        _ => None,
    }
}

/// Returns the rendered terminal row for a pane-frame status segment.
fn pane_frame_row_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    position: TerminalFramePosition,
    row_offset: u16,
) -> u16 {
    if pane_frame_merges_into_divider(geometry, geometries, position) {
        return row_offset.saturating_add(match position {
            TerminalFramePosition::Top => geometry.row.saturating_sub(1),
            TerminalFramePosition::Bottom => {
                geometry.row.saturating_add(geometry.rows).saturating_sub(1)
            }
        });
    }
    row_offset.saturating_add(match position {
        TerminalFramePosition::Top => geometry.row,
        TerminalFramePosition::Bottom => {
            let render_rows = pane_render_region_size_for_geometry(geometry, geometries)
                .map(|size| size.rows)
                .unwrap_or(geometry.rows);
            geometry.row.saturating_add(render_rows).saturating_sub(1)
        }
    })
}

/// Returns rendered cells occupied by each default window-group pill.
pub fn window_group_frame_pillbox_cells(
    frame_context: &TerminalFrameContext,
    row: u16,
    width: u16,
) -> Vec<MouseWindowGroupFrameCell> {
    if !group_frame_visible(frame_context) {
        return Vec::new();
    }
    let entries = group_frame_pillbox_entries(frame_context);
    window_frame_pillbox_segments(&entries)
        .into_iter()
        .flat_map(|segment| {
            let WindowFramePillboxTarget::Group(group_index) = segment.target else {
                return Vec::new();
            };
            let start = segment.start.min(usize::from(width));
            let end = segment
                .start
                .saturating_add(segment.width)
                .min(usize::from(width));
            (start..end)
                .filter_map(move |column| {
                    Some(MouseWindowGroupFrameCell {
                        column: u16::try_from(column).ok()?,
                        row,
                        group_index,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Carries the target represented by a window-frame pillbox segment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowFramePillboxTarget {
    /// The pill selects an existing window by display index.
    Window(usize),
    /// The pill selects an existing window group by display index.
    Group(usize),
    /// The pill triggers a built-in window status-bar action.
    Action(WindowFrameAction),
}

/// Carries Window Frame Pillbox Entry state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowFramePillboxEntry {
    /// Target represented by the rendered pill.
    target: WindowFramePillboxTarget,
    /// Copyable display text for the rendered pill.
    text: String,
    /// Whether the pill should use the active/pressed rendition.
    active: bool,
    /// Whether this entry represents a spawned-subagent window.
    subagent: bool,
}

impl WindowFramePillboxEntry {
    /// Builds an entry for a built-in window action control.
    fn action(action: WindowFrameAction, frame_context: &TerminalFrameContext) -> Self {
        let text = format!(" {} ", action.icon());
        let active = frame_context.pressed_window_action.as_ref() == Some(&action);
        Self {
            target: WindowFramePillboxTarget::Action(action),
            text,
            active,
            subagent: false,
        }
    }
}

impl From<&TerminalWindowFrameContext> for WindowFramePillboxEntry {
    /// Runs the from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from(window: &TerminalWindowFrameContext) -> Self {
        Self {
            target: WindowFramePillboxTarget::Window(window.index),
            text: format!(" {} {} ", window.index, sanitize_frame_text(&window.title)),
            active: window.active,
            subagent: window.subagent,
        }
    }
}

impl From<&TerminalWindowGroupFrameContext> for WindowFramePillboxEntry {
    /// Builds a pillbox entry for a window group.
    fn from(group: &TerminalWindowGroupFrameContext) -> Self {
        Self {
            target: WindowFramePillboxTarget::Group(group.index),
            text: format!(" {} {} ", group.index, sanitize_frame_text(&group.title)),
            active: group.active,
            subagent: false,
        }
    }
}

/// Carries Window Frame Pillbox Segment state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowFramePillboxSegment {
    /// Stores the start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    start: usize,
    /// Stores the width value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    width: usize,
    /// Stores the target value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    target: WindowFramePillboxTarget,
    /// Stores the active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    active: bool,
    /// Whether this segment represents a spawned-subagent window.
    subagent: bool,
}

/// Runs the window frame pillbox entries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_frame_pillbox_entries(
    window: &Window,
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    if frame_context.windows.is_empty() {
        return vec![WindowFramePillboxEntry {
            target: WindowFramePillboxTarget::Window(window.index),
            text: format!(
                " {} {} ",
                window.index,
                sanitize_frame_text(&window.title())
            ),
            active: true,
            subagent: false,
        }];
    }
    frame_context
        .windows
        .iter()
        .map(WindowFramePillboxEntry::from)
        .collect()
}

/// Builds default window-frame entries directly from runtime frame context.
fn window_frame_pillbox_entries_from_context(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    frame_context
        .windows
        .iter()
        .map(WindowFramePillboxEntry::from)
        .collect()
}

/// Returns default action pill entries for the window status bar.
fn window_action_pillbox_entries(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    WindowFrameAction::all()
        .into_iter()
        .map(|action| WindowFramePillboxEntry::action(action, frame_context))
        .collect()
}

/// Returns default pillbox entries for the top window-group bar.
fn group_frame_pillbox_entries(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    frame_context
        .groups
        .iter()
        .map(WindowFramePillboxEntry::from)
        .collect()
}

/// Runs the window frame pillbox text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_frame_pillbox_text(window: &Window, frame_context: &TerminalFrameContext) -> String {
    window_frame_pillbox_text_from_entries(&window_frame_pillbox_entries(window, frame_context))
}

/// Runs the window frame pillbox text from entries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_frame_pillbox_text_from_entries(entries: &[WindowFramePillboxEntry]) -> String {
    entries
        .iter()
        .map(|entry| entry.text.clone())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs the window frame pillbox segments operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_frame_pillbox_segments(
    entries: &[WindowFramePillboxEntry],
) -> Vec<WindowFramePillboxSegment> {
    let mut segments = Vec::with_capacity(entries.len());
    let mut start = 0usize;
    for (entry_index, entry) in entries.iter().enumerate() {
        if entry_index > 0 {
            start = start.saturating_add(1);
        }
        let width = char_count(&entry.text);
        segments.push(WindowFramePillboxSegment {
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

/// Runs the window frame field value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn window_frame_field_value(
    window: &Window,
    frame_context: &TerminalFrameContext,
    field: &str,
) -> String {
    let active_pane = window.active_pane();
    let value = match field {
        "session.id" => frame_context.session_id.clone().unwrap_or_default(),
        "window.id" => window.id.to_string(),
        "window.index" => window.index.to_string(),
        "window.list" => window_frame_pillbox_text(window, frame_context),
        "window.buttons" | "window.actions" => {
            window_frame_pillbox_text_from_entries(&window_action_pillbox_entries(frame_context))
        }
        "window.title" => window.title(),
        "window.name" => window.name.clone(),
        "window.active" => "true".to_string(),
        "window.pane_count" => window.panes().len().to_string(),
        "pane.id" => active_pane.id.to_string(),
        "pane.index" => active_pane.index.to_string(),
        "pane.title" => active_pane.title.clone(),
        "pane.active" => active_pane.active.to_string(),
        "layout.name" => window.layout_policy().name().to_string(),
        "agent.active_count" => frame_context
            .window_agent_active_counts
            .get(window.id.as_str())
            .copied()
            .unwrap_or_default()
            .to_string(),
        "message.unread_count" => frame_context
            .window_unread_message_counts
            .get(window.id.as_str())
            .copied()
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    sanitize_frame_text(&value)
}

/// Runs the pane frame field value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_frame_field_value(
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    field: &str,
) -> String {
    let pane_context = frame_context.panes.get(pane.id.as_str());
    let value =
        match field {
            "session.id" => frame_context.session_id.clone().unwrap_or_default(),
            "window.id" => window.id.to_string(),
            "window.index" => window.index.to_string(),
            "window.title" => window.title(),
            "window.name" => window.name.clone(),
            "window.active" => "true".to_string(),
            "window.pane_count" => window.panes().len().to_string(),
            "window.buttons" | "window.actions" => window_frame_pillbox_text_from_entries(
                &window_action_pillbox_entries(frame_context),
            ),
            "system.uptime" => frame_context
                .window_status
                .as_ref()
                .map(|status| status.system_uptime.clone())
                .unwrap_or_default(),
            "datetime.local" => frame_context
                .window_status
                .as_ref()
                .map(|status| status.datetime_local.clone())
                .unwrap_or_default(),
            "pane.id" => pane.id.to_string(),
            "pane.index" => pane.index.to_string(),
            "pane.title" => pane.title.clone(),
            "pane.active" => pane.active.to_string(),
            "pane.size" => format!("{}x{}", pane.size.columns, pane.size.rows),
            "pane.primary_pid" => {
                optional_u32_frame_value(pane_context.and_then(|ctx| ctx.primary_pid))
            }
            "pane.process_name" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.process_name)
                    .unwrap_or_default()
            }
            "pane.exit_status" => optional_pane_context_value(pane_context, |ctx| &ctx.exit_status)
                .unwrap_or_default(),
            "pane.pwd" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.current_working_directory)
                    .unwrap_or_default()
            }
            "pane.mode" => optional_pane_context_value(pane_context, |ctx| &ctx.mode)
                .unwrap_or_else(|| "normal".to_string()),
            "agent.id" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.agent_id).unwrap_or_default()
            }
            "agent.name" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.agent_name).unwrap_or_default()
            }
            "agent.status" => optional_pane_context_value(pane_context, |ctx| &ctx.agent_status)
                .unwrap_or_default(),
            "agent.model" => optional_pane_context_value(pane_context, |ctx| &ctx.agent_model)
                .unwrap_or_default(),
            "agent.reasoning" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.agent_reasoning)
                    .unwrap_or_default()
            }
            "agent.auto_reasoning" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.agent_auto_reasoning)
                    .unwrap_or_default()
            }
            "agent.latency" => optional_pane_context_value(pane_context, |ctx| &ctx.agent_latency)
                .unwrap_or_default(),
            "agent.preset" => optional_pane_context_value(pane_context, |ctx| &ctx.agent_preset)
                .unwrap_or_default(),
            "agent.context_usage" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.agent_context_usage)
                    .unwrap_or_default()
            }
            "policy.mode" => frame_context.policy_mode.clone().unwrap_or_default(),
            "observer.pending_count" => frame_context.pending_observer_count.to_string(),
            "history.position" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.history_position)
                    .unwrap_or_default()
            }
            _ => String::new(),
        };
    sanitize_frame_text(&value)
}

/// Runs the optional pane context value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_pane_context_value(
    pane_context: Option<&TerminalPaneFrameContext>,
    value: fn(&TerminalPaneFrameContext) -> &Option<String>,
) -> Option<String> {
    pane_context.and_then(|context| value(context).clone())
}

/// Runs the optional u32 frame value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_u32_frame_value(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Runs the sanitize frame text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sanitize_frame_text(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

/// Runs the write merged pane frames on dividers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_merged_pane_frames_on_dividers(
    canvas: &mut [Vec<char>],
    geometries: &[PaneGeometry],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
) {
    for geometry in geometries {
        if !pane_frame.enabled
            || !pane_frame_merges_into_divider(geometry, geometries, pane_frame.position)
        {
            continue;
        }
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.index == geometry.index)
            .unwrap_or_else(|| window.active_pane());
        let row = match pane_frame.position {
            TerminalFramePosition::Top => geometry.row.saturating_sub(1),
            TerminalFramePosition::Bottom => {
                geometry.row.saturating_add(geometry.rows).saturating_sub(1)
            }
        };
        let line = canvas.get_mut(usize::from(row));
        let Some(line) = line else {
            continue;
        };
        let column_start = usize::from(geometry.column);
        let width = usize::from(
            pane_render_region_size_for_geometry(geometry, geometries)
                .map(|s| s.columns)
                .unwrap_or(geometry.columns),
        );
        write_pane_frame_layout_cells(
            line,
            column_start,
            width,
            window,
            pane,
            frame_context,
            pane_frame.template,
        );
    }
}

/// Runs the write styled merged pane frames on dividers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_styled_merged_pane_frames_on_dividers(
    text_canvas: &mut [Vec<char>],
    style_canvas: &mut [Vec<TerminalStyleSpan>],
    geometries: &[PaneGeometry],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    ui_theme: &UiTheme,
) {
    for geometry in geometries {
        if !pane_frame.enabled
            || !pane_frame_merges_into_divider(geometry, geometries, pane_frame.position)
        {
            continue;
        }
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.index == geometry.index)
            .unwrap_or_else(|| window.active_pane());
        let row = match pane_frame.position {
            TerminalFramePosition::Top => geometry.row.saturating_sub(1),
            TerminalFramePosition::Bottom => {
                geometry.row.saturating_add(geometry.rows).saturating_sub(1)
            }
        };
        let line = text_canvas.get_mut(usize::from(row));
        let Some(line) = line else {
            continue;
        };
        let column_start = usize::from(geometry.column);
        let width = usize::from(
            pane_render_region_size_for_geometry(geometry, geometries)
                .map(|s| s.columns)
                .unwrap_or(geometry.columns),
        );
        let layout = write_pane_frame_layout_cells(
            line,
            column_start,
            width,
            window,
            pane,
            frame_context,
            pane_frame.template,
        );
        if let Some(spans) = style_canvas.get_mut(usize::from(row)) {
            if layout.left_text_width > 0 {
                spans.push(TerminalStyleSpan {
                    start: column_start,
                    length: layout.left_text_width,
                    rendition: pane_frame_rendition(pane, pane_frame.style, ui_theme),
                });
            }
            spans.extend(pane_frame_right_status_style_spans(
                &layout,
                column_start,
                frame_context,
                ui_theme,
            ));
            spans.extend(merged_pane_frame_boundary_style_spans(
                geometries,
                window,
                row,
                column_start,
                width,
                ui_theme,
            ));
        }
    }
}

/// Writes a pane frame into a divider row as a complete status-bar region.
fn write_pane_frame_layout_cells(
    row: &mut [char],
    column_start: usize,
    max_columns: usize,
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
) -> PaneFrameRowLayout {
    let layout = pane_frame_row_layout(
        window,
        pane,
        frame_context,
        template,
        max_columns,
        pane_frame_fill_char(template),
    );
    write_frame_text_cells(row, column_start, max_columns, &layout.text);
    layout
}

/// Builds style spans for divider junctions that bound a merged pane status row.
fn merged_pane_frame_boundary_style_spans(
    geometries: &[PaneGeometry],
    window: &Window,
    row: u16,
    column_start: usize,
    width: usize,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    pane_divider_cells(geometries, true)
        .into_iter()
        .filter(|cell| {
            cell.row == row && merged_pane_frame_boundary_cell(*cell, column_start, width)
        })
        .map(|cell| TerminalStyleSpan {
            start: usize::from(cell.column),
            length: 1,
            rendition: pane_border_rendition(
                divider_cell_touches_active_pane(cell, geometries, window),
                ui_theme,
            ),
        })
        .collect()
}

/// Returns whether a divider cell acts as a non-vertical boundary cap for a
/// merged pane status row.
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

/// Runs the right aligned status bounds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn right_aligned_status_bounds(status: &str, width: usize) -> Option<(usize, usize)> {
    let status_limit = width.saturating_sub(usize::from(width > 1));
    let status_width = fitted_text_width(status, status_limit);
    if status_width == 0 {
        return None;
    }
    let trailing_padding = usize::from(width > status_width);
    let start = width.saturating_sub(status_width.saturating_add(trailing_padding));
    Some((start, status_width))
}

/// Writes text into a row of terminal cells without padding with spaces.
/// Returns the number of cells consumed (useful for style span bounds).
fn write_frame_text_cells(
    row: &mut [char],
    column_start: usize,
    max_columns: usize,
    text: &str,
) -> usize {
    let mut used = 0usize;
    for grapheme in terminal_graphemes(text) {
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
                row[continuation_cell] = ' ';
            }
        }
        used = used.saturating_add(grapheme_width);
    }
    used
}

/// Runs the render panes by geometry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_panes_by_geometry(
    size: Size,
    geometries: &[PaneGeometry],
    rendered_panes: &[Vec<String>],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
) -> Vec<String> {
    let rows = usize::from(size.rows);
    let columns = usize::from(size.columns);
    let mut canvas = vec![vec![' '; columns]; rows];

    for geometry in geometries {
        let Some(pane) = rendered_panes.get(geometry.index) else {
            continue;
        };
        let row_start = usize::from(geometry.row);
        let column_start = usize::from(geometry.column);
        if row_start >= rows || column_start >= columns {
            continue;
        }
        let region_size =
            pane_render_region_size_for_geometry(geometry, geometries).unwrap_or(Size {
                columns: geometry.columns,
                rows: geometry.rows,
            });
        let pane_rows = usize::from(region_size.rows).min(rows.saturating_sub(row_start));
        let pane_columns =
            usize::from(region_size.columns).min(columns.saturating_sub(column_start));
        for row_offset in 0..pane_rows {
            if let Some(line) = pane.get(row_offset) {
                write_text_cells(
                    &mut canvas[row_start + row_offset],
                    column_start,
                    pane_columns,
                    line,
                );
            }
        }
    }
    draw_pane_dividers(&mut canvas, geometries, true);

    write_merged_pane_frames_on_dividers(
        &mut canvas,
        geometries,
        window,
        frame_context,
        pane_frame,
    );

    canvas.into_iter().map(collect_text_cells).collect()
}

/// Runs the render styled panes by geometry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn render_styled_panes_by_geometry(
    size: Size,
    geometries: &[PaneGeometry],
    rendered_panes: &[Vec<TerminalStyledLine>],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyledLine> {
    let rows = usize::from(size.rows);
    let columns = usize::from(size.columns);
    let mut text_canvas = vec![vec![' '; columns]; rows];
    let mut style_canvas = vec![Vec::new(); rows];

    for geometry in geometries {
        let Some(pane) = rendered_panes.get(geometry.index) else {
            continue;
        };
        let row_start = usize::from(geometry.row);
        let column_start = usize::from(geometry.column);
        if row_start >= rows || column_start >= columns {
            continue;
        }
        let region_size =
            pane_render_region_size_for_geometry(geometry, geometries).unwrap_or(Size {
                columns: geometry.columns,
                rows: geometry.rows,
            });
        let pane_rows = usize::from(region_size.rows).min(rows.saturating_sub(row_start));
        let pane_columns =
            usize::from(region_size.columns).min(columns.saturating_sub(column_start));
        for row_offset in 0..pane_rows {
            let Some(line) = pane.get(row_offset) else {
                continue;
            };
            write_text_cells(
                &mut text_canvas[row_start + row_offset],
                column_start,
                pane_columns,
                &line.text,
            );
            style_canvas[row_start + row_offset].extend(
                line.style_spans
                    .iter()
                    .filter_map(|span| clip_style_span(*span, pane_columns))
                    .map(|span| offset_style_span(span, column_start)),
            );
        }
    }
    draw_styled_pane_dividers(
        &mut text_canvas,
        &mut style_canvas,
        geometries,
        true,
        window,
        ui_theme,
    );

    write_styled_merged_pane_frames_on_dividers(
        &mut text_canvas,
        &mut style_canvas,
        geometries,
        window,
        frame_context,
        pane_frame,
        ui_theme,
    );

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

/// Returns the rendered cells occupied by mux-managed pane dividers.
pub fn pane_border_cells_for_geometries(
    geometries: &[PaneGeometry],
    row_offset: u16,
) -> Vec<MouseBorderCell> {
    pane_divider_cells(geometries, true)
        .into_iter()
        .map(|cell| MouseBorderCell {
            column: cell.column,
            row: cell.row.saturating_add(row_offset),
        })
        .collect()
}

/// Carries Pane Divider Cell state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneDividerCell {
    /// Stores the column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    column: u16,
    /// Stores the row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    row: u16,
    /// Stores the glyph value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    glyph: char,
}

/// Directional strokes that meet in one mux-managed pane divider cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PaneDividerConnections {
    /// Stores the up value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    up: bool,
    /// Stores the down value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    down: bool,
    /// Stores the left value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    left: bool,
    /// Stores the right value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    right: bool,
    /// Stores the vertical value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    vertical: bool,
    /// Stores the horizontal value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    horizontal: bool,
}

impl PaneDividerConnections {
    /// Records a vertical divider stroke through this cell.
    fn add_vertical(&mut self, up: bool, down: bool) {
        self.vertical = true;
        self.up |= up;
        self.down |= down;
    }

    /// Records a horizontal divider stroke through this cell.
    fn add_horizontal(&mut self, left: bool, right: bool) {
        self.horizontal = true;
        self.left |= left;
        self.right |= right;
    }

    /// Returns whether this cell belongs to a vertical divider segment.
    fn has_vertical(&self) -> bool {
        self.vertical
    }

    /// Returns whether this cell belongs to a horizontal divider segment.
    fn has_horizontal(&self) -> bool {
        self.horizontal
    }

    /// Chooses the thin box-drawing glyph that matches the recorded strokes.
    fn glyph(self) -> char {
        let mut up = self.up;
        let mut down = self.down;
        let mut left = self.left;
        let mut right = self.right;
        if self.vertical && !up && !down {
            up = true;
            down = true;
        }
        if self.horizontal && !left && !right {
            left = true;
            right = true;
        }
        match (up, down, left, right) {
            (true, true, true, true) => '\u{253c}',
            (true, true, true, false) => '\u{2524}',
            (true, true, false, true) => '\u{251c}',
            (true, false, true, true) => '\u{2534}',
            (false, true, true, true) => '\u{252c}',
            (true, false, false, true) => '\u{2514}',
            (true, false, true, false) => '\u{2518}',
            (false, true, false, true) => '\u{250c}',
            (false, true, true, false) => '\u{2510}',
            (true, true, false, false) => '\u{2502}',
            (false, false, true, true) => '\u{2500}',
            (true, false, false, false) | (false, true, false, false) => '\u{2502}',
            (false, false, true, false) | (false, false, false, true) => '\u{2500}',
            (false, false, false, false) => ' ',
        }
    }
}

/// Returns the box-drawing glyph for an explicit divider-connection mask.
///
/// This test helper keeps the glyph contract covered independently from any
/// particular split-tree shape, while production rendering still derives the
/// same connection mask from pane geometry.
#[cfg(test)]
pub(crate) fn pane_divider_glyph_for_test(up: bool, down: bool, left: bool, right: bool) -> char {
    PaneDividerConnections {
        up,
        down,
        left,
        right,
        vertical: up || down,
        horizontal: left || right,
    }
    .glyph()
}

/// Runs the pane divider cells operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_divider_cells(
    geometries: &[PaneGeometry],
    include_horizontal: bool,
) -> Vec<PaneDividerCell> {
    if geometries.len() < 2 {
        return Vec::new();
    }
    let mut cells = BTreeMap::new();
    for (index, first) in geometries.iter().enumerate() {
        for second in geometries.iter().skip(index.saturating_add(1)) {
            let first_right = first.column.saturating_add(first.columns);
            let second_right = second.column.saturating_add(second.columns);
            let first_bottom = first.row.saturating_add(first.rows);
            let second_bottom = second.row.saturating_add(second.rows);

            if first_right == second.column || second_right == first.column {
                let boundary = first_right.min(second_right).saturating_sub(1);
                let start = first.row.max(second.row);
                let end = first_bottom.min(second_bottom);
                for row in start..end {
                    insert_vertical_divider_cell(
                        &mut cells,
                        boundary,
                        row,
                        row > start,
                        row.saturating_add(1) < end,
                    );
                }
            }
            if include_horizontal && (first_bottom == second.row || second_bottom == first.row) {
                let boundary = first_bottom.min(second_bottom).saturating_sub(1);
                let start = first.column.max(second.column);
                let end = first_right.min(second_right);
                for column in start..end {
                    insert_horizontal_divider_cell(
                        &mut cells,
                        column,
                        boundary,
                        column > start,
                        column.saturating_add(1) < end,
                    );
                }
            }
        }
    }
    connect_touching_divider_cells(&mut cells);
    cells
        .into_iter()
        .map(|((column, row), connections)| PaneDividerCell {
            column,
            row,
            glyph: connections.glyph(),
        })
        .collect()
}

/// Runs the geometry has above divider operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn geometry_has_above_divider(geometry: &PaneGeometry, geometries: &[PaneGeometry]) -> bool {
    geometries.iter().any(|candidate| {
        candidate.index != geometry.index
            && candidate.row.saturating_add(candidate.rows) == geometry.row
            && range_overlap_u16(
                geometry.column,
                geometry.column.saturating_add(geometry.columns),
                candidate.column,
                candidate.column.saturating_add(candidate.columns),
            ) > 0
    })
}

/// Runs the pane frame merges into divider operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn pane_frame_merges_into_divider(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    frame_position: TerminalFramePosition,
) -> bool {
    match frame_position {
        TerminalFramePosition::Top => geometry_has_above_divider(geometry, geometries),
        TerminalFramePosition::Bottom => geometry_has_bottom_divider(geometry, geometries),
    }
}

/// Runs the geometry has bottom divider operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn geometry_has_bottom_divider(geometry: &PaneGeometry, geometries: &[PaneGeometry]) -> bool {
    let bottom = geometry.row.saturating_add(geometry.rows);
    geometries.iter().any(|candidate| {
        candidate.index != geometry.index
            && candidate.row == bottom
            && range_overlap_u16(
                geometry.column,
                geometry.column.saturating_add(geometry.columns),
                candidate.column,
                candidate.column.saturating_add(candidate.columns),
            ) > 0
    })
}

/// Returns whether the geometry's right edge is occupied by a shared divider.
fn geometry_has_right_divider(geometry: &PaneGeometry, geometries: &[PaneGeometry]) -> bool {
    let right = geometry.column.saturating_add(geometry.columns);
    geometries.iter().any(|candidate| {
        candidate.index != geometry.index
            && candidate.column == right
            && range_overlap_u16(
                geometry.row,
                geometry.row.saturating_add(geometry.rows),
                candidate.row,
                candidate.row.saturating_add(candidate.rows),
            ) > 0
    })
}

/// Runs the range overlap u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn range_overlap_u16(first_start: u16, first_end: u16, second_start: u16, second_end: u16) -> u16 {
    first_end
        .min(second_end)
        .saturating_sub(first_start.max(second_start))
}

/// Inserts one cell from a vertical divider segment.
fn insert_vertical_divider_cell(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    column: u16,
    row: u16,
    up: bool,
    down: bool,
) {
    cells
        .entry((column, row))
        .or_default()
        .add_vertical(up, down);
}

/// Inserts one cell from a horizontal divider segment.
fn insert_horizontal_divider_cell(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    column: u16,
    row: u16,
    left: bool,
    right: bool,
) {
    cells
        .entry((column, row))
        .or_default()
        .add_horizontal(left, right);
}

/// Connects divider cells that touch because neighboring panes share a seam.
fn connect_touching_divider_cells(cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>) {
    let snapshot = cells.clone();
    for (&(column, row), connections) in &snapshot {
        if connections.has_vertical() {
            if let Some(below_row) = row.checked_add(1) {
                let below = (column, below_row);
                if snapshot
                    .get(&below)
                    .is_some_and(PaneDividerConnections::has_vertical)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.down = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&below) {
                        neighbor.up = true;
                    }
                }
            }
            if column > 0 {
                let left = (column.saturating_sub(1), row);
                if snapshot
                    .get(&left)
                    .is_some_and(PaneDividerConnections::has_horizontal)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.left = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&left) {
                        neighbor.right = true;
                    }
                }
            }
            if let Some(right_column) = column.checked_add(1) {
                let right = (right_column, row);
                if snapshot
                    .get(&right)
                    .is_some_and(PaneDividerConnections::has_horizontal)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.right = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&right) {
                        neighbor.left = true;
                    }
                }
            }
        }
        if connections.has_horizontal() {
            if let Some(right_column) = column.checked_add(1) {
                let right = (right_column, row);
                if snapshot
                    .get(&right)
                    .is_some_and(PaneDividerConnections::has_horizontal)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.right = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&right) {
                        neighbor.left = true;
                    }
                }
            }
            if row > 0 {
                let above = (column, row.saturating_sub(1));
                if snapshot
                    .get(&above)
                    .is_some_and(PaneDividerConnections::has_vertical)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.up = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&above) {
                        neighbor.down = true;
                    }
                }
            }
            if let Some(below_row) = row.checked_add(1) {
                let below = (column, below_row);
                if snapshot
                    .get(&below)
                    .is_some_and(PaneDividerConnections::has_vertical)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.down = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&below) {
                        neighbor.up = true;
                    }
                }
            }
        }
    }
}

/// Runs the draw pane dividers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn draw_pane_dividers(
    canvas: &mut [Vec<char>],
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

/// Runs the draw styled pane dividers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn draw_styled_pane_dividers(
    text_canvas: &mut [Vec<char>],
    style_canvas: &mut [Vec<TerminalStyleSpan>],
    geometries: &[PaneGeometry],
    include_horizontal: bool,
    window: &Window,
    ui_theme: &UiTheme,
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
                rendition: pane_border_rendition(
                    divider_cell_touches_active_pane(cell, geometries, window),
                    ui_theme,
                ),
            });
        }
    }
}

/// Runs the divider cell touches active pane operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn divider_cell_touches_active_pane(
    cell: PaneDividerCell,
    geometries: &[PaneGeometry],
    window: &Window,
) -> bool {
    let active_index = window.active_pane_index();
    let Some(geometry) = geometries
        .iter()
        .find(|geometry| geometry.index == active_index)
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

/// Internal marker for cells occupied by the continuation half of a wide glyph.
const TERMINAL_WIDE_CONTINUATION_CELL: char = '\0';

/// Writes one single-width cell while removing any overlapping wide glyph.
///
/// A divider or frame cell can land on either half of a previously rendered
/// wide glyph. If only the sentinel half is overwritten, the leading glyph
/// would still consume two terminal cells when collected into a string and
/// would shift everything to its right. Clearing both halves keeps the canvas
/// and the terminal's display-width model aligned.
fn write_single_width_cell(row: &mut [char], column: usize, glyph: char) {
    if column >= row.len() {
        return;
    }
    if row[column] == TERMINAL_WIDE_CONTINUATION_CELL && column > 0 {
        row[column - 1] = ' ';
    }
    if row
        .get(column.saturating_add(1))
        .is_some_and(|next| *next == TERMINAL_WIDE_CONTINUATION_CELL)
    {
        row[column.saturating_add(1)] = ' ';
    }
    row[column] = glyph;
}

/// Runs the write text cells operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_text_cells(row: &mut [char], column_start: usize, max_columns: usize, text: &str) {
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
fn collect_text_cells(row: Vec<char>) -> String {
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

/// Runs the blank cells operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn blank_cells(size: Size) -> Vec<Vec<char>> {
    (0..size.rows).map(|_| blank_row(size.columns)).collect()
}

/// Runs the blank row operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn blank_row(columns: u16) -> Vec<char> {
    vec![' '; usize::from(columns)]
}

/// Runs the trim row operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn trim_row(row: &[char]) -> String {
    row.iter().collect::<String>().trim_end().to_string()
}

/// Runs the fit width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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

/// Runs the fitted text width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn fitted_text_width(value: &str, max_width: usize) -> usize {
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

/// Runs the fit styled width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn fit_styled_width(line: &TerminalStyledLine, width: usize) -> TerminalStyledLine {
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

/// Runs the offset style span operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn offset_style_span(span: TerminalStyleSpan, column_offset: usize) -> TerminalStyleSpan {
    TerminalStyleSpan {
        start: span.start.saturating_add(column_offset),
        length: span.length,
        rendition: span.rendition,
    }
}

/// Runs the clip style span operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn clip_style_span(span: TerminalStyleSpan, width: usize) -> Option<TerminalStyleSpan> {
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

/// Runs the search forward operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn search_forward(
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

/// Runs the search backward operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn search_backward(
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

/// Runs the validate copy position operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_copy_position(lines: &[String], position: CopyPosition) -> Result<()> {
    if lines.get(position.line).is_none() {
        return Err(MezError::invalid_args(
            "copy mode selection line is out of range",
        ));
    }
    Ok(())
}

/// Runs the normalize selection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn normalize_selection(
    start: CopyPosition,
    end: CopyPosition,
) -> (CopyPosition, CopyPosition) {
    if start <= end {
        (start, end)
    } else {
        (end, start)
    }
}

/// Runs the line slice operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn line_slice(line: &str, start: usize, end: usize) -> String {
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

/// Runs the char count operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn char_count(value: &str) -> usize {
    terminal_text_width(value)
}

/// Runs the char column at byte operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn char_column_at_byte(value: &str, byte_index: usize) -> usize {
    terminal_text_width(&value[..byte_index])
}

/// Runs the terminal char width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_char_width(ch: char) -> usize {
    if terminal_scalar_has_emoji_presentation_width(ch) {
        return 2;
    }
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Returns the display width of one Unicode grapheme cluster.
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
    UnicodeWidthStr::width(grapheme)
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
