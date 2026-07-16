//! Runtime render mouse geometry helpers.
//!
//! This module owns pane resize drag geometry for the runtime render layer.
//! Higher-level runtime methods still decide when mouse input is active; this
//! module converts pane geometry and drag state into concrete resize updates.

use super::*;
use crate::runtime::{MIN_PANE_COLUMNS, MIN_PANE_ROWS, MouseResizeDragState, PaneGeometry};
use mez_mux::layout::range_overlap_u16;

/// Carries the pane geometry update produced by one mouse resize drag step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MouseResizeDragUpdate {
    /// Updated pane geometries after applying the drag position.
    pub(super) geometries: Vec<PaneGeometry>,
}

/// Returns the geometry update for one resize drag state and mouse position.
///
/// # Parameters
/// - `state`: Active mouse resize drag state.
/// - `column`: Mouse column in terminal coordinates.
/// - `row`: Mouse row in terminal coordinates.
pub(super) fn mouse_resize_update_from_state(
    state: MouseResizeDragState,
    column: u16,
    row: u16,
) -> MouseResizeDragUpdate {
    match state {
        MouseResizeDragState::Vertical {
            min_column,
            max_column,
            left_indices,
            right_indices,
            geometries,
            ..
        } => MouseResizeDragUpdate {
            geometries: resize_vertical_border_geometries(
                geometries,
                column.clamp(min_column, max_column),
                &left_indices,
                &right_indices,
            ),
        },
        MouseResizeDragState::Horizontal {
            min_row,
            max_row,
            row_offset,
            top_indices,
            bottom_indices,
            geometries,
            ..
        } => {
            let body_row = row.saturating_sub(row_offset).clamp(min_row, max_row);
            MouseResizeDragUpdate {
                geometries: resize_horizontal_border_geometries(
                    geometries,
                    body_row,
                    &top_indices,
                    &bottom_indices,
                ),
            }
        }
    }
}

/// Resizes geometries around a vertical border.
///
/// # Parameters
/// - `geometries`: Pane geometries captured when the drag began.
/// - `border_column`: New divider column.
/// - `left_indices`: Pane indices on the left side of the divider.
/// - `right_indices`: Pane indices on the right side of the divider.
fn resize_vertical_border_geometries(
    mut geometries: Vec<PaneGeometry>,
    border_column: u16,
    left_indices: &[usize],
    right_indices: &[usize],
) -> Vec<PaneGeometry> {
    let right_column = border_column.saturating_add(1);
    for geometry in &mut geometries {
        if left_indices.contains(&geometry.index) {
            geometry.columns = right_column.saturating_sub(geometry.column);
        } else if right_indices.contains(&geometry.index) {
            let old_right = geometry.column.saturating_add(geometry.columns);
            geometry.column = right_column;
            geometry.columns = old_right.saturating_sub(right_column);
        }
    }
    geometries
}

/// Resizes geometries around a horizontal border.
///
/// # Parameters
/// - `geometries`: Pane geometries captured when the drag began.
/// - `border_row`: New divider row.
/// - `top_indices`: Pane indices above the divider.
/// - `bottom_indices`: Pane indices below the divider.
fn resize_horizontal_border_geometries(
    mut geometries: Vec<PaneGeometry>,
    border_row: u16,
    top_indices: &[usize],
    bottom_indices: &[usize],
) -> Vec<PaneGeometry> {
    let bottom_row = border_row.saturating_add(1);
    for geometry in &mut geometries {
        if top_indices.contains(&geometry.index) {
            geometry.rows = bottom_row.saturating_sub(geometry.row);
        } else if bottom_indices.contains(&geometry.index) {
            let old_bottom = geometry.row.saturating_add(geometry.rows);
            geometry.row = bottom_row;
            geometry.rows = old_bottom.saturating_sub(bottom_row);
        }
    }
    geometries
}

/// Builds resize drag state for a vertical divider under the mouse.
///
/// # Parameters
/// - `geometries`: Rendered pane geometries.
/// - `border_column`: Divider column in window-body coordinates.
/// - `row`: Mouse row in window-body coordinates.
pub(super) fn vertical_mouse_resize_state(
    geometries: &[PaneGeometry],
    border_column: u16,
    row: u16,
) -> Option<MouseResizeDragState> {
    let border_right = border_column.checked_add(1)?;
    let (left_indices, right_indices) = vertical_border_component(geometries, border_right, row)?;
    let min_column = left_indices
        .iter()
        .filter_map(|index| geometries.iter().find(|geometry| geometry.index == *index))
        .map(|geometry| {
            geometry
                .column
                .saturating_add(MIN_PANE_COLUMNS)
                .saturating_sub(1)
        })
        .max()?;
    let max_column = right_indices
        .iter()
        .filter_map(|index| geometries.iter().find(|geometry| geometry.index == *index))
        .map(|geometry| {
            geometry
                .column
                .saturating_add(geometry.columns)
                .saturating_sub(MIN_PANE_COLUMNS)
                .saturating_sub(1)
        })
        .min()?;
    if min_column > max_column {
        return None;
    }
    Some(MouseResizeDragState::Vertical {
        min_column,
        max_column,
        left_indices,
        right_indices,
        geometries: geometries.to_vec(),
    })
}

