//! Terminal pane-divider geometry and glyph rendering helpers.
//!
//! This module owns the conversion from pane geometries into mux-managed
//! divider cells, box-drawing glyph connection masks, mouse-border hit cells,
//! and styled/plain divider canvas writes.

use std::collections::BTreeMap;

use crate::layout::{PaneGeometry, Window, range_overlap_u16};
use crate::terminal::{
    GraphicRendition, MouseBorderCell, TerminalStyleSpan, UiTheme,
};

pub use mez_mux::presentation::pane_frame_merges_into_divider;

use super::{TerminalRenderCell, pane_border_rendition, write_single_width_cell};

/// Returns the rendered cells occupied by mux-managed pane dividers.
pub fn pane_border_cells_for_geometries(
    geometries: &[PaneGeometry],
    row_offset: u16,
) -> Vec<MouseBorderCell> {
    pane_divider_cells(geometries, true)
        .into_iter()
        .map(|cell| MouseBorderCell {
            column: cell.column,
            row: cell.row.saturating_add(row_offset),
        })
        .collect()
}

/// Carries one pane-divider cell and the glyph selected for its connection mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneDividerCell {
    /// Terminal column for the divider cell.
    column: u16,
    /// Terminal row for the divider cell.
    row: u16,
    /// Box-drawing glyph rendered into this cell.
    glyph: char,
}

/// Directional strokes that meet in one mux-managed pane divider cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PaneDividerConnections {
    /// Whether a stroke leaves this cell upward.
    up: bool,
    /// Whether a stroke leaves this cell downward.
    down: bool,
    /// Whether a stroke leaves this cell leftward.
    left: bool,
    /// Whether a stroke leaves this cell rightward.
    right: bool,
    /// Whether this cell belongs to a vertical segment.
    vertical: bool,
    /// Whether this cell belongs to a horizontal segment.
    horizontal: bool,
}

impl PaneDividerConnections {
    /// Records a vertical divider stroke through this cell.
    fn add_vertical(&mut self, up: bool, down: bool) {
        self.vertical = true;
        self.up |= up;
        self.down |= down;
    }

    /// Records a horizontal divider stroke through this cell.
    fn add_horizontal(&mut self, left: bool, right: bool) {
        self.horizontal = true;
        self.left |= left;
        self.right |= right;
    }

    /// Returns whether this cell belongs to a vertical divider segment.
    fn has_vertical(&self) -> bool {
        self.vertical
    }

    /// Returns whether this cell belongs to a horizontal divider segment.
    fn has_horizontal(&self) -> bool {
        self.horizontal
    }

    /// Chooses the thin box-drawing glyph that matches the recorded strokes.
    fn glyph(self) -> char {
        let mut up = self.up;
        let mut down = self.down;
        let mut left = self.left;
        let mut right = self.right;
        if self.vertical && !up && !down {
            up = true;
            down = true;
        }
        if self.horizontal && !left && !right {
            left = true;
            right = true;
        }
        match (up, down, left, right) {
            (true, true, true, true) => '\u{253c}',
            (true, true, true, false) => '\u{2524}',
            (true, true, false, true) => '\u{251c}',
            (true, false, true, true) => '\u{2534}',
            (false, true, true, true) => '\u{252c}',
            (true, false, false, true) => '\u{2514}',
            (true, false, true, false) => '\u{2518}',
            (false, true, false, true) => '\u{250c}',
            (false, true, true, false) => '\u{2510}',
            (true, true, false, false) => '\u{2502}',
            (false, false, true, true) => '\u{2500}',
            (true, false, false, false) | (false, true, false, false) => '\u{2502}',
            (false, false, true, false) | (false, false, false, true) => '\u{2500}',
            (false, false, false, false) => ' ',
        }
    }
}

/// Returns the box-drawing glyph for an explicit divider-connection mask.
///
/// This test helper keeps the glyph contract covered independently from any
/// particular split-tree shape, while production rendering still derives the
/// same connection mask from pane geometry.
#[cfg(test)]
pub(crate) fn pane_divider_glyph_for_test(up: bool, down: bool, left: bool, right: bool) -> char {
    PaneDividerConnections {
        up,
        down,
        left,
        right,
        vertical: up || down,
        horizontal: left || right,
    }
    .glyph()
}

