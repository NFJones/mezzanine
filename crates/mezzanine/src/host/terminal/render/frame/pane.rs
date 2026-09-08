//! Pane ownership for terminal frame rendering.

#[cfg(test)]
use super::super::fit_width;
use super::super::{
    AgentPromptBlock, DEFAULT_PANE_FRAME_TEMPLATE, FrameStatusSegment, RenderedFrameStatus,
    TerminalFrameContext, TerminalFramePosition, TerminalFrameRenderOptions,
    TerminalPaneFrameContext, TerminalStyledLine, UiTheme, Window, fit_styled_width,
    fitted_text_width, overlay_agent_display_lines, render_agent_prompt_block, sanitize_frame_text,
};
use super::{pane_frame_field_value, styled_pane_frame_line};
use crate::host::terminal::{
    PaneStatusCondition, PaneStatusField, PaneStatusFormat, PaneStatusOccurrenceId,
    PaneStatusPillDefinition, PaneStatusRail, PaneStatusSegmentIdentity,
};
use mez_mux::render::{
    PaneFrameRowLayout, PaneStatusLayoutItem, PaneStatusLayoutOptions, PaneStatusLayoutState,
    compose_pane_status_layout, line_slice, render_frame_pill_text,
};

/// Secret-safe stable identity for one configured pane-status occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneStatusDiagnosticIdentity {
    /// Stable pane that owns the occurrence.
    pub(crate) owner_pane_id: mez_core::ids::PaneId,
    /// Stable rail and template ordinal.
    pub(crate) occurrence: PaneStatusOccurrenceId,
    /// Product-owned field identity.
    pub(crate) field: PaneStatusField,
    /// Finite action owner; custom command and argument payloads are excluded.
    pub(crate) action_owner: String,
    /// Effective configuration generation used by semantic actions.
    pub(crate) config_generation: u64,
    /// Relevant pane-context generation used by semantic actions.
    pub(crate) context_generation: u64,
}

/// Diagnostic projection for one configured status occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneStatusDiagnosticOccurrence {
    /// Bounded configured marker identity.
    pub(crate) source: String,
    /// Stable owner, occurrence, field, and action-owner identity.
    pub(crate) identity: PaneStatusDiagnosticIdentity,
    /// Resolution result before width fitting.
    pub(crate) availability: &'static str,
    /// Authoritative shared layout result when the occurrence was available.
    pub(crate) layout_state: Option<&'static str>,
    /// Full padded display-cell requirement.
    pub(crate) full_cells: usize,
    /// Compact padded display-cell requirement.
    pub(crate) compact_cells: usize,
    /// Cells selected by the authoritative shared layout.
    pub(crate) selected_cells: usize,
}

/// Complete read-only projection of one pane's resolved status layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneStatusDiagnosticProjection {
    /// Stable pane owner.
    pub(crate) owner_pane_id: mez_core::ids::PaneId,
    /// Exact pane-frame row width supplied to the shared resolver.
    pub(crate) pane_width_cells: usize,
    /// Configured minimum title budget.
    pub(crate) title_min_width_cells: usize,
    /// Cells retained by the rendered title.
    pub(crate) title_used_cells: usize,
    /// Maximum status-cell budget after title reservation and row padding.
    pub(crate) status_budget_cells: usize,
    /// Cells selected across visible, compact, and overflow-indicator items.
    pub(crate) status_used_cells: usize,
    /// Every configured occurrence in stable left-then-right template order.
    pub(crate) occurrences: Vec<PaneStatusDiagnosticOccurrence>,
}

