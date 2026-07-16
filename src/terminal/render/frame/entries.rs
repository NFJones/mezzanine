//! Entries ownership for terminal frame rendering.

use super::super::{
    FramePillboxEntry, FramePillboxSegment, MouseWindowGroupFrameCell, PaneAgentStatusField,
    TerminalFrameContext, TerminalWindowFrameContext, TerminalWindowGroupFrameContext,
    WindowFrameAction, frame_pillbox_segment_columns, group_frame_visible,
    render_frame_pillbox_segments, render_frame_pillbox_text, sanitize_frame_text,
};
use mez_mux::layout::Window;

/// Maps an internal pane-frame field name to a clickable selector field.
pub(in crate::terminal::render) fn pane_agent_status_field_from_frame_field(
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
pub(in crate::terminal::render) fn pillbox_segment_local_columns(
    start: usize,
    width: usize,
    frame_width: usize,
) -> impl Iterator<Item = usize> {
    frame_pillbox_segment_columns(start, width, frame_width)
}

/// Carries the target represented by a window-frame pillbox segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::terminal::render) enum WindowFramePillboxTarget {
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
pub(in crate::terminal::render) fn window_frame_action_entry(
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
pub(in crate::terminal::render) fn window_frame_pillbox_entries(
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
pub(in crate::terminal::render) fn window_frame_pillbox_entries_from_context(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    frame_context
        .windows
        .iter()
        .map(window_frame_entry)
        .collect()
}

/// Returns default action pill entries for the window status bar.
pub(in crate::terminal::render) fn window_action_pillbox_entries(
    frame_context: &TerminalFrameContext,
) -> Vec<WindowFramePillboxEntry> {
    WindowFrameAction::all()
        .into_iter()
        .map(|action| window_frame_action_entry(action, frame_context))
        .collect()
}

/// Returns default pillbox entries for the top window-group bar.
pub(in crate::terminal::render) fn group_frame_pillbox_entries(
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
pub(in crate::terminal::render) fn window_frame_pillbox_text(
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
pub(in crate::terminal::render) fn window_frame_pillbox_text_from_entries(
    entries: &[WindowFramePillboxEntry],
) -> String {
    render_frame_pillbox_text(entries)
}

/// Runs the window frame pillbox segments operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn window_frame_pillbox_segments(
    entries: &[WindowFramePillboxEntry],
) -> Vec<WindowFramePillboxSegment> {
    render_frame_pillbox_segments(entries)
}

/// Runs the window frame field value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::terminal::render) fn window_frame_field_value(
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
