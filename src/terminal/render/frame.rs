//! Frame and status-bar rendering helpers.
//!
//! This module owns window-group frames, window status templates, pane frame
//! text, pane right-status pills, and the hit cells derived from those rendered
//! frame rows. It deliberately keeps frame text and frame hit testing together
//! so mouse targeting uses the same layout calculations as drawing.

use super::*;

/// Runs the place window frame operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn place_window_frame<T>(
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
pub(super) fn place_group_frame<T>(lines: &mut Vec<T>, frame: T, authoritative_rows: u16) {
    let rows = usize::from(authoritative_rows);
    lines.insert(0, frame);
    lines.truncate(rows);
}

/// Renders the unstyled top group bar when more than one group exists.
pub(super) fn group_frame_text(
    frame_context: &TerminalFrameContext,
    width: usize,
) -> Option<String> {
    group_frame_visible(frame_context).then(|| {
        fit_width(
            &window_frame_pillbox_text_from_entries(&group_frame_pillbox_entries(frame_context)),
            width,
        )
    })
}

/// Renders the styled top group bar when more than one group exists.
pub(super) fn styled_group_frame_line(
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
pub(super) fn styled_window_frame_line(
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
pub(super) fn styled_window_pillbox_line(
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
pub(super) fn window_pillbox_rendition(
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
pub(super) fn styled_pane_frame_line(
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
pub(super) fn pane_frame_right_status_style_spans(
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
pub(super) fn pane_frame_right_status_segment_style_spans(
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
pub(super) fn pane_frame_right_status_rendition(
    segment: &PaneFrameRightStatusSegment,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    match segment.field {
        "history.position" => ui_theme.colors.scroll_indicator.rendition(),
        "pane.pwd" => ui_theme.colors.pane_pwd.rendition(),
        "agent.model" => ui_theme.colors.agent_model.rendition(),
        "agent.reasoning" => ui_theme.colors.agent_reasoning.rendition(),
        "agent.routing" => pane_frame_agent_routing_rendition(&segment.value, ui_theme),
        "agent.latency" => pane_frame_latency_rendition(&segment.value, ui_theme),
        "agent.preset" => ui_theme.colors.agent_model.rendition(),
        "agent.name" => ui_theme.colors.agent_model.rendition(),
        "agent.context_usage" => pane_frame_agent_context_usage_rendition(&segment.value, ui_theme),
        "agent.status" => pane_frame_agent_status_rendition(&segment.value, ui_theme),
        "policy.mode" => pane_frame_policy_mode_rendition(&segment.value, ui_theme),
        _ => ui_theme.colors.scroll_indicator.rendition(),
    }
}

/// Returns the routing pill rendition for a pane-local value.
pub(super) fn pane_frame_agent_routing_rendition(
    value: &str,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    match value {
        "auto:on" => ui_theme.colors.agent_reasoning.rendition(),
        "auto:off" => ui_theme.colors.agent_status_idle.rendition(),
        _ => ui_theme.colors.scroll_indicator.rendition(),
    }
}

/// Returns the latency-preference pill rendition for a pane-local value.
pub(super) fn pane_frame_latency_rendition(value: &str, ui_theme: &UiTheme) -> GraphicRendition {
    match value {
        "slow" => ui_theme.colors.agent_status_idle.rendition(),
        "fast" => ui_theme.colors.agent_status_running.rendition(),
        _ => ui_theme.colors.agent_model.rendition(),
    }
}

/// Returns the approval-policy pill rendition for a pane-local value.
pub(super) fn pane_frame_policy_mode_rendition(
    value: &str,
    ui_theme: &UiTheme,
) -> GraphicRendition {
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
pub(super) fn pane_frame_agent_context_usage_rendition(
    value: &str,
    ui_theme: &UiTheme,
) -> GraphicRendition {
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
pub(super) fn pane_frame_agent_status_rendition(
    status: &str,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    match status {
        "queued" | "running" | "thinking" | "routing" | "executing" | "waiting" | "compacting" => {
            ui_theme.colors.agent_status_running.rendition()
        }
        "blocked" | "waiting_approval" => ui_theme.colors.agent_status_blocked.rendition(),
        "failed" => ui_theme.colors.agent_status_failed.rendition(),
        "interrupted" | "stopped" => ui_theme.colors.agent_status_idle.rendition(),
        _ => ui_theme.colors.agent_status_idle.rendition(),
    }
}

/// Returns whether an agent status should render with active-work animation.
pub(super) fn pane_frame_agent_status_is_active(status: &str) -> bool {
    matches!(
        status,
        "queued" | "running" | "thinking" | "routing" | "executing" | "waiting" | "compacting"
    )
}

/// Shared scan width for active agent status animations.
pub(super) const AGENT_STATUS_SCAN_BAND_WIDTH: usize = 9;

/// Builds the animated scan background for an active agent status pill.
pub(super) fn pane_frame_agent_status_scan_spans(
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

/// Runs the subtle frame fill span operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn subtle_frame_fill_span(
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
pub(super) fn styled_frame_line_with_rendition(
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
pub(super) fn pane_frame_rendition(
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
pub(super) fn pane_border_rendition(active: bool, ui_theme: &UiTheme) -> GraphicRendition {
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

/// Runs the themed frame rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn themed_frame_rendition(
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
pub(super) fn frame_style_rendition(frame_style: TerminalFrameStyle) -> Option<GraphicRendition> {
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
pub(super) fn overlay_agent_display_lines<T: Clone>(
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
pub(super) fn agent_display_overlay_targets<T>(
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
pub(super) fn render_styled_pane_lines(
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
pub(super) fn render_pane_frame_text(
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
pub(super) struct PaneFrameRowLayout {
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
pub(super) struct PaneFrameRightStatusSegment {
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
pub(super) struct PaneFrameRightValue {
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
pub(super) struct RenderedPaneFrameRightStatus {
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
pub(super) fn pane_frame_row_layout(
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
pub(super) fn pane_frame_fill_char(template: &str) -> char {
    if template == DEFAULT_PANE_FRAME_TEMPLATE {
        '─'
    } else {
        ' '
    }
}

/// Renders pane-frame title text over horizontal border fill.
pub(super) fn pane_frame_text_with_fill(text: &str, width: usize, fill: char) -> (String, usize) {
    let mut row = vec![fill; width];
    let written_width = write_frame_text_cells(&mut row, 0, width, text);
    (row.into_iter().collect(), written_width)
}

/// Extends the pane title pill over the blank separator before right status.
///
/// The row text already reserves this separator so right-aligned status stays
/// readable. Including it in the title style span makes the right-side padding
/// visible as part of the pane title pill instead of leaving a bare gap.
pub(super) fn pane_frame_left_pill_style_width(text_width: usize, available_width: usize) -> usize {
    if text_width > 0 && text_width < available_width {
        text_width.saturating_add(1)
    } else {
        text_width
    }
}

/// Builds the pane-frame right status, prioritizing scrollback position before
/// pane-local agent state.
pub(super) fn pane_frame_right_status(
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
pub(super) fn pane_frame_right_aligned_values(
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
pub(super) fn render_pane_frame_right_status(
    values: &[PaneFrameRightValue],
) -> RenderedPaneFrameRightStatus {
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
pub(super) fn pane_agent_shell_visible(
    frame_context: &TerminalFrameContext,
    pane_id: &str,
) -> bool {
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
pub(super) fn pane_agent_prompt_space_reserved(
    pane_context: Option<&TerminalPaneFrameContext>,
) -> bool {
    pane_context.is_some_and(|context| {
        context.agent_prompt.is_some() || context.mode.as_deref() == Some("agent")
    })
}

/// Runs the pane agent prompt transparent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_agent_prompt_transparent(
    frame_context: &TerminalFrameContext,
    pane_id: &str,
) -> bool {
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
pub(super) fn pane_frame_right_aligned_display_value(field: &str, value: String) -> String {
    if matches!(
        field,
        "pane.pwd"
            | "agent.model"
            | "agent.reasoning"
            | "agent.routing"
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
pub(super) fn pane_frame_right_aligned_segment_value(field: &str, value: &str) -> String {
    if field == "agent.name" && value.trim() == "manager" {
        return String::new();
    }
    if field == "agent.routing" && !value.trim().is_empty() {
        return "route".to_string();
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
pub(super) fn render_window_frame_text(
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
pub(super) enum WindowStatusSegmentKind {
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
pub(super) struct WindowStatusSegment {
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
pub(super) struct WindowRightStatusLayout {
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
pub(super) fn window_right_status_layout(
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
pub(super) struct RenderedWindowStatusTemplate {
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
pub(super) struct WindowStatusFieldComponent {
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
pub(super) fn render_window_status_template(
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
pub(super) fn window_status_field_component(
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
pub(super) fn window_actions_status_component(
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
pub(super) fn window_action_status_component(
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
pub(super) fn window_status_template_button_action(field: &str) -> Option<WindowFrameAction> {
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
pub(super) fn window_status_field_value(
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
pub(super) fn window_status_style_spans(
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
pub(super) fn pane_agent_status_field_from_frame_field(
    field: &str,
) -> Option<PaneAgentStatusField> {
    match field {
        "agent.model" => Some(PaneAgentStatusField::Model),
        "agent.reasoning" => Some(PaneAgentStatusField::Reasoning),
        "agent.routing" => Some(PaneAgentStatusField::Routing),
        "agent.latency" => Some(PaneAgentStatusField::Latency),
        "agent.preset" => Some(PaneAgentStatusField::Preset),
        "policy.mode" => Some(PaneAgentStatusField::ApprovalPolicy),
        _ => None,
    }
}

/// Returns the rendered terminal row for a pane-frame status segment.
pub(super) fn pane_frame_row_for_geometry(
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
pub(super) enum WindowFramePillboxTarget {
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
pub(super) struct WindowFramePillboxEntry {
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
pub(super) struct WindowFramePillboxSegment {
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
pub(super) fn window_frame_pillbox_entries(
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
pub(super) fn window_frame_pillbox_entries_from_context(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    frame_context
        .windows
        .iter()
        .map(WindowFramePillboxEntry::from)
        .collect()
}

/// Returns default action pill entries for the window status bar.
pub(super) fn window_action_pillbox_entries(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    WindowFrameAction::all()
        .into_iter()
        .map(|action| WindowFramePillboxEntry::action(action, frame_context))
        .collect()
}

/// Returns default pillbox entries for the top window-group bar.
pub(super) fn group_frame_pillbox_entries(
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
pub(super) fn window_frame_pillbox_text(
    window: &Window,
    frame_context: &TerminalFrameContext,
) -> String {
    window_frame_pillbox_text_from_entries(&window_frame_pillbox_entries(window, frame_context))
}

/// Runs the window frame pillbox text from entries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_frame_pillbox_text_from_entries(
    entries: &[WindowFramePillboxEntry],
) -> String {
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
pub(super) fn window_frame_pillbox_segments(
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
pub(super) fn window_frame_field_value(
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
pub(super) fn pane_frame_field_value(
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
            "agent.routing" => optional_pane_context_value(pane_context, |ctx| &ctx.agent_routing)
                .unwrap_or_default(),
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
pub(super) fn optional_pane_context_value(
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
pub(super) fn optional_u32_frame_value(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Runs the sanitize frame text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn sanitize_frame_text(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

/// Runs the write merged pane frames on dividers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_merged_pane_frames_on_dividers(
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
pub(super) fn write_styled_merged_pane_frames_on_dividers(
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
pub(super) fn write_pane_frame_layout_cells(
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

/// Runs the right aligned status bounds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn right_aligned_status_bounds(status: &str, width: usize) -> Option<(usize, usize)> {
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
pub(super) fn write_frame_text_cells(
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
