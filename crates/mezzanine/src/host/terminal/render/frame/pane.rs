//! Pane ownership for terminal frame rendering.

#[cfg(test)]
use super::super::fit_width;
use super::super::{
    AgentPromptBlock, DEFAULT_PANE_FRAME_TEMPLATE, FrameStatusSegment, RenderedFrameStatus,
    TerminalFrameContext, TerminalFramePosition, TerminalFrameRenderOptions,
    TerminalPaneFrameContext, TerminalStyledLine, UiTheme, Window, compose_pane_frame_status_row,
    fit_styled_width, fitted_text_width, overlay_agent_display_lines, render_agent_prompt_block,
    sanitize_frame_text,
};
use super::{pane_frame_field_value, styled_pane_frame_line};
use crate::host::terminal::{
    PaneStatusCondition, PaneStatusField, PaneStatusFormat, PaneStatusOccurrenceId,
    PaneStatusPillDefinition, PaneStatusRail, PaneStatusSegmentIdentity,
};
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
#[cfg(test)]
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
#[cfg(test)]
pub(in crate::host::terminal::render) fn render_pane_frame_text(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
) -> String {
    pane_frame_row_layout(window, pane, frame_context, template, width, ' ').text
}

/// Semantic segment shared by pane-status rendering, styling, and hit testing.
pub(in crate::host::terminal::render) type PaneFrameRightStatusSegment =
    FrameStatusSegment<PaneStatusSegmentIdentity>;

/// Rendered semantic pane-status rail.
pub(in crate::host::terminal::render) type RenderedPaneFrameRightStatus =
    RenderedFrameStatus<PaneStatusSegmentIdentity>;

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
) -> PaneFrameRowLayout<PaneStatusSegmentIdentity> {
    let title = render_pane_frame_template(window, pane, frame_context, template);
    let left_status = pane_frame_status_rail(window, pane, frame_context, PaneStatusRail::Left);
    let right_status = pane_frame_status_rail(window, pane, frame_context, PaneStatusRail::Right);
    compose_pane_frame_status_row(
        &title,
        left_status,
        (!right_status.text.is_empty()).then_some(right_status),
        width,
        fill,
    )
}

/// Returns the background fill glyph for a pane frame template.
pub(in crate::host::terminal::render) fn pane_frame_fill_char(template: &str) -> char {
    if template == DEFAULT_PANE_FRAME_TEMPLATE {
        '─'
    } else {
        ' '
    }
}

/// Resolves one configured pane-status rail into typed semantic segments.
fn pane_frame_status_rail(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    rail: PaneStatusRail,
) -> RenderedPaneFrameRightStatus {
    let status_config = &frame_context.pane_status;
    let template = match rail {
        PaneStatusRail::Left => &status_config.left_status,
        PaneStatusRail::Right => &status_config.right_status,
    };
    let config_generation = status_config.generation();
    let mut text = String::new();
    let mut segments = Vec::new();
    let mut remaining = template.as_str();
    let mut ordinal = 0u16;
    while let Some(start) = remaining.find("#{") {
        let literal = &remaining[..start];
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find('}') else {
            break;
        };
        let marker = &after_start[..end];
        let component = resolve_pane_status_component(
            window,
            pane,
            frame_context,
            marker,
            PaneStatusOccurrenceId { rail, ordinal },
            config_generation,
        );
        ordinal = ordinal.saturating_add(1);
        if !component.text.is_empty() {
            append_pane_status_literal(&mut text, literal);
            let component_start = fitted_text_width(&text, usize::MAX);
            text.push_str(&component.text);
            segments.extend(component.segments.into_iter().map(|mut segment| {
                segment.start = component_start.saturating_add(segment.start);
                segment
            }));
        } else if !literal.trim().is_empty() {
            append_pane_status_literal(&mut text, literal);
        }
        remaining = &after_start[end + 1..];
    }
    if !remaining.trim().is_empty() {
        append_pane_status_literal(&mut text, remaining);
    }
    RenderedFrameStatus {
        text: sanitize_frame_text(&text),
        segments,
    }
}

/// Appends template literal text without retaining orphan whitespace between
/// unavailable status fields.
fn append_pane_status_literal(text: &mut String, literal: &str) {
    if literal.trim().is_empty() {
        if !text.is_empty() {
            text.push_str(literal);
        }
    } else {
        text.push_str(literal);
    }
}