/// Builds pane-divider cells from neighboring pane geometry.
fn pane_divider_cells(
    geometries: &[PaneGeometry],
    include_horizontal: bool,
) -> Vec<PaneDividerCell> {
    if geometries.len() < 2 {
        return Vec::new();
    }
    let mut cells = BTreeMap::new();
    for (index, first) in geometries.iter().enumerate() {
        for second in geometries.iter().skip(index.saturating_add(1)) {
            let first_right = first.column.saturating_add(first.columns);
            let second_right = second.column.saturating_add(second.columns);
            let first_bottom = first.row.saturating_add(first.rows);
            let second_bottom = second.row.saturating_add(second.rows);

            if first_right == second.column || second_right == first.column {
                let boundary = first_right.min(second_right).saturating_sub(1);
                let start = first.row.max(second.row);
                let end = first_bottom.min(second_bottom);
                for row in start..end {
                    insert_vertical_divider_cell(
                        &mut cells,
                        boundary,
                        row,
                        row > start,
                        row.saturating_add(1) < end,
                    );
                }
            }
            if include_horizontal && (first_bottom == second.row || second_bottom == first.row) {
                let boundary = first_bottom.min(second_bottom).saturating_sub(1);
                let start = first.column.max(second.column);
                let end = first_right.min(second_right);
                for column in start..end {
                    insert_horizontal_divider_cell(
                        &mut cells,
                        column,
                        boundary,
                        column > start,
                        column.saturating_add(1) < end,
                    );
                }
            }
        }
    }
    connect_touching_divider_cells(&mut cells);
    cells
        .into_iter()
        .map(|((column, row), connections)| PaneDividerCell {
            column,
            row,
            glyph: connections.glyph(),
        })
        .collect()
}

/// Returns whether a pane geometry has a shared divider immediately below it.
pub(super) fn geometry_has_bottom_divider(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
) -> bool {
    let bottom = geometry.row.saturating_add(geometry.rows);
    geometries.iter().any(|candidate| {
        candidate.index != geometry.index
            && candidate.row == bottom
            && range_overlap_u16(
                geometry.column,
                geometry.column.saturating_add(geometry.columns),
                candidate.column,
                candidate.column.saturating_add(candidate.columns),
            ) > 0
    })
}

/// Returns whether the geometry's right edge is occupied by a shared divider.
pub(super) fn geometry_has_right_divider(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
) -> bool {
    let right = geometry.column.saturating_add(geometry.columns);
    geometries.iter().any(|candidate| {
        candidate.index != geometry.index
            && candidate.column == right
            && range_overlap_u16(
                geometry.row,
                geometry.row.saturating_add(geometry.rows),
                candidate.row,
                candidate.row.saturating_add(candidate.rows),
            ) > 0
    })
}

/// Inserts one cell from a vertical divider segment.
fn insert_vertical_divider_cell(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    column: u16,
    row: u16,
    up: bool,
    down: bool,
) {
    cells
        .entry((column, row))
        .or_default()
        .add_vertical(up, down);
}

/// Inserts one cell from a horizontal divider segment.
fn insert_horizontal_divider_cell(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    column: u16,
    row: u16,
    left: bool,
    right: bool,
) {
    cells
        .entry((column, row))
        .or_default()
        .add_horizontal(left, right);
}

/// Connects divider cells that touch because neighboring panes share a seam.
fn connect_touching_divider_cells(cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>) {
    let snapshot = cells.clone();
    for (&(column, row), connections) in &snapshot {
        if connections.has_vertical() {
            if let Some(below_row) = row.checked_add(1) {
                let below = (column, below_row);
                if snapshot
                    .get(&below)
                    .is_some_and(PaneDividerConnections::has_vertical)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.down = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&below) {
                        neighbor.up = true;
                    }
                }
            }
            if column > 0 {
                let left = (column.saturating_sub(1), row);
                if snapshot
                    .get(&left)
                    .is_some_and(PaneDividerConnections::has_horizontal)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.left = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&left) {
                        neighbor.right = true;
                    }
                }
            }
            if let Some(right_column) = column.checked_add(1) {
                let right = (right_column, row);
                if snapshot
                    .get(&right)
                    .is_some_and(PaneDividerConnections::has_horizontal)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.right = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&right) {
                        neighbor.left = true;
                    }
                }
            }
        }
        if connections.has_horizontal() {
            if let Some(right_column) = column.checked_add(1) {
                let right = (right_column, row);
                if snapshot
                    .get(&right)
                    .is_some_and(PaneDividerConnections::has_horizontal)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.right = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&right) {
                        neighbor.left = true;
                    }
                }
            }
            if row > 0 {
                let above = (column, row.saturating_sub(1));
                if snapshot
                    .get(&above)
                    .is_some_and(PaneDividerConnections::has_vertical)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.up = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&above) {
                        neighbor.down = true;
                    }
                }
            }
            if let Some(below_row) = row.checked_add(1) {
                let below = (column, below_row);
                if snapshot
                    .get(&below)
                    .is_some_and(PaneDividerConnections::has_vertical)
                {
                    if let Some(current) = cells.get_mut(&(column, row)) {
                        current.down = true;
                    }
                    if let Some(neighbor) = cells.get_mut(&below) {
                        neighbor.up = true;
                    }
                }
            }
        }
    }
}