#[derive(Debug, Clone)]
struct PaneStatusRailResolution {
    rendered: RenderedPaneFrameRightStatus,
    occurrences: Vec<PaneStatusDiagnosticOccurrence>,
}

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
    while lines.len() < content_start.saturating_add(content_rows) {
        lines.push(TerminalStyledLine::plain(" ".repeat(width)));
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
    while lines.len() < content_start.saturating_add(content_rows) {
        lines.push(" ".repeat(width));
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
pub(crate) fn pane_frame_row_layout(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
    fill: char,
) -> PaneFrameRowLayout<PaneStatusSegmentIdentity> {
    pane_frame_row_layout_with_diagnostics(window, pane, frame_context, template, width, fill).0
}

/// Projects the same resolved conditions and whole-pill layout used by rendering.
pub(crate) fn pane_frame_status_diagnostic_projection(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
    fill: char,
) -> PaneStatusDiagnosticProjection {
    let title_min_width_cells = frame_context.pane_status.title_min_width;
    let (layout, mut occurrences) =
        pane_frame_row_layout_with_diagnostics(window, pane, frame_context, template, width, fill);
    for item in &layout.status_items {
        if item.key.occurrence.ordinal == u16::MAX {
            continue;
        }
        if let Some(occurrence) = occurrences.iter_mut().find(|occurrence| {
            occurrence.identity.occurrence == item.key.occurrence
                && occurrence.identity.owner_pane_id == item.key.owner_pane_id
        }) {
            occurrence.layout_state = Some(pane_status_layout_state_name(item.state));
            occurrence.selected_cells = match item.state {
                PaneStatusLayoutState::Visible | PaneStatusLayoutState::Compacted => {
                    fitted_text_width(&item.display, usize::MAX)
                }
                PaneStatusLayoutState::Hidden | PaneStatusLayoutState::Overflowed => 0,
            };
        }
    }
    let selected_rail_width = |rail: PaneStatusRail| {
        let mut bounds = layout
            .right_status_segments
            .iter()
            .filter(|segment| segment.key.occurrence.rail == rail)
            .map(|segment| (segment.start, segment.start.saturating_add(segment.width)));
        let Some((first_start, first_end)) = bounds.next() else {
            return 0;
        };
        let (start, end) = bounds.fold(
            (first_start, first_end),
            |(start, end), (next_start, next_end)| (start.min(next_start), end.max(next_end)),
        );
        end.saturating_sub(start)
    };
    let left_used_cells = selected_rail_width(PaneStatusRail::Left);
    let right_used_cells = selected_rail_width(PaneStatusRail::Right);
    let status_used_cells = left_used_cells
        .saturating_add(right_used_cells)
        .saturating_add(usize::from(left_used_cells > 0 && right_used_cells > 0));
    let available_before_title = width.saturating_sub(usize::from(width > 0));
    let title_budget = title_min_width_cells.min(available_before_title);
    let status_budget_cells = available_before_title
        .saturating_sub(title_budget)
        .saturating_sub(usize::from(title_budget > 0));
    PaneStatusDiagnosticProjection {
        owner_pane_id: pane.id.clone(),
        pane_width_cells: width,
        title_min_width_cells,
        title_used_cells: layout.left_text_width,
        status_budget_cells,
        status_used_cells,
        occurrences,
    }
}

fn pane_frame_row_layout_with_diagnostics(
    window: &Window,
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    template: &str,
    width: usize,
    fill: char,
) -> (
    PaneFrameRowLayout<PaneStatusSegmentIdentity>,
    Vec<PaneStatusDiagnosticOccurrence>,
) {
    let title = render_pane_frame_template(window, pane, frame_context, template);
    let left_status = pane_frame_status_rail(window, pane, frame_context, PaneStatusRail::Left);
    let right_status = pane_frame_status_rail(window, pane, frame_context, PaneStatusRail::Right);
    let mut occurrences = left_status.occurrences;
    occurrences.extend(right_status.occurrences);
    let left_items = pane_status_layout_items(left_status.rendered, PaneStatusRail::Left);
    let right_items = pane_status_layout_items(right_status.rendered, PaneStatusRail::Right);
    let config_generation = frame_context.pane_status.generation();
    let overflow_identity = PaneStatusSegmentIdentity {
        owner_pane_id: pane.id.clone(),
        occurrence: PaneStatusOccurrenceId {
            rail: PaneStatusRail::Right,
            ordinal: u16::MAX,
        },
        field: PaneStatusField::PaneStatus,
        style: crate::host::terminal::PaneStatusStyle::Automatic,
        color_overrides: crate::host::terminal::FramePillColorOverrides::default(),
        action: crate::host::terminal::PaneStatusAction::OpenSettings,
        compact_display: "…".to_string(),
        min_width: None,
        max_width: None,
        priority: 100,
        config_generation,
        context_generation: pane_status_context_generation(
            pane,
            frame_context,
            PaneStatusField::PaneStatus,
            "overflow",
        ),
    };
    let layout = compose_pane_status_layout(
        &title,
        left_items,
        right_items,
        width,
        fill,
        PaneStatusLayoutOptions {
            title_min_width: frame_context.pane_status.title_min_width,
            overflow_policy: frame_context.pane_status.overflow,
            overflow_indicator: Some(PaneStatusLayoutItem {
                key: overflow_identity,
                value: String::new(),
                display: " … ".to_string(),
                compact_display: " … ".to_string(),
                separator: " ".to_string(),
                priority: 100,
                min_width: None,
            }),
        },
    );
    (layout, occurrences)
}

fn pane_status_layout_state_name(state: PaneStatusLayoutState) -> &'static str {
    match state {
        PaneStatusLayoutState::Visible => "full",
        PaneStatusLayoutState::Compacted => "compact",
        PaneStatusLayoutState::Hidden => "hidden",
        PaneStatusLayoutState::Overflowed => "overflow",
    }
}

/// Converts resolved semantic rail segments into whole layout candidates.
fn pane_status_layout_items(
    status: RenderedPaneFrameRightStatus,
    _rail: PaneStatusRail,
) -> Vec<PaneStatusLayoutItem<PaneStatusSegmentIdentity>> {
    let mut previous_end = 0usize;
    status
        .segments
        .into_iter()
        .map(|segment| {
            let separator = line_slice(&status.text, previous_end, segment.start);
            let end = segment.start.saturating_add(segment.width);
            let display = line_slice(&status.text, segment.start, end);
            previous_end = end;
            let compact_display = render_frame_pill_text(&segment.key.compact_display);
            PaneStatusLayoutItem {
                key: segment.key.clone(),
                value: segment.value,
                display,
                compact_display,
                separator,
                priority: segment.key.priority,
                min_width: segment.key.min_width,
            }
        })
        .collect()
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
) -> PaneStatusRailResolution {
    let status_config = &frame_context.pane_status;
    let template = match rail {
        PaneStatusRail::Left => &status_config.left_status,
        PaneStatusRail::Right => &status_config.right_status,
    };
    let config_generation = status_config.generation();
    let mut text = String::new();
    let mut segments = Vec::new();
    let mut occurrences = Vec::new();
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
        occurrences.push(component.diagnostic);
        let component = component.rendered;
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
    PaneStatusRailResolution {
        rendered: RenderedFrameStatus {
            text: sanitize_frame_text(&text),
            segments,
        },
        occurrences,
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
) -> PaneStatusComponentResolution {
    let definition = marker
        .strip_prefix("pill.")
        .and_then(|name| frame_context.pane_status.pills.get(name).cloned())
        .or_else(|| PaneStatusField::parse(marker).map(PaneStatusPillDefinition::builtin));
    let Some(definition) = definition else {
        return unavailable_pane_status_component(
            pane,
            frame_context,
            marker,
            occurrence,
            config_generation,
        );
    };
    let field_name = definition.field.as_str();
    let provider_name = marker.strip_prefix("pill.");
    let display_value = if definition.field == PaneStatusField::Provider {
        provider_name
            .and_then(|name| {
                frame_context
                    .panes
                    .get(pane.id.as_str())?
                    .status_pills
                    .get(name)
            })
            .cloned()
            .unwrap_or_default()
    } else {
        pane_frame_field_value(window, pane, frame_context, field_name)
    };
    let raw_value = if definition.field == PaneStatusField::PaneStatus {
        frame_context
            .panes
            .get(pane.id.as_str())
            .and_then(|context| context.pane_status_state.clone())
            .unwrap_or_else(|| display_value.clone())
    } else {
        display_value.clone()
    };
    let provider_label_only = definition.field == PaneStatusField::Provider
        && definition
            .label
            .as_deref()
            .is_some_and(|label| !label.trim().is_empty());
    let context_generation =
        pane_status_context_generation(pane, frame_context, definition.field, &raw_value);
    let identity = pane_status_diagnostic_identity(
        pane,
        occurrence,
        &definition,
        config_generation,
        context_generation,
    );
    if display_value.is_empty() && !provider_label_only {
        return PaneStatusComponentResolution {
            rendered: empty_rendered_pane_status(),
            diagnostic: PaneStatusDiagnosticOccurrence {
                source: pane_status_diagnostic_source(marker),
                identity,
                availability: "unavailable",
                layout_state: None,
                full_cells: 0,
                compact_cells: 0,
                selected_cells: 0,
            },
        };
    }
    if !pane_status_conditions_match(pane, frame_context, &definition, &display_value) {
        return PaneStatusComponentResolution {
            rendered: empty_rendered_pane_status(),
            diagnostic: PaneStatusDiagnosticOccurrence {
                source: pane_status_diagnostic_source(marker),
                identity,
                availability: "condition-hidden",
                layout_state: None,
                full_cells: 0,
                compact_cells: 0,
                selected_cells: 0,
            },
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
        return PaneStatusComponentResolution {
            rendered: RenderedFrameStatus {
                text,
                segments: Vec::new(),
            },
            diagnostic: PaneStatusDiagnosticOccurrence {
                source: pane_status_diagnostic_source(marker),
                identity,
                availability: "unavailable",
                layout_state: None,
                full_cells: 0,
                compact_cells: 0,
                selected_cells: 0,
            },
        };
    }
    let width = fitted_text_width(&text, usize::MAX);
    let compact_text = mez_mux::render::render_frame_pill_text(&compact_display);
    let segment_identity = PaneStatusSegmentIdentity {
        owner_pane_id: pane.id.clone(),
        occurrence,
        field: definition.field,
        style: definition.style,
        color_overrides: definition.color_overrides,
        action: definition.action,
        compact_display,
        min_width: definition.min_width,
        max_width: definition.max_width,
        priority: definition.priority,
        config_generation,
        context_generation,
    };
    PaneStatusComponentResolution {
        rendered: RenderedFrameStatus {
            text,
            segments: vec![FrameStatusSegment {
                start: 0,
                width,
                key: segment_identity,
                value: raw_value,
            }],
        },
        diagnostic: PaneStatusDiagnosticOccurrence {
            source: pane_status_diagnostic_source(marker),
            identity,
            availability: "available",
            layout_state: None,
            full_cells: width,
            compact_cells: fitted_text_width(&compact_text, usize::MAX),
            selected_cells: 0,
        },
    }
}

struct PaneStatusComponentResolution {
    rendered: RenderedPaneFrameRightStatus,
    diagnostic: PaneStatusDiagnosticOccurrence,
}

fn empty_rendered_pane_status() -> RenderedPaneFrameRightStatus {
    RenderedFrameStatus {
        text: String::new(),
        segments: Vec::new(),
    }
}

fn unavailable_pane_status_component(
    pane: &mez_mux::layout::Pane,
    frame_context: &TerminalFrameContext,
    marker: &str,
    occurrence: PaneStatusOccurrenceId,
    config_generation: u64,
) -> PaneStatusComponentResolution {
    let field = PaneStatusField::PaneStatus;
    let context_generation = pane_status_context_generation(pane, frame_context, field, "");
    let definition = PaneStatusPillDefinition::builtin(field);
    PaneStatusComponentResolution {
        rendered: empty_rendered_pane_status(),
        diagnostic: PaneStatusDiagnosticOccurrence {
            source: pane_status_diagnostic_source(marker),
            identity: pane_status_diagnostic_identity(
                pane,
                occurrence,
                &definition,
                config_generation,
                context_generation,
            ),
            availability: "unavailable",
            layout_state: None,
            full_cells: 0,
            compact_cells: 0,
            selected_cells: 0,
        },
    }
}

fn pane_status_diagnostic_identity(
    pane: &mez_mux::layout::Pane,
    occurrence: PaneStatusOccurrenceId,
    definition: &PaneStatusPillDefinition,
    config_generation: u64,
    context_generation: u64,
) -> PaneStatusDiagnosticIdentity {
    let action_owner = match &definition.action {
        crate::host::terminal::PaneStatusAction::None => "none".to_string(),
        crate::host::terminal::PaneStatusAction::Builtin(field) => {
            format!("builtin:{}", pane_agent_status_field_name(*field))
        }
        crate::host::terminal::PaneStatusAction::OpenSettings => "settings".to_string(),
        crate::host::terminal::PaneStatusAction::Terminal { .. } => "terminal".to_string(),
        crate::host::terminal::PaneStatusAction::Agent { .. } => "agent".to_string(),
    };
    PaneStatusDiagnosticIdentity {
        owner_pane_id: pane.id.clone(),
        occurrence,
        field: definition.field,
        action_owner,
        config_generation,
        context_generation,
    }
}

fn pane_agent_status_field_name(
    field: crate::host::terminal::PaneAgentStatusField,
) -> &'static str {
    match field {
        crate::host::terminal::PaneAgentStatusField::Model => "model",
        crate::host::terminal::PaneAgentStatusField::Reasoning => "reasoning",
        crate::host::terminal::PaneAgentStatusField::Thinking => "thinking",
        crate::host::terminal::PaneAgentStatusField::Planning => "planning",
        crate::host::terminal::PaneAgentStatusField::Routing => "routing",
        crate::host::terminal::PaneAgentStatusField::ApprovalPolicy => "approval-policy",
        crate::host::terminal::PaneAgentStatusField::Latency => "latency",
        crate::host::terminal::PaneAgentStatusField::Preset => "preset",
        crate::host::terminal::PaneAgentStatusField::Settings => "settings",
    }
}

fn pane_status_diagnostic_source(marker: &str) -> String {
    marker
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
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
