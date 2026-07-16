//! Window ownership for terminal frame rendering.

use super::super::*;
use super::*;

/// Runs the render window frame template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn render_window_frame_template(
    window: &Window,
    frame_context: &TerminalFrameContext,
    template: &str,
) -> String {
    mez_mux::render::render_frame_template(template, |field| {
        window_frame_field_value(window, frame_context, field)
    })
}

/// Runs the render window frame text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn render_window_frame_text(
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
pub(in crate::terminal::render) enum WindowStatusSegmentKind {
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
pub(in crate::terminal::render) type WindowStatusSegment =
    FrameStatusSegment<WindowStatusSegmentKind>;

/// Product-specialized right-aligned status placement owned by `mez-mux`.
pub(in crate::terminal::render) type WindowRightStatusLayout =
    PositionedFrameStatus<WindowStatusSegmentKind>;

/// Runs the window right status layout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn window_right_status_layout(
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
pub(in crate::terminal::render) type RenderedWindowStatusTemplate =
    RenderedFrameStatus<WindowStatusSegmentKind>;

/// Product-specialized template field retained before mux placement.
pub(in crate::terminal::render) type WindowStatusFieldComponent =
    RenderedFrameStatus<WindowStatusSegmentKind>;

/// Runs the render window status template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn render_window_status_template(
    frame_context: &TerminalFrameContext,
    status: &TerminalWindowStatusContext,
) -> RenderedWindowStatusTemplate {
    mez_mux::render::render_frame_status_template(&status.template, |field| {
        window_status_field_component(frame_context, status, field)
    })
}

/// Expands one window status template field into text and relative segments.
pub(in crate::terminal::render) fn window_status_field_component(
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
pub(in crate::terminal::render) fn window_actions_status_component(
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
pub(in crate::terminal::render) fn window_action_status_component(
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
pub(in crate::terminal::render) fn window_status_template_button_action(
    field: &str,
) -> Option<WindowFrameAction> {
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
pub(in crate::terminal::render) fn window_status_field_value(
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
pub(in crate::terminal::render) fn window_status_style_spans(
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
            let width =
                usize::from(pane_render_region_size_for_geometry(geometry, geometries).columns);
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
