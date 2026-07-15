//! Terminal Render implementation.
//!
//! This module owns the terminal render boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS, BTreeMap, ClientStatusKind, ClientStatusLine,
    GraphicRendition, MezError, MousePaneAgentStatusCell, MouseWindowActionFrameCell,
    PaneAgentStatusField, PaneRenderInput, Result, TerminalClientLoopConfig, TerminalFrameContext,
    TerminalPaneFrameContext, TerminalScreen, TerminalStyleSpan, TerminalStyledLine,
    WindowFrameAction,
};
use mez_mux::input::{MouseWindowFrameCell, MouseWindowGroupFrameCell};
use mez_mux::layout::{PaneGeometry, Size, Window};
use mez_mux::presentation::{ClientViewRole, ReadlinePromptRegion, RenderedClientView};
use mez_mux::presentation::{
    TerminalFramePosition, TerminalFrameStyle, TerminalWindowFrameContext,
    TerminalWindowGroupFrameContext, TerminalWindowStatusContext, compose_client_viewport,
};
pub(crate) use mez_mux::render::overlay_fixed_column_style_spans;
use mez_mux::render::{
    FramePillboxEntry, FramePillboxSegment, FrameStatusSegment, FrameStatusValue,
    PositionedFrameStatus, RenderedFrameStatus, TerminalRenderCell,
    clip_style_spans as clipped_style_spans, compose_frame_pillbox_row, compose_frame_text_row,
    compose_pane_frame_row, display_overlay_targets as agent_display_overlay_targets,
    fit_styled_width, fit_width, fitted_text_width, frame_pillbox_segment_columns,
    frame_style_rendition, overlay_display_lines as overlay_agent_display_lines,
    position_frame_status, render_frame_pillbox_segments, render_frame_pillbox_text,
    render_frame_status, sanitize_frame_text, style_span_overlaps_columns,
    style_span_segments_outside_range, styled_frame_line_with_rendition,
    write_text_cells_with_width as write_frame_text_cells,
};
pub(super) use mez_mux::render::{char_count, line_slice};
use mez_mux::theme::{UiColorPair, UiTheme};

// Client view composition and pane/window rendering.
mod dividers;
mod frame;
mod overlay;
mod panes;
mod prompt;
mod style;

mod text;

#[cfg(test)]
pub(crate) use dividers::pane_divider_glyph_for_test;
use dividers::{merged_pane_frame_boundary_style_spans, pane_divider_rendition};
pub use dividers::{pane_border_cells_for_geometries, pane_frame_merges_into_divider};
use frame::{
    AGENT_STATUS_SCAN_BAND_WIDTH, group_frame_text, pane_agent_prompt_space_reserved,
    pane_agent_prompt_transparent, pane_agent_shell_visible, pane_border_rendition,
    render_pane_lines, render_styled_pane_lines, render_window_frame_text, styled_group_frame_line,
    styled_window_frame_line, write_merged_pane_frames_on_dividers,
    write_styled_merged_pane_frames_on_dividers,
};
pub use frame::{
    pane_frame_agent_status_pillbox_cells, window_frame_action_pillbox_cells,
    window_frame_pillbox_cells, window_group_frame_pillbox_cells,
};
use mez_mux::presentation::{place_group_frame, place_window_frame};
pub use overlay::{
    compose_display_overlay_line_style_spans, compose_display_overlay_lines,
    compose_modal_display_overlay_line_style_spans, compose_modal_display_overlay_lines,
    modal_display_overlay_max_scroll, modal_display_overlay_page_rows,
};
pub(super) use overlay::{
    normalize_overlay_canvas, normalize_overlay_style_spans, overlay_text_style_width,
    status_line_rendition,
};
pub use panes::{
    draw_styled_window_from_screens, draw_window_from_screens, pane_content_size_for_geometry,
    pane_render_region_size_for_geometry, render_window, render_window_with_pane_frame_template,
    rendered_pane_geometries,
};
use panes::{window_body_size, zoomed_pane_geometry};
pub(crate) use prompt::agent_prompt_input_rendition;
use prompt::{
    AgentPromptBlock, agent_live_footer_state_label, agent_live_footer_style_spans,
    clipped_prompt_region, display_overlay_text_rendition, render_agent_prompt_block,
    write_line_segment,
};
pub use prompt::{
    agent_prompt_reserved_line_count, compose_display_region_overlay_line_style_spans,
    compose_display_region_overlay_lines, compose_prompt_overlay_lines,
    compose_prompt_overlay_presentation, compose_prompt_overlay_presentation_with_styles,
    compose_prompt_region_presentation_with_styles, compose_readline_prompt_client_presentation,
    render_readline_prompt_status_row,
};
use style::{
    agent_status_running_gradient_palette, animated_scan_background, blend_terminal_color,
    contrasting_binary_foreground, gradient_highlight_for_offset, neutral_surface_step,
    push_or_extend_style_span,
};
pub(crate) use text::{
    DEFAULT_AGENT_WRAP_COLUMN_CAP, TerminalEmojiWidth, agent_log_wrap_width, agent_wrap_column_cap,
    set_agent_wrap_column_cap, set_terminal_emoji_width, terminal_grapheme_width,
    terminal_graphemes, terminal_text_width, wrap_agent_log_lines,
};

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
    "agent.thinking",
    "agent.routing",
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
    "agent.thinking",
    "agent.routing",
    "agent.latency",
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
    let active_pane_screen = window
        .panes()
        .get(window.active_pane_index())
        .and_then(|active_pane| screens.get(&active_pane.id.to_string()));
    Ok(Some(RenderedClientView {
        role,
        authoritative_size: window.size,
        client_size,
        lines,
        line_style_spans,
        selection: None,
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
        focus_events: active_pane_screen.is_some_and(|screen| screen.focus_events_enabled()),
        alternate_screen: active_pane_screen.is_some_and(|screen| screen.alternate_screen_active()),
        host_mouse_reporting: config.mouse_policy.enabled,
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
        vec![zoomed_pane_geometry(active_index, body_size)]
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
        .unwrap_or(mez_terminal::TerminalCursorState { row: 0, column: 0 });
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
        vec![zoomed_pane_geometry(active_index, body_size)]
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
    overlay_fixed_column_style_spans(spans, column, width, &styled_line.style_spans);
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

/// Identifies prompt-block styles that should not survive a resize mismatch.
fn style_span_is_agent_prompt_block(
    span: TerminalStyleSpan,
    prompt_rendition: GraphicRendition,
    display_rendition: GraphicRendition,
) -> bool {
    span.rendition == prompt_rendition || span.rendition == display_rendition
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
    let (mut lines, mut line_style_spans) = compose_client_viewport(view);
    let target_rows = lines.len();
    let target_columns = if view.requires_client_scroll {
        usize::from(view.client_size.columns)
    } else {
        usize::from(view.authoritative_size.columns)
    };
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