/// Writes pane-divider glyphs into a plain text canvas.
pub(super) fn draw_pane_dividers(
    canvas: &mut [Vec<TerminalRenderCell>],
    geometries: &[PaneGeometry],
    include_horizontal: bool,
) {
    for cell in pane_divider_cells(geometries, include_horizontal) {
        let row = usize::from(cell.row);
        let column = usize::from(cell.column);
        if let Some(line) = canvas.get_mut(row) {
            write_single_width_cell(line, column, cell.glyph);
        }
    }
}

/// Writes pane-divider glyphs and style spans into a styled text canvas.
pub(super) fn draw_styled_pane_dividers(
    text_canvas: &mut [Vec<TerminalRenderCell>],
    style_canvas: &mut [Vec<TerminalStyleSpan>],
    geometries: &[PaneGeometry],
    include_horizontal: bool,
    window: &Window,
    ui_theme: &UiTheme,
) {
    for cell in pane_divider_cells(geometries, include_horizontal) {
        let row = usize::from(cell.row);
        let column = usize::from(cell.column);
        if let Some(line) = text_canvas.get_mut(row) {
            write_single_width_cell(line, column, cell.glyph);
        }
        if let Some(spans) = style_canvas.get_mut(row) {
            let rendition = if divider_cell_touches_active_pane(cell, geometries, window) {
                pane_border_rendition(true, ui_theme)
            } else {
                pane_divider_rendition(ui_theme)
            };
            spans.push(TerminalStyleSpan {
                start: column,
                length: 1,
                rendition,
            });
        }
    }
}

/// Builds style spans for divider junctions that bound a merged pane status row.
pub(super) fn merged_pane_frame_boundary_style_spans(
    geometries: &[PaneGeometry],
    row: u16,
    column_start: usize,
    width: usize,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    pane_divider_cells(geometries, true)
        .into_iter()
        .filter(|cell| {
            cell.row == row && merged_pane_frame_boundary_cell(*cell, column_start, width)
        })
        .map(|cell| TerminalStyleSpan {
            start: usize::from(cell.column),
            length: 1,
            rendition: pane_divider_rendition(ui_theme),
        })
        .collect()
}

/// Returns the stable divider rendition used for merged pane-frame boundary
/// caps.
fn pane_divider_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(ui_theme.colors.pane_divider.foreground),
        background: None,
        ..GraphicRendition::default()
    }
}

/// Returns whether a divider cell acts as a non-vertical boundary cap for a
/// merged pane status row.
fn merged_pane_frame_boundary_cell(
    cell: PaneDividerCell,
    column_start: usize,
    width: usize,
) -> bool {
    if cell.glyph == '\u{2502}' {
        return false;
    }
    let column = usize::from(cell.column);
    let column_end = column_start.saturating_add(width);
    (column_start > 0 && column.saturating_add(1) == column_start) || column == column_end
}

/// Reports whether one divider cell touches the active pane's border.
fn divider_cell_touches_active_pane(
    cell: PaneDividerCell,
    geometries: &[PaneGeometry],
    window: &Window,
) -> bool {
    let active_index = window.active_pane_index();
    let Some(geometry) = geometries
        .iter()
        .find(|geometry| geometry.index == active_index)
    else {
        return false;
    };
    let column = cell.column;
    let row = cell.row;
    let vertical_overlap = row >= geometry.row && row < geometry.row.saturating_add(geometry.rows);
    let horizontal_overlap =
        column >= geometry.column && column < geometry.column.saturating_add(geometry.columns);
    let right_edge = geometry
        .column
        .saturating_add(geometry.columns)
        .saturating_sub(1);
    let bottom_edge = geometry.row.saturating_add(geometry.rows).saturating_sub(1);
    (vertical_overlap && (column == right_edge || column.saturating_add(1) == geometry.column))
        || (horizontal_overlap && (row == bottom_edge || row.saturating_add(1) == geometry.row))
}