/// Resolves one bare or named built-in marker to its padded display segment.
fn resolve_pane_status_component(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    marker: &str,
    occurrence: PaneStatusOccurrenceId,
    config_generation: u64,
) -> RenderedPaneFrameRightStatus {
    let definition = marker
        .strip_prefix("pill.")
        .and_then(|name| frame_context.pane_status.pills.get(name).cloned())
        .or_else(|| PaneStatusField::parse(marker).map(PaneStatusPillDefinition::builtin));
    let Some(definition) = definition else {
        return RenderedFrameStatus {
            text: String::new(),
            segments: Vec::new(),
        };
    };
    let field_name = definition.field.as_str();
    let display_value = pane_frame_field_value(window, pane, frame_context, field_name);
    let raw_value = if definition.field == PaneStatusField::PaneStatus {
        frame_context
            .panes
            .get(pane.id.as_str())
            .and_then(|context| context.pane_status_state.clone())
            .unwrap_or_else(|| display_value.clone())
    } else {
        display_value.clone()
    };
    if display_value.is_empty()
        || !pane_status_conditions_match(pane, frame_context, &definition, &display_value)
    {
        return RenderedFrameStatus {
            text: String::new(),
            segments: Vec::new(),
        };
    }
    let mut display =
        pane_status_display_value(definition.field, definition.format, &display_value);
    let mut compact_display =
        pane_status_display_value(definition.field, definition.compact_format, &display_value);
    if let Some(label) = definition
        .label
        .as_deref()
        .filter(|label| !label.trim().is_empty())
    {
        display = format!("{} {display}", label.trim());
        compact_display = format!("{} {compact_display}", label.trim());
    }
    if let Some(max_width) = definition.max_width {
        display = mez_mux::render::line_slice(&display, 0, max_width);
        compact_display = mez_mux::render::line_slice(&compact_display, 0, max_width);
    }
    let text = mez_mux::render::render_frame_pill_text(&display);
    if text.is_empty() {
        return RenderedFrameStatus {
            text,
            segments: Vec::new(),
        };
    }
    let context_generation =
        pane_status_context_generation(pane, frame_context, definition.field, &raw_value);
    let width = fitted_text_width(&text, usize::MAX);
    RenderedFrameStatus {
        text,
        segments: vec![FrameStatusSegment {
            start: 0,
            width,
            key: PaneStatusSegmentIdentity {
                owner_pane_id: pane.id.clone(),
                occurrence,
                field: definition.field,
                style: definition.style,
                action: definition.action,
                compact_display,
                min_width: definition.min_width,
                max_width: definition.max_width,
                priority: definition.priority,
                config_generation,
                context_generation,
            },
            value: raw_value,
        }],
    }
}

/// Evaluates the finite AND-combined condition vocabulary for one pane.
fn pane_status_conditions_match(
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    definition: &PaneStatusPillDefinition,
    display_value: &str,
) -> bool {
    let pane_context = frame_context.panes.get(pane.id.as_str());
    let agent_view = pane_context.and_then(|context| context.mode.as_deref()) == Some("agent");
    let scrollback = pane_context
        .and_then(|context| context.history_position.as_deref())
        .is_some_and(|value| !value.trim().is_empty());
    let busy = pane_context
        .and_then(|context| context.agent_status.as_deref())
        .is_some_and(|value| {
            matches!(
                value,
                "queued"
                    | "running"
                    | "thinking"
                    | "executing"
                    | "waiting"
                    | "bootstrapping"
                    | "certifying_sandbox"
                    | "compacting"
                    | "memorizing"
            )
        });
    definition.when.iter().all(|condition| match condition {
        PaneStatusCondition::AgentView => agent_view,
        PaneStatusCondition::ShellView => !agent_view,
        PaneStatusCondition::Focused => pane.active,
        PaneStatusCondition::Unfocused => !pane.active,
        PaneStatusCondition::Busy => busy,
        PaneStatusCondition::Idle => !busy,
        PaneStatusCondition::Supported | PaneStatusCondition::Nonempty => {
            !display_value.trim().is_empty()
        }
        PaneStatusCondition::Scrollback => scrollback,
    })
}

/// Formats one built-in value without evaluating arbitrary user expressions.
fn pane_status_display_value(
    field: PaneStatusField,
    format: PaneStatusFormat,
    value: &str,
) -> String {
    let value = value.trim();
    match format {
        PaneStatusFormat::Full => value.to_string(),
        PaneStatusFormat::Percent => {
            if value.ends_with('%') {
                value.to_string()
            } else {
                format!("{value}%")
            }
        }
        PaneStatusFormat::Short => match field {
            PaneStatusField::AgentName if value == "manager" => String::new(),
            PaneStatusField::AgentRouting => "route".to_string(),
            PaneStatusField::AgentThinking => "thinking".to_string(),
            PaneStatusField::AgentPlanning => "plan".to_string(),
            _ => value.to_string(),
        },
    }
}

/// Hashes relevant pane-local state for stale semantic-action detection.
fn pane_status_context_generation(
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    field: PaneStatusField,
    value: &str,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    pane.id.hash(&mut hasher);
    pane.active.hash(&mut hasher);
    field.hash(&mut hasher);
    value.hash(&mut hasher);
    if let Some(context) = frame_context.panes.get(pane.id.as_str()) {
        context.mode.hash(&mut hasher);
        context.agent_status.hash(&mut hasher);
        context.history_position.hash(&mut hasher);
    }
    hasher.finish()
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

/// Compacts a home-relative or absolute pane working-directory display path to
/// the last three path segments when the displayed depth exceeds that limit.
pub(in crate::host::terminal::render) fn compact_pane_working_directory(value: &str) -> String {
    mez_mux::render::compact_display_path(value, 3)
}
