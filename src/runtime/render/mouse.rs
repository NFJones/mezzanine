//! Runtime render mouse geometry helpers.
//!
//! This module owns pane resize drag geometry for the runtime render layer.
//! Higher-level runtime methods still decide when mouse input is active; this
//! module converts pane geometry and drag state into concrete resize updates.

use crate::layout::range_overlap_u16;
use crate::runtime::{MIN_PANE_COLUMNS, MIN_PANE_ROWS, MouseResizeDragState, PaneGeometry};

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