#[cfg(test)]
mod tests {
    use super::super::blank_render_cells;
    use super::*;
    use crate::ids::IdFactory;
    use crate::layout::SplitDirection;
    use crate::terminal::{Size, Window};

    /// Verifies pane divider styling still uses the active border palette when
    /// a divider cell touches the active pane border.
    #[test]
    fn styled_pane_dividers_highlight_active_pane_border() {
        let mut ids = IdFactory::default();
        let mut window = Window::new(&mut ids, 0, "main", Size::new(8, 4).unwrap());
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
        let geometries = window.pane_geometries();
        let rows = usize::from(window.size.rows);
        let columns = usize::from(window.size.columns);
        let mut text_canvas = blank_render_cells(rows, columns, ' ');
        let mut style_canvas = vec![Vec::new(); rows];
        let ui_theme = UiTheme::default();

        draw_styled_pane_dividers(
            &mut text_canvas,
            &mut style_canvas,
            &geometries,
            true,
            &window,
            &ui_theme,
        );

        let active = pane_border_rendition(true, &ui_theme);
        assert!(
            style_canvas
                .iter()
                .flatten()
                .any(|span| span.rendition == active)
        );
    }

    /// Verifies merged pane-frame boundary caps keep the stable divider
    /// palette even when pane focus moves between panes.
    #[test]
    fn merged_pane_frame_boundaries_use_focus_stable_rendition() {
        let mut ids = IdFactory::default();
        let mut window = Window::new(&mut ids, 0, "main", Size::new(28, 6).unwrap());
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
        window
            .split_active(&mut ids, SplitDirection::Horizontal)
            .unwrap();
        let ui_theme = UiTheme::default();
        let stable = pane_divider_rendition(&ui_theme);

        let geometries = window.pane_geometries();
        let target = geometries
            .iter()
            .max_by_key(|geometry| (geometry.row, geometry.column))
            .copied()
            .expect("split window should produce pane geometries");
        let row = target.row.saturating_sub(1);
        let column_start = usize::from(target.column);
        let width = usize::from(target.columns);

        let focused_boundary_spans = merged_pane_frame_boundary_style_spans(
            &geometries,
            row,
            column_start,
            width,
            &ui_theme,
        );

        window.select_pane("0").unwrap();
        let unfocused_boundary_spans = merged_pane_frame_boundary_style_spans(
            &geometries,
            row,
            column_start,
            width,
            &ui_theme,
        );

        assert!(!focused_boundary_spans.is_empty());
        assert!(
            focused_boundary_spans
                .iter()
                .all(|span| span.length == 1 && span.rendition == stable)
        );
        assert_eq!(focused_boundary_spans, unfocused_boundary_spans);
    }

    /// Verifies neutral divider cells honor the dedicated divider palette
    /// instead of falling back to the inactive pane-border colors.
    #[test]
    fn styled_pane_dividers_use_dedicated_divider_palette_for_neutral_cells() {
        let mut ids = IdFactory::default();
        let mut window = Window::new(&mut ids, 0, "main", Size::new(28, 6).unwrap());
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
        window
            .split_active(&mut ids, SplitDirection::Horizontal)
            .unwrap();
        let geometries = window.pane_geometries();
        let rows = usize::from(window.size.rows);
        let columns = usize::from(window.size.columns);
        let mut text_canvas = blank_render_cells(rows, columns, ' ');
        let mut style_canvas = vec![Vec::new(); rows];
        let ui_theme = UiTheme::default();
        let divider = pane_divider_rendition(&ui_theme);

        draw_styled_pane_dividers(
            &mut text_canvas,
            &mut style_canvas,
            &geometries,
            true,
            &window,
            &ui_theme,
        );

        assert!(
            style_canvas
                .iter()
                .flatten()
                .any(|span| span.rendition == divider)
        );
    }
}
