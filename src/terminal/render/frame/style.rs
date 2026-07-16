//! Style ownership for terminal frame rendering.

use super::super::{
    AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS, DEFAULT_WINDOW_FRAME_TEMPLATE, GraphicRendition,
    TerminalFrameContext, TerminalFrameStyle, TerminalStyleSpan, TerminalStyledLine, UiColorPair,
    UiTheme, Window, agent_status_running_gradient_palette, animated_scan_background,
    blend_terminal_color, compose_frame_pillbox_row, contrasting_binary_foreground, fit_width,
    frame_style_rendition, gradient_highlight_for_offset, group_frame_visible,
    neutral_surface_step, push_or_extend_style_span, styled_frame_line_with_rendition,
};
use super::{
    PaneFrameRightStatusSegment, WindowStatusSegmentKind, group_frame_pillbox_entries,
    pane_frame_row_layout, render_window_frame_text, render_window_status_template,
    window_frame_pillbox_entries, window_frame_pillbox_text_from_entries,
    window_right_status_layout, window_status_style_spans,
};
use mez_mux::render::PaneFrameRowLayout;

/// Renders the unstyled top group bar when more than one group exists.
pub(in crate::terminal::render) fn group_frame_text(
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
pub(in crate::terminal::render) fn styled_group_frame_line(
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
pub(in crate::terminal::render) fn styled_window_frame_line(
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
pub(in crate::terminal::render) fn styled_window_pillbox_line(
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
pub(in crate::terminal::render) fn window_pillbox_rendition(
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
pub(in crate::terminal::render) fn styled_pane_frame_line(
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
pub(in crate::terminal::render) fn pane_frame_right_status_style_spans(
    layout: &PaneFrameRowLayout<&'static str>,
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
pub(in crate::terminal::render) fn pane_frame_right_status_segment_style_spans(
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
pub(in crate::terminal::render) fn pane_frame_right_status_rendition(
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
pub(in crate::terminal::render) fn pane_frame_agent_thinking_rendition(
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
pub(in crate::terminal::render) fn pane_frame_agent_routing_rendition(
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
pub(in crate::terminal::render) fn pane_frame_latency_rendition(
    value: &str,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    match value {
        "slow" => ui_theme.colors.agent_status_idle.rendition(),
        "fast" => ui_theme.colors.agent_status_running.rendition(),
        _ => ui_theme.colors.agent_model.rendition(),
    }
}

/// Returns the approval-policy pill rendition for a pane-local value.
pub(in crate::terminal::render) fn pane_frame_policy_mode_rendition(
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
pub(in crate::terminal::render) fn pane_frame_agent_context_usage_rendition(
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
pub(in crate::terminal::render) fn pane_frame_agent_status_rendition(
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
pub(in crate::terminal::render) fn pane_frame_agent_status_is_active(status: &str) -> bool {
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
pub(in crate::terminal::render) const AGENT_STATUS_SCAN_BAND_WIDTH: usize = 9;

/// Builds the animated scan background for an active agent status pill.
pub(in crate::terminal::render) fn pane_frame_agent_status_scan_spans(
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
pub(in crate::terminal::render) fn subtle_frame_fill_span(
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
pub(in crate::terminal::render) fn pane_frame_rendition(
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
pub(in crate::terminal::render) fn pane_border_rendition(
    active: bool,
    ui_theme: &UiTheme,
) -> GraphicRendition {
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
pub(in crate::terminal::render) fn themed_frame_rendition(
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
