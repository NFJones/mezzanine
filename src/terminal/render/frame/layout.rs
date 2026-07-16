//! Layout ownership for terminal frame rendering.

use super::super::*;
use super::*;
use mez_mux::render::PaneFrameRowLayout;

/// Runs the pane frame field value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn pane_frame_field_value(
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
pub(in crate::terminal::render) fn optional_pane_context_value(
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
pub(in crate::terminal::render) fn optional_u32_frame_value(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Runs the write merged pane frames on dividers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn write_merged_pane_frames_on_dividers(
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
pub(in crate::terminal::render) fn write_styled_merged_pane_frames_on_dividers(
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
pub(in crate::terminal::render) fn write_pane_frame_layout_cells(
    row: &mut [TerminalRenderCell],
    column_start: usize,
    max_columns: usize,
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
) -> PaneFrameRowLayout<&'static str> {
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
