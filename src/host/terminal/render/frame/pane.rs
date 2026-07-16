//! Pane ownership for terminal frame rendering.

use super::super::{
    AgentPromptBlock, DEFAULT_PANE_FRAME_RIGHT_ALIGNED, DEFAULT_PANE_FRAME_TEMPLATE,
    FrameStatusSegment, FrameStatusValue, RenderedFrameStatus, TerminalFrameContext,
    TerminalFramePosition, TerminalFrameRenderOptions, TerminalPaneFrameContext,
    TerminalStyledLine, UiTheme, Window, compose_pane_frame_row, fit_styled_width, fit_width,
    overlay_agent_display_lines, render_agent_prompt_block, render_frame_status,
};
use super::{pane_frame_field_value, styled_pane_frame_line};
use mez_mux::render::PaneFrameRowLayout;

/// Runs the render styled pane lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::host::terminal::render) fn render_styled_pane_lines(
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
pub(in crate::host::terminal::render) fn render_pane_lines(
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
pub(in crate::host::terminal::render) fn render_pane_frame_template(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
) -> String {
    mez_mux::render::render_frame_template(template, |field| {
        pane_frame_field_value(window, pane, frame_context, field)
    })
}

/// Runs the render pane frame text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::host::terminal::render) fn render_pane_frame_text(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
) -> String {
    pane_frame_row_layout(window, pane, frame_context, template, width, ' ').text
}

/// Carries Pane Frame Right Status Segment state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(in crate::host::terminal::render) type PaneFrameRightStatusSegment =
    FrameStatusSegment<&'static str>;

/// Carries Pane Frame Right Value state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(in crate::host::terminal::render) type PaneFrameRightValue = FrameStatusValue<&'static str>;

/// Carries Rendered Pane Frame Right Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(in crate::host::terminal::render) type RenderedPaneFrameRightStatus =
    RenderedFrameStatus<&'static str>;

/// Runs the pane frame row layout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::host::terminal::render) fn pane_frame_row_layout(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
    fill: char,
) -> PaneFrameRowLayout<&'static str> {
    let text = render_pane_frame_template(window, pane, frame_context, template);
    let right_status = pane_frame_right_status(window, pane, frame_context, template);
    compose_pane_frame_row(&text, right_status, width, fill)
}

/// Returns the background fill glyph for a pane frame template.
pub(in crate::host::terminal::render) fn pane_frame_fill_char(template: &str) -> char {
    if template == DEFAULT_PANE_FRAME_TEMPLATE {
        '─'
    } else {
        ' '
    }
}

/// Builds the pane-frame right status, appending scrollback position after
/// pane-local agent state.
pub(in crate::host::terminal::render) fn pane_frame_right_status(
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
pub(in crate::host::terminal::render) fn pane_frame_right_aligned_values(
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
pub(in crate::host::terminal::render) fn render_pane_frame_right_status(
    values: &[PaneFrameRightValue],
) -> RenderedPaneFrameRightStatus {
    render_frame_status(values)
}

/// Runs the pane agent shell visible operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::host::terminal::render) fn pane_agent_shell_visible(
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
pub(in crate::host::terminal::render) fn pane_agent_prompt_space_reserved(
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
pub(in crate::host::terminal::render) fn pane_agent_prompt_transparent(
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
pub(in crate::host::terminal::render) fn pane_frame_right_aligned_display_value(
    field: &str,
    value: String,
) -> String {
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
pub(in crate::host::terminal::render) fn pane_frame_right_aligned_segment_value(
    field: &str,
    value: &str,
) -> String {
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
pub(in crate::host::terminal::render) fn compact_pane_working_directory(value: &str) -> String {
    mez_mux::render::compact_display_path(value, 3)
}
