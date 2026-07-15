//! Frame and status-bar rendering helpers.
//!
//! This module owns window-group frames, window status templates, pane frame
//! text, pane right-status pills, and the hit cells derived from those rendered
//! frame rows. It deliberately keeps frame text and frame hit testing together
//! so mouse targeting uses the same layout calculations as drawing.

use super::*;

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
    let row = compose_frame_pillbox_row::<_, WindowStatusSegmentKind>(&entries, None, width, ' ');
    let style_spans = subtle_frame_fill_span(width, frame_style, ui_theme)
        .into_iter()
        .chain(
            row.pillbox_segments
                .into_iter()
                .map(|segment| TerminalStyleSpan {
                    start: segment.start,
                    length: segment.width,
                    rendition: window_pillbox_rendition(
                        segment.active,
                        segment.subagent,
                        frame_style,
                        ui_theme,
                    ),
                }),
        )
        .collect::<Vec<_>>();
    Some(TerminalStyledLine {
        text: row.text,
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
    let row = compose_frame_pillbox_row(
        &entries,
        frame_context
            .window_status
            .as_ref()
            .filter(|status| !status.template.trim().is_empty() && width > 0)
            .map(|status| render_window_status_template(frame_context, status)),
        width,
        ' ',
    );
    let style_spans = subtle_frame_fill_span(width, frame_style, ui_theme)
        .into_iter()
        .chain(
            row.pillbox_segments
                .into_iter()
                .map(|segment| TerminalStyleSpan {
                    start: segment.start,
                    length: segment.width,
                    rendition: window_pillbox_rendition(
                        segment.active,
                        segment.subagent,
                        frame_style,
                        ui_theme,
                    ),
                }),
        )
        .chain(
            row.right_status_segments
                .iter()
                .map(|segment| TerminalStyleSpan {
                    start: segment.start,
                    length: segment.width,
                    rendition: match &segment.key {
                        WindowStatusSegmentKind::Action { pressed, .. } => {
                            window_pillbox_rendition(
                                *pressed,
                                false,
                                TerminalFrameStyle::Default,
                                ui_theme,
                            )
                        }
                        WindowStatusSegmentKind::Uptime => {
                            ui_theme.colors.window_status_uptime.rendition()
                        }
                        WindowStatusSegmentKind::DateTime => {
                            ui_theme.colors.window_status_datetime.rendition()
                        }
                        WindowStatusSegmentKind::StatusPill => {
                            ui_theme.colors.window_status_uptime.rendition()
                        }
                    },
                }),
        )
        .collect::<Vec<_>>();
    TerminalStyledLine {
        text: row.text,
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
    pane: &mez_mux::layout::Pane,
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
    if segment.key == "agent.status"
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
    match segment.key {
        "history.position" => ui_theme.colors.scroll_indicator.rendition(),
        "pane.pwd" => ui_theme.colors.pane_pwd.rendition(),
        "agent.model" => ui_theme.colors.agent_model.rendition(),
        "agent.reasoning" => ui_theme.colors.agent_reasoning.rendition(),
        "agent.thinking" => pane_frame_agent_thinking_rendition(&segment.value, ui_theme),
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

/// Returns the thinking-mode pill rendition for a pane-local value.
pub(super) fn pane_frame_agent_thinking_rendition(
    value: &str,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    match value {
        "on" => ui_theme.colors.agent_reasoning.rendition(),
        "off" => ui_theme.colors.agent_status_idle.rendition(),
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
        "queued" | "running" | "remembering" | "memorizing" | "thinking" | "routing"
        | "executing" | "waiting" | "compacting" => {
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
        "queued"
            | "running"
            | "remembering"
            | "memorizing"
            | "thinking"
            | "routing"
            | "executing"
            | "waiting"
            | "compacting"
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

/// Runs the pane frame rendition operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_frame_rendition(
    pane: &mez_mux::layout::Pane,
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

/// Runs the render styled pane lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_styled_pane_lines(
    window: &Window,
    pane: &mez_mux::layout::Pane,
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
            prompt_live_footer_suffixes: Vec::new(),
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
    pane: &mez_mux::layout::Pane,
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
            prompt_live_footer_suffixes: Vec::new(),
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
    pane: &mez_mux::layout::Pane,
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
    pane: &mez_mux::layout::Pane,
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
pub(super) type PaneFrameRowLayout = mez_mux::render::PaneFrameRowLayout<&'static str>;

/// Carries Pane Frame Right Status Segment state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) type PaneFrameRightStatusSegment = FrameStatusSegment<&'static str>;

/// Carries Pane Frame Right Value state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) type PaneFrameRightValue = FrameStatusValue<&'static str>;

/// Carries Rendered Pane Frame Right Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) type RenderedPaneFrameRightStatus = RenderedFrameStatus<&'static str>;

/// Runs the pane frame row layout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_frame_row_layout(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
    fill: char,
) -> PaneFrameRowLayout {
    let text = render_pane_frame_template(window, pane, frame_context, template);
    let right_status = pane_frame_right_status(window, pane, frame_context, template);
    compose_pane_frame_row(&text, right_status, width, fill)
}

/// Returns the background fill glyph for a pane frame template.
pub(super) fn pane_frame_fill_char(template: &str) -> char {
    if template == DEFAULT_PANE_FRAME_TEMPLATE {
        '─'
    } else {
        ' '
    }
}

/// Builds the pane-frame right status, appending scrollback position after
/// pane-local agent state.
pub(super) fn pane_frame_right_status(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
) -> Option<RenderedPaneFrameRightStatus> {
    let history_field = "history.position";
    let history_value = pane_frame_field_value(window, pane, frame_context, history_field);
    let mut right_fields = pane_frame_right_aligned_values(window, pane, frame_context, template);
    if !history_value.is_empty() && !template.contains("#{history.position}") {
        right_fields.push(PaneFrameRightValue {
            key: history_field,
            value: history_value.clone(),
            display: history_value,
        });
    }

    (!right_fields.is_empty()).then(|| render_pane_frame_right_status(&right_fields))
}

/// Runs the pane frame right aligned values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_frame_right_aligned_values(
    window: &Window,
    pane: &mez_mux::layout::Pane,
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
                    key: field,
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
    render_frame_status(values)
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
            | "agent.thinking"
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
    if field == "agent.thinking" && !value.trim().is_empty() {
        return "thinking".to_string();
    }
    value.to_string()
}

/// Compacts a home-relative or absolute pane working-directory display path to
/// the last three path segments when the displayed depth exceeds that limit.
pub(super) fn compact_pane_working_directory(value: &str) -> String {
    let mut prefix = "";
    let mut path = value;
    if let Some(rest) = value.strip_prefix("~/") {
        prefix = "~/";
        path = rest;
    } else if value == "~" || value == "/" {
        return value.to_string();
    } else if let Some(rest) = value.strip_prefix('/') {
        prefix = "/";
        path = rest;
    }

    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() <= 3 {
        return format!("{prefix}{path}");
    }
    format!(
        "…/{}",
        segments[segments.len().saturating_sub(3)..].join("/")
    )
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
    compose_frame_text_row(
        &text,
        frame_context
            .window_status
            .as_ref()
            .filter(|status| !status.template.trim().is_empty())
            .map(|status| render_window_status_template(frame_context, status)),
        width,
        ' ',
    )
    .text
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
    /// Represents a configured command-backed status pill.
    StatusPill,
}

impl WindowStatusSegmentKind {
    /// Returns the window action associated with this segment, if any.
    fn action(&self) -> Option<&WindowFrameAction> {
        match self {
            Self::Action { action, .. } => Some(action),
            Self::Uptime | Self::DateTime | Self::StatusPill => None,
        }
    }
}

/// Product semantic key carried through mux-owned frame status placement.
pub(super) type WindowStatusSegment = FrameStatusSegment<WindowStatusSegmentKind>;

/// Product-specialized right-aligned status placement owned by `mez-mux`.
pub(super) type WindowRightStatusLayout = PositionedFrameStatus<WindowStatusSegmentKind>;

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
    position_frame_status(render_window_status_template(frame_context, status), width)
}

/// Product-specialized rendered status retained before mux placement.
pub(super) type RenderedWindowStatusTemplate = RenderedFrameStatus<WindowStatusSegmentKind>;

/// Product-specialized template field retained before mux placement.
pub(super) type WindowStatusFieldComponent = RenderedFrameStatus<WindowStatusSegmentKind>;

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
            key: kind,
            value: text.clone(),
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
                key: WindowStatusSegmentKind::Action {
                    action,
                    pressed: segment.active,
                },
                value: text.clone(),
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
    let entries = vec![window_frame_action_entry(action, frame_context)];
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
                key: WindowStatusSegmentKind::Action {
                    action,
                    pressed: segment.active,
                },
                value: text.clone(),
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
    if let Some(name) = field.strip_prefix("pill.") {
        return (
            status
                .status_pills
                .get(name)
                .map(|value| sanitize_frame_text(value))
                .unwrap_or_default(),
            Some(WindowStatusSegmentKind::StatusPill),
        );
    }
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
            sanitize_frame_text(&compact_pane_working_directory(
                status
                    .active_pane_working_directory
                    .as_deref()
                    .unwrap_or_default(),
            )),
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
            rendition: match &segment.key {
                WindowStatusSegmentKind::Action { pressed, .. } => {
                    window_pillbox_rendition(*pressed, false, TerminalFrameStyle::Default, ui_theme)
                }
                WindowStatusSegmentKind::Uptime => ui_theme.colors.window_status_uptime.rendition(),
                WindowStatusSegmentKind::DateTime => {
                    ui_theme.colors.window_status_datetime.rendition()
                }
                WindowStatusSegmentKind::StatusPill => {
                    ui_theme.colors.window_status_uptime.rendition()
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
    mez_mux::render::frame_pillbox_hit_cells(&window_frame_pillbox_segments(&entries), row, width)
        .into_iter()
        .filter_map(|cell| {
            let WindowFramePillboxTarget::Window(window_index) = cell.target else {
                return None;
            };
            Some(MouseWindowFrameCell {
                column: cell.column,
                row: cell.row,
                window_index,
            })
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
    mez_mux::render::frame_status_hit_cells(&status.segments, row, width)
        .into_iter()
        .filter_map(|cell| {
            let action = cell.target.action().cloned()?;
            Some(MouseWindowActionFrameCell {
                column: cell.column,
                row: cell.row,
                action,
            })
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
            let row = mez_mux::presentation::pane_frame_row_for_geometry(
                geometry, geometries, position, row_offset,
            );
            let fill = if pane_frame_merges_into_divider(geometry, geometries, position) {
                pane_frame_fill_char(template)
            } else {
                ' '
            };
            pane_frame_row_layout(window, pane, frame_context, template, width, fill)
                .right_status_segments
                .into_iter()
                .flat_map(move |segment| {
                    let Some(field) = pane_agent_status_field_from_frame_field(segment.key) else {
                        return Vec::new();
                    };
                    pillbox_segment_local_columns(segment.start, segment.width, width)
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
        "agent.thinking" => Some(PaneAgentStatusField::Thinking),
        "agent.routing" => Some(PaneAgentStatusField::Routing),
        "agent.latency" => Some(PaneAgentStatusField::Latency),
        "agent.preset" => Some(PaneAgentStatusField::Preset),
        "policy.mode" => Some(PaneAgentStatusField::ApprovalPolicy),
        _ => None,
    }
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
    mez_mux::render::frame_pillbox_hit_cells(&window_frame_pillbox_segments(&entries), row, width)
        .into_iter()
        .filter_map(|cell| {
            let WindowFramePillboxTarget::Group(group_index) = cell.target else {
                return None;
            };
            Some(MouseWindowGroupFrameCell {
                column: cell.column,
                row: cell.row,
                group_index,
            })
        })
        .collect()
}

/// Returns clipped local columns occupied by one pillbox segment.
fn pillbox_segment_local_columns(
    start: usize,
    width: usize,
    frame_width: usize,
) -> impl Iterator<Item = usize> {
    frame_pillbox_segment_columns(start, width, frame_width)
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

type WindowFramePillboxEntry = FramePillboxEntry<WindowFramePillboxTarget>;
type WindowFramePillboxSegment = FramePillboxSegment<WindowFramePillboxTarget>;

/// Builds an entry for a built-in window action control.
fn window_frame_action_entry(
    action: WindowFrameAction,
    frame_context: &TerminalFrameContext,
) -> WindowFramePillboxEntry {
    let text = format!(" {} ", action.icon());
    let active = frame_context.pressed_window_action.as_ref() == Some(&action);
    WindowFramePillboxEntry {
        target: WindowFramePillboxTarget::Action(action),
        text,
        active,
        subagent: false,
    }
}

fn window_frame_entry(window: &TerminalWindowFrameContext) -> WindowFramePillboxEntry {
    WindowFramePillboxEntry {
        target: WindowFramePillboxTarget::Window(window.index),
        text: format!(" {} {} ", window.index, sanitize_frame_text(&window.title)),
        active: window.active,
        subagent: window.subagent,
    }
}

fn window_group_frame_entry(group: &TerminalWindowGroupFrameContext) -> WindowFramePillboxEntry {
    WindowFramePillboxEntry {
        target: WindowFramePillboxTarget::Group(group.index),
        text: format!(" {} {} ", group.index, sanitize_frame_text(&group.title)),
        active: group.active,
        subagent: false,
    }
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
        .map(window_frame_entry)
        .collect()
}

/// Builds default window-frame entries directly from runtime frame context.
pub(super) fn window_frame_pillbox_entries_from_context(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    frame_context
        .windows
        .iter()
        .map(window_frame_entry)
        .collect()
}

/// Returns default action pill entries for the window status bar.
pub(super) fn window_action_pillbox_entries(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    WindowFrameAction::all()
        .into_iter()
        .map(|action| window_frame_action_entry(action, frame_context))
        .collect()
}

/// Returns default pillbox entries for the top window-group bar.
pub(super) fn group_frame_pillbox_entries(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    frame_context
        .groups
        .iter()
        .map(window_group_frame_entry)
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
    render_frame_pillbox_text(entries)
}

/// Runs the window frame pillbox segments operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_frame_pillbox_segments(
    entries: &[WindowFramePillboxEntry],
) -> Vec<WindowFramePillboxSegment> {
    render_frame_pillbox_segments(entries)
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
    pane: &mez_mux::layout::Pane,
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
                    .map(|value| compact_pane_working_directory(&value))
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
            "agent.thinking" => {
                optional_pane_context_value(pane_context, |ctx| &ctx.agent_thinking)
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

/// Runs the write merged pane frames on dividers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_merged_pane_frames_on_dividers(
    canvas: &mut [Vec<TerminalRenderCell>],
    geometries: &[PaneGeometry],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
) {
    if !pane_frame.enabled {
        return;
    }
    for placement in
        mez_mux::presentation::merged_pane_frame_placements(geometries, pane_frame.position)
    {
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.index == placement.pane_index)
            .unwrap_or_else(|| window.active_pane());
        let line = canvas.get_mut(placement.row);
        let Some(line) = line else {
            continue;
        };
        write_pane_frame_layout_cells(
            line,
            placement.column_start,
            placement.width,
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
    text_canvas: &mut [Vec<TerminalRenderCell>],
    style_canvas: &mut [Vec<TerminalStyleSpan>],
    geometries: &[PaneGeometry],
    window: &Window,
    frame_context: &TerminalFrameContext,
    pane_frame: TerminalFrameRenderOptions<'_>,
    ui_theme: &UiTheme,
) {
    if !pane_frame.enabled {
        return;
    }
    for placement in
        mez_mux::presentation::merged_pane_frame_placements(geometries, pane_frame.position)
    {
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.index == placement.pane_index)
            .unwrap_or_else(|| window.active_pane());
        let line = text_canvas.get_mut(placement.row);
        let Some(line) = line else {
            continue;
        };
        let layout = write_pane_frame_layout_cells(
            line,
            placement.column_start,
            placement.width,
            window,
            pane,
            frame_context,
            pane_frame.template,
        );
        if let Some(spans) = style_canvas.get_mut(placement.row) {
            if layout.left_text_width > 0 {
                spans.push(TerminalStyleSpan {
                    start: placement.column_start,
                    length: layout.left_text_width,
                    rendition: pane_frame_rendition(pane, pane_frame.style, ui_theme),
                });
            }
            spans.extend(pane_frame_right_status_style_spans(
                &layout,
                placement.column_start,
                frame_context,
                ui_theme,
            ));
            spans.extend(merged_pane_frame_boundary_style_spans(
                geometries,
                u16::try_from(placement.row).unwrap_or(u16::MAX),
                placement.column_start,
                placement.width,
                ui_theme,
            ));
        }
    }
}

/// Writes a pane frame into a divider row as a complete status-bar region.
pub(super) fn write_pane_frame_layout_cells(
    row: &mut [TerminalRenderCell],
    column_start: usize,
    max_columns: usize,
    window: &Window,
    pane: &mez_mux::layout::Pane,
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