/// Builds resize drag state for a horizontal divider under the mouse.
///
/// # Parameters
/// - `geometries`: Rendered pane geometries.
/// - `border_row`: Divider row in window-body coordinates.
/// - `column`: Mouse column in window-body coordinates.
/// - `row_offset`: Terminal row offset from group/window frame rows.
pub(super) fn horizontal_mouse_resize_state(
    geometries: &[PaneGeometry],
    border_row: u16,
    column: u16,
    row_offset: u16,
) -> Option<MouseResizeDragState> {
    let border_bottom = border_row.checked_add(1)?;
    let (top_indices, bottom_indices) =
        horizontal_border_component(geometries, border_bottom, column)?;
    let min_row = top_indices
        .iter()
        .filter_map(|index| geometries.iter().find(|geometry| geometry.index == *index))
        .map(|geometry| geometry.row.saturating_add(MIN_PANE_ROWS).saturating_sub(1))
        .max()?;
    let max_row = bottom_indices
        .iter()
        .filter_map(|index| geometries.iter().find(|geometry| geometry.index == *index))
        .map(|geometry| {
            geometry
                .row
                .saturating_add(geometry.rows)
                .saturating_sub(MIN_PANE_ROWS)
                .saturating_sub(1)
        })
        .min()?;
    if min_row > max_row {
        return None;
    }
    Some(MouseResizeDragState::Horizontal {
        min_row,
        max_row,
        row_offset,
        top_indices,
        bottom_indices,
        geometries: geometries.to_vec(),
    })
}

/// Returns the connected panes on each side of one vertical resize border.
///
/// # Parameters
/// - `geometries`: Rendered pane geometries.
/// - `border_right`: First column to the right of the divider.
/// - `row`: Row used to seed connected-component discovery.
fn vertical_border_component(
    geometries: &[PaneGeometry],
    border_right: u16,
    row: u16,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let mut start = row;
    let mut end = row.saturating_add(1);
    let mut left_indices = Vec::new();
    let mut right_indices = Vec::new();
    loop {
        let previous = (start, end, left_indices.len(), right_indices.len());
        for geometry in geometries {
            let geometry_start = geometry.row;
            let geometry_end = geometry.row.saturating_add(geometry.rows);
            if range_overlap_u16(start, end, geometry_start, geometry_end) == 0 {
                continue;
            }
            if geometry.column.saturating_add(geometry.columns) == border_right {
                push_unique(&mut left_indices, geometry.index);
                start = start.min(geometry_start);
                end = end.max(geometry_end);
            } else if geometry.column == border_right {
                push_unique(&mut right_indices, geometry.index);
                start = start.min(geometry_start);
                end = end.max(geometry_end);
            }
        }
        if previous == (start, end, left_indices.len(), right_indices.len()) {
            break;
        }
    }
    (!left_indices.is_empty() && !right_indices.is_empty()).then_some((left_indices, right_indices))
}

/// Returns the connected panes on each side of one horizontal resize border.
///
/// # Parameters
/// - `geometries`: Rendered pane geometries.
/// - `border_bottom`: First row below the divider.
/// - `column`: Column used to seed connected-component discovery.
fn horizontal_border_component(
    geometries: &[PaneGeometry],
    border_bottom: u16,
    column: u16,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let mut start = column;
    let mut end = column.saturating_add(1);
    let mut top_indices = Vec::new();
    let mut bottom_indices = Vec::new();
    loop {
        let previous = (start, end, top_indices.len(), bottom_indices.len());
        for geometry in geometries {
            let geometry_start = geometry.column;
            let geometry_end = geometry.column.saturating_add(geometry.columns);
            if range_overlap_u16(start, end, geometry_start, geometry_end) == 0 {
                continue;
            }
            if geometry.row.saturating_add(geometry.rows) == border_bottom {
                push_unique(&mut top_indices, geometry.index);
                start = start.min(geometry_start);
                end = end.max(geometry_end);
            } else if geometry.row == border_bottom {
                push_unique(&mut bottom_indices, geometry.index);
                start = start.min(geometry_start);
                end = end.max(geometry_end);
            }
        }
        if previous == (start, end, top_indices.len(), bottom_indices.len()) {
            break;
        }
    }
    (!top_indices.is_empty() && !bottom_indices.is_empty()).then_some((top_indices, bottom_indices))
}

/// Pushes a value if it is not already present.
///
/// # Parameters
/// - `values`: The ordered collection to mutate.
/// - `value`: The value to insert if absent.
fn push_unique(values: &mut Vec<usize>, value: usize) {
    if !values.contains(&value) {
        values.push(value);
    }
}

impl RuntimeSessionService {
    pub(super) fn apply_attached_mouse_action(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        action: MouseAction,
        queue_for_adapter: bool,
    ) -> Result<bool> {
        match action {
            MouseAction::Ignore => Ok(true),
            MouseAction::ForwardToPane => Ok(false),
            MouseAction::FocusWindow { index } => {
                self.session
                    .select_window(primary_client_id, &index.to_string())?;
                Ok(true)
            }
            MouseAction::FocusGroup { index } => {
                let effects = self
                    .session
                    .select_group_transition(primary_client_id, &index.to_string())?;
                self.sync_pane_resize_effects(&effects)?;
                Ok(true)
            }
            MouseAction::PressWindowAction { action } => {
                self.presentation.pressed_window_action = Some(action);
                Ok(true)
            }
            MouseAction::ReleaseWindowAction { action } => {
                let should_run = self.presentation.pressed_window_action.as_ref() == Some(&action);
                self.presentation.pressed_window_action = None;
                if should_run {
                    self.apply_window_frame_action(primary_client_id, action)?;
                }
                Ok(true)
            }
            MouseAction::CancelWindowAction => {
                self.presentation.pressed_window_action = None;
                Ok(true)
            }
            MouseAction::OpenPaneAgentStatusSelector { pane_index, field } => {
                self.open_pane_agent_status_selector(primary_client_id, pane_index, field)?;
                Ok(true)
            }
            MouseAction::HoverPaneAgentStatusSelector {
                pane_index,
                field,
                item_index,
            } => {
                self.hover_pane_agent_status_selector(pane_index, field, item_index);
                Ok(true)
            }
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index,
                field,
                item_index,
            } => {
                self.select_pane_agent_status_selector(
                    primary_client_id,
                    pane_index,
                    field,
                    item_index,
                )?;
                Ok(true)
            }
            MouseAction::ScrollPaneAgentStatusSelector {
                pane_index,
                field,
                lines,
            } => {
                self.scroll_pane_agent_status_selector(pane_index, field, lines);
                Ok(true)
            }
            MouseAction::ClosePaneAgentStatusSelector => {
                self.presentation.pane_agent_status_selector = None;
                Ok(true)
            }
            MouseAction::BeginDisplayOverlaySelection { .. }
            | MouseAction::UpdateDisplayOverlaySelection { .. }
            | MouseAction::FinishDisplayOverlaySelection { .. } => Ok(false),
            MouseAction::SelectDisplayOverlay { .. } | MouseAction::ScrollDisplayOverlay { .. } => {
                Ok(false)
            }
            MouseAction::ShowWindowChooser { .. } => {
                self.execute_attached_display_command(primary_client_id, "choose-window")?;
                Ok(true)
            }
            MouseAction::FocusPane(position) => {
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                let pane_id = target.pane_id.clone();
                self.session
                    .select_pane_global(primary_client_id, pane_id.as_str())?;
                if self.execute_agent_command_link_at_pane_position(
                    primary_client_id,
                    pane_id.as_str(),
                    target.position,
                )? {
                    self.presentation.mouse_selection_drag_state = None;
                    self.presentation.last_mouse_click_state = None;
                    return Ok(true);
                }
                let now = current_unix_millis();
                if self
                    .presentation
                    .last_mouse_click_state
                    .as_ref()
                    .is_some_and(|click| {
                        click.pane_id == pane_id
                            && click.position == target.position
                            && now.saturating_sub(click.clicked_at_unix_ms)
                                <= DOUBLE_CLICK_WORD_SELECTION_WINDOW_MS
                    })
                {
                    self.presentation.mouse_selection_drag_state = None;
                    self.presentation.last_mouse_click_state = None;
                    self.copy_word_at_pane_position(
                        primary_client_id,
                        pane_id.as_str(),
                        target.position,
                    )?;
                    return Ok(true);
                }
                self.presentation.last_mouse_click_state = Some(RuntimeMouseClickState {
                    pane_id: pane_id.clone(),
                    position: target.position,
                    clicked_at_unix_ms: now,
                });
                self.presentation.mouse_selection_drag_state = Some(MouseSelectionDragState {
                    pane_id,
                    position: target.position,
                    origin_position: position,
                    autoscroll_position: None,
                });
                Ok(true)
            }
            MouseAction::CopyWord(position) => {
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                self.copy_word_at_pane_position(
                    primary_client_id,
                    target.pane_id.as_str(),
                    target.position,
                )
            }
            MouseAction::FocusPaneOnly(position) => {
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                self.session
                    .select_pane_global(primary_client_id, target.pane_id.as_str())?;
                self.presentation.mouse_selection_drag_state = None;
                Ok(true)
            }
            MouseAction::PasteClipboard(position) => {
                self.presentation.mouse_selection_drag_state = None;
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                self.session
                    .select_pane_global(primary_client_id, target.pane_id.as_str())?;
                let Some(descriptor) = self.find_pane_descriptor(target.pane_id.as_str()) else {
                    return Ok(true);
                };
                match self.paste_clipboard_or_most_recent_buffer_to_text_entry_or_pane(
                    primary_client_id,
                    &descriptor,
                    queue_for_adapter,
                ) {
                    Ok(_) => Ok(true),
                    Err(err) if err.kind() == crate::error::MezErrorKind::NotFound => Ok(true),
                    Err(err) => Err(err),
                }
            }
            MouseAction::ResizePane { column, row } => {
                self.presentation.mouse_selection_drag_state = None;
                let Some(update) = self.mouse_resize_drag_update(column, row)? else {
                    let pane_id = self.active_pane_id()?;
                    let size = Size {
                        columns: column.saturating_add(1).max(MIN_PANE_COLUMNS),
                        rows: row.saturating_add(1).max(MIN_PANE_ROWS),
                    };
                    self.resize_pane_pty(primary_client_id, Some(pane_id.as_str()), size)?;
                    return Ok(true);
                };
                let effects = self
                    .session
                    .replace_active_window_pane_geometries_transition(
                        primary_client_id,
                        update.geometries,
                    )?;
                self.sync_pane_resize_effects(&effects)?;
                Ok(true)
            }
            MouseAction::FinishResizePane => {
                self.presentation.mouse_resize_drag_state = None;
                Ok(true)
            }
            MouseAction::ScrollHistory { lines, position } => {
                self.presentation.mouse_selection_drag_state = None;
                let target = self
                    .mouse_pane_target_at(position)
                    .unwrap_or(MousePaneTarget {
                        pane_id: self.active_pane_id()?.to_string(),
                        position,
                    });
                let should_exit = {
                    let copy_mode = self.ensure_active_copy_mode(target.pane_id.as_str())?;
                    copy_mode.scroll_by(lines);
                    lines > 0 && copy_mode.is_at_bottom() && copy_mode.selection().is_none()
                };
                if should_exit {
                    self.active_copy_modes.remove(target.pane_id.as_str());
                    self.scrollback_copy_mode_panes
                        .remove(target.pane_id.as_str());
                } else {
                    self.scrollback_copy_mode_panes
                        .insert(target.pane_id.clone());
                }
                Ok(true)
            }
            MouseAction::CopySelectionStart(position) => {
                let target = self.mouse_selection_target_at(position)?;
                self.session
                    .select_pane_global(primary_client_id, target.pane_id.as_str())?;
                let pane_id = target.pane_id;
                self.presentation.mouse_selection_drag_state = Some(MouseSelectionDragState {
                    pane_id: pane_id.clone(),
                    position: target.position,
                    origin_position: position,
                    autoscroll_position: None,
                });
                let copy_mode = self.ensure_mouse_selection_copy_mode(pane_id.as_str())?;
                let position = runtime_copy_position_for_view(copy_mode, target.position);
                copy_mode.select_range(position, position)?;
                Ok(true)
            }
            MouseAction::CopySelectionUpdate(position) => {
                self.apply_mouse_selection_update(primary_client_id, position, false)
            }
            MouseAction::CopySelectionFinish(position) => {
                self.apply_mouse_selection_update(primary_client_id, position, true)
            }
        }
    }

    /// Executes an agent command link embedded in visible pane output.
    ///
    /// # Parameters
    /// - `primary_client_id`: The primary client selecting the link.
    /// - `pane_id`: The pane whose visible output was clicked.
    /// - `position`: The pane-local cell position that was clicked.
    fn execute_agent_command_link_at_pane_position(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(command) = self.agent_command_link_at_pane_position(pane_id, position) else {
            return Ok(false);
        };
        let body = match self.execute_agent_shell_command(primary_client_id, &command) {
            Ok(body) => body,
            Err(error) => {
                self.set_agent_prompt_display_lines(
                    pane_id,
                    agent_prompt_error_display_lines(&error),
                )?;
                return Ok(true);
            }
        };
        match runtime_agent_shell_display_output(&body, &self.ui_theme) {
            Ok(display_output) => self.set_agent_prompt_display_output(pane_id, display_output)?,
            Err(error) => {
                self.set_agent_prompt_display_lines(
                    pane_id,
                    agent_prompt_error_display_lines(&error),
                )?;
            }
        }
        if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
            self.agent_prompt_inputs.remove(pane_id);
        }
        Ok(true)
    }

    /// Returns the agent command link at one visible pane position.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose visible line should be inspected.
    /// - `position`: The pane-local cell position to test.
    fn agent_command_link_at_pane_position(
        &self,
        pane_id: &str,
        position: CopyPosition,
    ) -> Option<String> {
        let screen = self.pane_screens.get(pane_id)?;
        let line = screen.visible_lines().get(position.line)?.to_string();
        agent_command_link_at_line_column(line.as_str(), position.column)
    }

    /// Runs a command-backed window status-bar action selected by mouse release.
    fn apply_window_frame_action(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        action: WindowFrameAction,
    ) -> Result<()> {
        let command = action.command().to_string();
        match action.command_kind() {
            WindowFrameCommandKind::Terminal => {
                self.execute_terminal_command(primary_client_id, &command)?;
            }
            WindowFrameCommandKind::Agent => {
                let pane_id = self.active_pane_id()?;
                self.enter_agent_mode_for_pane(&pane_id)?;
                self.execute_agent_shell_command(primary_client_id, &command)?;
            }
        }
        Ok(())
    }

    /// Applies keyboard navigation to the open pane-frame selector.
    pub(super) fn apply_pane_agent_status_selector_terminal_action(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        action: &TerminalClientLoopAction,
    ) -> Result<bool> {
        let TerminalClientLoopAction::ForwardToPane(input) = action else {
            return Ok(false);
        };
        match runtime_selector_input_action(input) {
            RuntimeSelectorInputAction::Exit => {
                self.presentation.pane_agent_status_selector = None;
                Ok(true)
            }
            RuntimeSelectorInputAction::Previous => {
                self.move_pane_agent_status_selector(-1);
                Ok(true)
            }
            RuntimeSelectorInputAction::Next => {
                self.move_pane_agent_status_selector(1);
                Ok(true)
            }
            RuntimeSelectorInputAction::First => {
                self.set_pane_agent_status_selector_index(0);
                Ok(true)
            }
            RuntimeSelectorInputAction::Last => {
                if let Some(selector) = self.presentation.pane_agent_status_selector.as_ref() {
                    self.set_pane_agent_status_selector_index(
                        selector.items.len().saturating_sub(1),
                    );
                }
                Ok(true)
            }
            RuntimeSelectorInputAction::Select => {
                let Some(selector) = self.presentation.pane_agent_status_selector.as_ref() else {
                    return Ok(false);
                };
                self.select_pane_agent_status_selector(
                    primary_client_id,
                    selector.pane_index,
                    selector.field,
                    selector.active_index,
                )?;
                Ok(true)
            }
            RuntimeSelectorInputAction::Ignore => Ok(false),
        }
    }

    /// Moves the open pane-frame selector highlight by one row.
    fn move_pane_agent_status_selector(&mut self, delta: isize) {
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        let Some(selector) = self.presentation.pane_agent_status_selector.as_mut() else {
            return;
        };
        selector.active_index =
            runtime_selector_step_index(selector.active_index, selector.items.len(), delta);
        runtime_pane_agent_status_selector_keep_active_visible(selector, visible_rows);
    }

    /// Sets the open pane-frame selector highlight to a bounded item index.
    fn set_pane_agent_status_selector_index(&mut self, item_index: usize) {
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        let Some(selector) = self.presentation.pane_agent_status_selector.as_mut() else {
            return;
        };
        selector.active_index = item_index.min(selector.items.len().saturating_sub(1));
        runtime_pane_agent_status_selector_keep_active_visible(selector, visible_rows);
    }

    /// Scrolls the open pane-frame selector without changing pane scrollback.
    fn scroll_pane_agent_status_selector(
        &mut self,
        pane_index: usize,
        field: PaneAgentStatusField,
        lines: isize,
    ) {
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        let Some(selector) = self.presentation.pane_agent_status_selector.as_mut() else {
            return;
        };
        if selector.pane_index != pane_index || selector.field != field {
            return;
        }
        let max_offset = selector.items.len().saturating_sub(visible_rows.max(1));
        if lines.is_negative() {
            selector.scroll_offset = selector.scroll_offset.saturating_sub(lines.unsigned_abs());
        } else {
            selector.scroll_offset = selector
                .scroll_offset
                .saturating_add(lines as usize)
                .min(max_offset);
        }
    }

    /// Returns the current selector's visible row count for the active window.
    fn pane_agent_status_selector_visible_rows(&self) -> usize {
        let Some(selector) = self.presentation.pane_agent_status_selector.as_ref() else {
            return 1;
        };
        let Some(size) = self.session.active_window().map(|window| window.size) else {
            return 1;
        };
        runtime_pane_agent_status_selector_layout(selector, size)
            .visible_items
            .len()
            .max(1)
    }

    /// Opens or applies the pane-frame selector for a pane.
    fn open_pane_agent_status_selector(
        &mut self,
        _primary_client_id: &mez_core::ids::ClientId,
        pane_index: usize,
        field: PaneAgentStatusField,
    ) -> Result<()> {
        let Some(window) = self.session.active_window() else {
            self.presentation.pane_agent_status_selector = None;
            return Ok(());
        };
        let Some(pane) = window.panes().iter().find(|pane| pane.index == pane_index) else {
            self.presentation.pane_agent_status_selector = None;
            return Ok(());
        };
        let pane_id = pane.id.to_string();
        if field == PaneAgentStatusField::Routing {
            self.presentation.pane_agent_status_selector = None;
            let outcome = self.execute_agent_shell_routing_command(&pane_id, "/routing toggle")?;
            let response =
                runtime_agent_shell_command_response_json(&pane_id, "/routing", Some(&outcome));
            if let Ok(display_output) =
                runtime_agent_shell_display_output(&response, &self.ui_theme)
            {
                self.set_agent_prompt_display_output(&pane_id, display_output)?;
            }
            return Ok(());
        }
        if field == PaneAgentStatusField::Thinking {
            self.presentation.pane_agent_status_selector = None;
            let outcome =
                self.execute_agent_shell_thinking_command(&pane_id, "/thinking toggle")?;
            let response =
                runtime_agent_shell_command_response_json(&pane_id, "/thinking", Some(&outcome));
            if let Ok(display_output) =
                runtime_agent_shell_display_output(&response, &self.ui_theme)
            {
                self.set_agent_prompt_display_output(&pane_id, display_output)?;
            }
            return Ok(());
        }
        let frame_context = self.terminal_frame_context();
        let cells = self.active_window_mouse_pane_agent_status_cells(&frame_context);
        let field_cells = cells
            .iter()
            .filter(|cell| cell.pane_index == pane_index && cell.field == field)
            .copied()
            .collect::<Vec<_>>();
        let Some(anchor_column) = field_cells.iter().map(|cell| cell.column).min() else {
            self.presentation.pane_agent_status_selector = None;
            return Ok(());
        };
        let anchor_row = field_cells.iter().map(|cell| cell.row).min().unwrap_or(0);
        let anchor_width = field_cells
            .iter()
            .map(|cell| cell.column)
            .max()
            .and_then(|max_column| max_column.checked_sub(anchor_column))
            .map(|width| width.saturating_add(1))
            .unwrap_or(1);
        let items = match field {
            PaneAgentStatusField::Model | PaneAgentStatusField::Preset => {
                self.configured_model_names_for_pane(&pane_id)?
            }
            PaneAgentStatusField::Reasoning => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, active_profile) =
                    self.active_model_profile_for_pane(&pane_id, &agent_id, None)?;
                self.configured_reasoning_levels_for_pane_model(&pane_id, &active_profile.model)?
            }
            PaneAgentStatusField::Thinking => Vec::new(),
            PaneAgentStatusField::ApprovalPolicy => {
                vec![
                    "ask".to_string(),
                    "auto-allow".to_string(),
                    "full-access".to_string(),
                ]
            }
            PaneAgentStatusField::Latency => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, active_profile) =
                    self.active_model_profile_for_pane(&pane_id, &agent_id, None)?;
                if self.model_profile_supports_latency_preference(&active_profile) {
                    vec![
                        "slow".to_string(),
                        "default".to_string(),
                        "fast".to_string(),
                    ]
                } else {
                    Vec::new()
                }
            }
            PaneAgentStatusField::Routing => Vec::new(),
        };
        if items.is_empty() {
            self.presentation.pane_agent_status_selector = None;
            return Ok(());
        }
        let active_value = self.active_pane_agent_status_selector_value(&pane_id, field);
        let active_index = active_value
            .as_deref()
            .and_then(|value| items.iter().position(|item| item == value))
            .unwrap_or(0);
        self.presentation.pane_agent_status_selector = Some(RuntimePaneAgentStatusSelector {
            pane_id,
            pane_index,
            field,
            items,
            active_index,
            scroll_offset: active_index,
            anchor_column,
            anchor_row,
            anchor_width,
        });
        let visible_rows = self.pane_agent_status_selector_visible_rows();
        if let Some(selector) = self.presentation.pane_agent_status_selector.as_mut() {
            runtime_pane_agent_status_selector_keep_active_visible(selector, visible_rows);
        }
        Ok(())
    }

    /// Updates the highlighted item for the open pane-frame selector.
    fn hover_pane_agent_status_selector(
        &mut self,
        pane_index: usize,
        field: PaneAgentStatusField,
        item_index: usize,
    ) {
        let Some(selector) = self.presentation.pane_agent_status_selector.as_mut() else {
            return;
        };
        if selector.pane_index == pane_index && selector.field == field {
            selector.active_index = item_index.min(selector.items.len().saturating_sub(1));
        }
    }

    /// Applies the selected pane-frame model or reasoning value.
    fn select_pane_agent_status_selector(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_index: usize,
        field: PaneAgentStatusField,
        item_index: usize,
    ) -> Result<()> {
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let Some(selector) = self.presentation.pane_agent_status_selector.take() else {
            return Ok(());
        };
        if selector.pane_index != pane_index || selector.field != field {
            return Ok(());
        }
        let Some(value) = selector.items.get(item_index).cloned() else {
            return Ok(());
        };
        let outcome = match field {
            PaneAgentStatusField::Model | PaneAgentStatusField::Preset => {
                self.apply_pane_model_picker_selection(&selector.pane_id, &value)?
            }
            PaneAgentStatusField::Reasoning => {
                self.apply_pane_reasoning_picker_selection(&selector.pane_id, &value)?
            }
            PaneAgentStatusField::Thinking => return Ok(()),
            PaneAgentStatusField::ApprovalPolicy => {
                let outcome = self.execute_agent_shell_approval_command(
                    &selector.pane_id,
                    &format!("/approval {value}"),
                )?;
                let response = runtime_agent_shell_command_response_json(
                    &selector.pane_id,
                    "/approval",
                    Some(&outcome),
                );
                if let Ok(display_output) =
                    runtime_agent_shell_display_output(&response, &self.ui_theme)
                {
                    self.set_agent_prompt_display_output(&selector.pane_id, display_output)?;
                }
                return Ok(());
            }
            PaneAgentStatusField::Routing => return Ok(()),
            PaneAgentStatusField::Latency => {
                self.apply_pane_latency_picker_selection(&selector.pane_id, &value)?
            }
        };
        let response = runtime_agent_shell_command_response_json(
            &selector.pane_id,
            match field {
                PaneAgentStatusField::Model => "/model",
                PaneAgentStatusField::Reasoning => "/model reasoning",
                PaneAgentStatusField::Thinking => "/thinking",
                PaneAgentStatusField::Routing => "/routing",
                PaneAgentStatusField::ApprovalPolicy => "/approval",
                PaneAgentStatusField::Latency => "/latency",
                PaneAgentStatusField::Preset => "/model",
            },
            Some(&outcome),
        );
        if let Ok(display_output) = runtime_agent_shell_display_output(&response, &self.ui_theme) {
            self.set_agent_prompt_display_output(&selector.pane_id, display_output)?;
        }
        Ok(())
    }

    /// Returns the active pane-frame value represented by a selector field.
    fn active_pane_agent_status_selector_value(
        &self,
        pane_id: &str,
        field: PaneAgentStatusField,
    ) -> Option<String> {
        match field {
            PaneAgentStatusField::Model
            | PaneAgentStatusField::Reasoning
            | PaneAgentStatusField::Thinking => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, profile) = self
                    .active_model_profile_for_pane(pane_id, &agent_id, None)
                    .ok()?;
                match field {
                    PaneAgentStatusField::Model => {
                        Some(format!("{}: {}", profile.provider, profile.model))
                    }
                    PaneAgentStatusField::Reasoning => profile.reasoning_display_value(),
                    PaneAgentStatusField::Thinking => self
                        .model_profile_thinking_enabled(&profile)
                        .map(|enabled| if enabled { "on" } else { "off" }.to_string()),
                    _ => None,
                }
            }
            PaneAgentStatusField::Routing => Some(
                if self
                    .agent_routing_overrides
                    .get(pane_id)
                    .copied()
                    .unwrap_or(self.agent_routing)
                {
                    "auto:on"
                } else {
                    "auto:off"
                }
                .to_string(),
            ),
            PaneAgentStatusField::ApprovalPolicy => Some(
                runtime_approval_policy_name(self.permission_policy.approval_policy).to_string(),
            ),
            PaneAgentStatusField::Latency => {
                let agent_id = format!("agent-{pane_id}");
                let (_active_name, profile) = self
                    .active_model_profile_for_pane(pane_id, &agent_id, None)
                    .ok()?;
                if !self.model_profile_supports_latency_preference(&profile) {
                    return None;
                }
                profile
                    .latency_preference
                    .or_else(|| Some("default".to_string()))
            }
            PaneAgentStatusField::Preset => self
                .active_model_preset_name_for_pane(pane_id)
                .map(|preset| format!("preset: {preset}")),
        }
    }

    /// Runs the apply mouse selection update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_mouse_selection_update(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        position: CopyPosition,
        finish: bool,
    ) -> Result<bool> {
        let target = self.mouse_selection_target_at(position)?;
        self.session
            .select_pane_global(primary_client_id, target.pane_id.as_str())?;
        let pane_id = target.pane_id;
        let anchor = self
            .presentation
            .mouse_selection_drag_state
            .as_ref()
            .filter(|state| state.pane_id == pane_id)
            .map(|state| state.position)
            .unwrap_or(target.position);
        let origin = self
            .presentation
            .mouse_selection_drag_state
            .as_ref()
            .filter(|state| state.pane_id == pane_id)
            .map(|state| state.origin_position)
            .unwrap_or(position);
        if finish && !self.active_copy_modes.contains_key(pane_id.as_str()) {
            self.presentation.mouse_selection_drag_state = None;
            return Ok(true);
        }
        let copied = {
            let copy_mode = self.ensure_mouse_selection_copy_mode(pane_id.as_str())?;
            let start = copy_mode
                .selection()
                .map(|(start, _)| start)
                .unwrap_or_else(|| runtime_copy_position_for_view(copy_mode, anchor));
            if let Some(edge) = target.edge {
                copy_mode.scroll_by(edge.scroll_delta(origin, position));
            }
            let end = runtime_copy_position_for_view(copy_mode, target.position);
            copy_mode.select_range(start, end)?;
            finish.then(|| copy_mode.copy_selection()).transpose()?
        };
        if finish {
            self.presentation.mouse_selection_drag_state = None;
            self.active_copy_modes.remove(pane_id.as_str());
            self.scrollback_copy_mode_panes.remove(pane_id.as_str());
            if let Some(copied) = copied {
                self.copy_text_to_buffer_and_host_clipboard(
                    "mouse",
                    copied,
                    format!("pane:{pane_id}:mouse"),
                )?;
            }
        } else {
            self.presentation.mouse_selection_drag_state = Some(MouseSelectionDragState {
                pane_id,
                position: anchor,
                origin_position: origin,
                autoscroll_position: target.edge.map(|_| position),
            });
        }
        Ok(true)
    }

    /// Ensures mouse drag selection has a copy buffer for the selected pane.
    ///
    /// Alternate-screen applications are excluded from normal scrollback by
    /// design, but mouse drag selection is an explicit copy operation over the
    /// visible pane body. For that path, seed copy mode from visible rows so
    /// full-screen terminal apps can still be copied without changing history
    /// capture semantics.
    fn ensure_mouse_selection_copy_mode(&mut self, pane_id: &str) -> Result<&mut CopyMode> {
        if !self.active_copy_modes.contains_key(pane_id) {
            let viewport_rows = self.copy_mode_viewport_rows_for_pane(pane_id);
            let screen = self.pane_screens.get(pane_id).ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane screen not found",
                )
            })?;
            let copy_mode = if screen.alternate_screen_active() {
                CopyMode::from_visible_screen(screen, viewport_rows)?
            } else {
                CopyMode::from_screen(screen, viewport_rows)?
            };
            self.active_copy_modes
                .insert(pane_id.to_string(), copy_mode);
        }
        self.active_copy_modes
            .get_mut(pane_id)
            .ok_or_else(|| MezError::invalid_state("active copy mode was not retained"))
    }

    /// Selects and copies the readline-style word under one pane-local position.
    ///
    /// # Parameters
    /// - `primary_client_id`: The primary client whose focus follows the click.
    /// - `pane_id`: Pane whose copy-mode buffer supplies the word text.
    /// - `position`: Pane-local terminal cell used as the word-selection anchor.
    fn copy_word_at_pane_position(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        position: CopyPosition,
    ) -> Result<bool> {
        self.session
            .select_pane_global(primary_client_id, pane_id)?;
        // Ensure a copy mode exists, then take ownership so the selection
        // highlight persists for one render frame before cleanup.
        self.ensure_active_copy_mode(pane_id)?;
        let mut copy_mode = self
            .active_copy_modes
            .remove(pane_id)
            .ok_or_else(|| MezError::invalid_state("active copy mode was not retained"))?;
        let copied = {
            let position = runtime_copy_position_for_view(&copy_mode, position);
            copy_mode.select_word_at(position)?;
            copy_mode.copy_selection()?
        };
        self.presentation.mouse_selection_drag_state = None;
        self.scrollback_copy_mode_panes.remove(pane_id);
        self.presentation.deferred_word_copy_cleanup.replace(Some((
            pane_id.to_string(),
            copy_mode,
            current_unix_millis().saturating_add(DOUBLE_CLICK_WORD_SELECTION_HIGHLIGHT_MS),
        )));
        self.copy_text_to_buffer_and_host_clipboard(
            "mouse",
            copied,
            format!("pane:{pane_id}:mouse-word"),
        )?;
        Ok(true)
    }

    /// Runs the mouse resize drag update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_resize_drag_update(
        &mut self,
        column: u16,
        row: u16,
    ) -> Result<Option<MouseResizeDragUpdate>> {
        if let Some(state) = self.presentation.mouse_resize_drag_state.clone() {
            return Ok(Some(mouse_resize_update_from_state(state, column, row)));
        }
        let Some(state) = self.mouse_resize_drag_state_at(column, row) else {
            return Ok(None);
        };
        let update = mouse_resize_update_from_state(state.clone(), column, row);
        self.presentation.mouse_resize_drag_state = Some(state);
        Ok(Some(update))
    }

    /// Runs the mouse resize drag state at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_resize_drag_state_at(&self, column: u16, row: u16) -> Option<MouseResizeDragState> {
        let window = self.session.active_window()?;
        let window_frame_visible = self.window_frames_enabled;
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        if group_top_offset > 0 && row == 0 {
            return None;
        }
        let mut display_window = window.clone();
        display_window.size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_top_offset).max(1),
        )
        .ok()?;
        let local_row = row.checked_sub(group_top_offset)?;
        if window_frame_visible {
            match self.window_frame_position {
                TerminalFramePosition::Top if local_row == 0 => return None,
                TerminalFramePosition::Bottom
                    if local_row == display_window.size.rows.saturating_sub(1) =>
                {
                    return None;
                }
                _ => {}
            }
        }
        let row_offset = group_top_offset.saturating_add(u16::from(
            window_frame_visible && self.window_frame_position == TerminalFramePosition::Top,
        ));
        let body_row = row.checked_sub(row_offset)?;
        let geometries = rendered_pane_geometries(&display_window, window_frame_visible).ok()?;

        vertical_mouse_resize_state(&geometries, column, body_row)
            .or_else(|| horizontal_mouse_resize_state(&geometries, body_row, column, row_offset))
    }

    /// Runs the mouse pane target at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_pane_target_at(&self, position: CopyPosition) -> Option<MousePaneTarget> {
        let window = self.session.active_window()?;
        let window_frame_visible = self.window_frames_enabled;
        let column = u16::try_from(position.column).ok()?;
        let row = u16::try_from(position.line).ok()?;
        let group_top_offset = u16::from(self.session.window_groups().len() > 1);
        if group_top_offset > 0 && row == 0 {
            return None;
        }
        let mut display_window = window.clone();
        display_window.size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_top_offset).max(1),
        )
        .ok()?;
        let local_row = row.checked_sub(group_top_offset)?;
        let window_frame_top_offset = group_top_offset.saturating_add(u16::from(
            window_frame_visible && self.window_frame_position == TerminalFramePosition::Top,
        ));
        if window_frame_visible {
            match self.window_frame_position {
                TerminalFramePosition::Top if local_row == 0 => return None,
                TerminalFramePosition::Bottom
                    if local_row == display_window.size.rows.saturating_sub(1) =>
                {
                    return None;
                }
                _ => {}
            }
        }
        let body_row = row.checked_sub(window_frame_top_offset)?;
        let geometries = rendered_pane_geometries(&display_window, window_frame_visible).ok()?;
        for geometry in &geometries {
            let region_size = pane_render_region_size_for_geometry(geometry, &geometries);
            let row_end = geometry.row.saturating_add(region_size.rows);
            let column_end = geometry.column.saturating_add(region_size.columns);
            if body_row < geometry.row
                || body_row >= row_end
                || column < geometry.column
                || column >= column_end
            {
                continue;
            }
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.index == geometry.index)?;
            let pane_frame_top_offset = u16::from(
                self.pane_frames_enabled
                    && self.pane_frame_position == TerminalFramePosition::Top
                    && !pane_frame_merges_into_divider(
                        geometry,
                        &geometries,
                        self.pane_frame_position,
                    ),
            );
            if pane_frame_top_offset > 0 && body_row == geometry.row {
                return None;
            }
            let local_row = body_row
                .saturating_sub(geometry.row)
                .saturating_sub(pane_frame_top_offset);
            let local_column = column.saturating_sub(geometry.column);
            return Some(MousePaneTarget {
                pane_id: pane.id.to_string(),
                position: CopyPosition {
                    line: usize::from(local_row),
                    column: usize::from(local_column),
                },
            });
        }
        None
    }

    /// Runs the mouse selection target at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_selection_target_at(&self, position: CopyPosition) -> Result<MouseSelectionTarget> {
        if let Some(state) = self.presentation.mouse_selection_drag_state.as_ref()
            && let Some(target) =
                self.mouse_pane_selection_target_at(state.pane_id.as_str(), position)
        {
            return Ok(target);
        }
        if let Some(target) = self.mouse_pane_target_at(position) {
            if let Some(selection_target) =
                self.mouse_pane_selection_target_at(target.pane_id.as_str(), position)
            {
                return Ok(selection_target);
            }
            return Ok(MouseSelectionTarget {
                pane_id: target.pane_id,
                position: target.position,
                edge: None,
            });
        }
        let active_pane_id = self.active_pane_id()?.to_string();
        if let Some(selection_target) =
            self.mouse_pane_selection_target_at(active_pane_id.as_str(), position)
        {
            return Ok(selection_target);
        }
        Ok(MouseSelectionTarget {
            pane_id: active_pane_id,
            position: CopyPosition { line: 0, column: 0 },
            edge: None,
        })
    }

    /// Runs the mouse pane selection target at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mouse_pane_selection_target_at(
        &self,
        pane_id: &str,
        position: CopyPosition,
    ) -> Option<MouseSelectionTarget> {
        let window = self.session.active_window()?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let (region_row, region_column, content_size) =
            self.copy_mode_overlay_region(window, pane.index)?;
        let row = isize::try_from(position.line).ok()?;
        let column = isize::try_from(position.column).ok()?;
        let content_start_row = isize::try_from(region_row).ok()?;
        let content_rows = isize::try_from(content_size.rows).ok()?.max(1);
        let content_last_row = content_start_row.saturating_add(content_rows.saturating_sub(1));
        let edge = if row <= content_start_row {
            Some(MouseSelectionEdge::Above)
        } else if row >= content_last_row {
            Some(MouseSelectionEdge::Below)
        } else {
            None
        };
        let local_line = if row < content_start_row {
            0
        } else if row > content_last_row {
            usize::from(content_size.rows.saturating_sub(1))
        } else {
            usize::try_from(row.saturating_sub(content_start_row)).ok()?
        };
        let content_columns = usize::from(content_size.columns);
        let geometry_column = isize::try_from(region_column).ok()?;
        let content_end_column =
            geometry_column.saturating_add(isize::try_from(content_size.columns).ok()?);
        let local_column = if column < geometry_column {
            0
        } else if column >= content_end_column {
            content_columns
        } else {
            usize::try_from(column.saturating_sub(geometry_column)).ok()?
        };
        Some(MouseSelectionTarget {
            pane_id: pane_id.to_string(),
            position: CopyPosition {
                line: local_line,
                column: local_column,
            },
            edge,
        })
    }
}
