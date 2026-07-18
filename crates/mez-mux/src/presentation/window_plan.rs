//! Deterministic window presentation planning.
//!
//! This module owns frame reservation, focus and zoom selection, pane geometry,
//! absolute clipping, divider composition, and content hit mapping. Product
//! renderers supply resolved options and apply content, styling, and I/O.

use std::collections::BTreeMap;

use mez_terminal::TerminalSize;

use crate::layout::{PaneGeometry, range_overlap_u16};

use super::TerminalFramePosition;

/// Places a one-row frame within an authoritative terminal region.
///
/// Top frames are inserted before body content, while bottom frames replace
/// the final available row. In both cases the result is clipped to the
/// authoritative row count.
pub fn place_window_frame<T>(
    lines: &mut Vec<T>,
    frame: T,
    position: TerminalFramePosition,
    authoritative_rows: u16,
) {
    let rows = usize::from(authoritative_rows);
    match position {
        TerminalFramePosition::Top => {
            lines.insert(0, frame);
            lines.truncate(rows);
        }
        TerminalFramePosition::Bottom => {
            lines.truncate(rows.saturating_sub(1));
            lines.push(frame);
            lines.truncate(rows);
        }
    }
}

/// Places a conditional top window-group frame above rendered window rows.
pub fn place_group_frame<T>(lines: &mut Vec<T>, frame: T, authoritative_rows: u16) {
    let rows = usize::from(authoritative_rows);
    lines.insert(0, frame);
    lines.truncate(rows);
}

/// Returns the drawable window body after reserving a mux-managed frame row.
pub fn rendered_window_body_size(size: TerminalSize, window_frames_enabled: bool) -> TerminalSize {
    let rows = if window_frames_enabled {
        size.rows.saturating_sub(1)
    } else {
        size.rows
    };
    TerminalSize {
        columns: size.columns,
        rows: rows.max(1),
    }
}

/// Frame and group-row choices consumed by mux presentation planning.
///
/// Product configuration is resolved before constructing this snapshot. It
/// intentionally carries no templates, styles, terminal screens, or runtime
/// service state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowPresentationOptions {
    /// Whether the conditional window-group row is visible.
    pub group_frame_visible: bool,
    /// Whether the window frame reserves one row.
    pub window_frame_visible: bool,
    /// Placement of the window frame inside the group-adjusted display area.
    pub window_frame_position: TerminalFramePosition,
    /// Whether each pane reserves or merges one frame row.
    pub pane_frames_visible: bool,
    /// Placement of pane frames inside their render regions.
    pub pane_frame_position: TerminalFramePosition,
}

/// Absolute clipped terminal region selected by a mux presentation plan.
///
/// Unlike [`TerminalSize`], a clipped region may have zero rows or columns.
/// This lets tiny terminals represent fully occluded content without inventing
/// out-of-bounds cursor, overlay, or mouse targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PresentationRegion {
    /// First row in authoritative window coordinates.
    pub row: u16,
    /// First column in authoritative window coordinates.
    pub column: u16,
    /// Visible width after clipping to the authoritative window.
    pub columns: u16,
    /// Visible height after clipping to the authoritative window.
    pub rows: u16,
}

impl PresentationRegion {
    /// Returns whether the region contains any visible terminal cells.
    pub const fn is_empty(self) -> bool {
        self.columns == 0 || self.rows == 0
    }

    /// Returns whether an absolute terminal cell is inside the region.
    pub fn contains(self, row: u16, column: u16) -> bool {
        row >= self.row
            && row < self.row.saturating_add(self.rows)
            && column >= self.column
            && column < self.column.saturating_add(self.columns)
    }
}

/// One pane selected for dependency-neutral window presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanePresentationPlan {
    /// Pane index used to locate the source pane and its prepared rows.
    pub source_index: usize,
    /// Whether this pane owns mux focus in the planned window snapshot.
    pub active: bool,
    /// Pane placement within the drawable window body.
    pub geometry: PaneGeometry,
    /// Visible pane region after shared-divider reservations.
    pub render_region_size: TerminalSize,
    /// Absolute clipped region occupied by pane body plus a standalone frame.
    pub render_region: PresentationRegion,
    /// Pane body size before clipping to the authoritative window.
    pub content_size: TerminalSize,
    /// Absolute clipped region available to content, prompts, and overlays.
    pub content_region: PresentationRegion,
    /// Absolute row occupied by the pane frame, when enabled and visible.
    pub frame_row: Option<u16>,
    /// Whether the pane frame occupies an adjacent shared divider.
    pub frame_merges_into_divider: bool,
}

/// One absolute pane-content hit selected by the presentation plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanePresentationTarget {
    /// Pane index used to locate the owning mux pane.
    pub source_index: usize,
    /// Row relative to the pane content region.
    pub row: u16,
    /// Column relative to the pane content region.
    pub column: u16,
}

/// Dependency-neutral presentation plan for one mux window.
///
/// Geometry remains body-local for canvas composition while absolute regions
/// are authoritative for cursor, overlay, border, and mouse adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowPresentationPlan {
    /// Original authoritative window size.
    pub authoritative_size: TerminalSize,
    /// Size remaining after reserving the optional group frame.
    pub display_size: TerminalSize,
    /// Drawable size after additionally reserving the optional window frame.
    pub body_size: TerminalSize,
    /// Absolute row occupied by the group frame, when visible.
    pub group_frame_row: Option<u16>,
    /// Absolute row occupied by the window frame, when visible.
    pub window_frame_row: Option<u16>,
    /// Absolute row at which body-local pane geometry begins.
    pub body_row_offset: u16,
    /// Ordered pane sources, focus, frame placement, and clipped regions.
    pub panes: Vec<PanePresentationPlan>,
}

impl WindowPresentationPlan {
    /// Returns the planned pane with the requested mux index.
    pub fn pane(&self, source_index: usize) -> Option<&PanePresentationPlan> {
        self.panes
            .iter()
            .find(|pane| pane.source_index == source_index)
    }

    /// Maps an absolute terminal cell to pane-local content coordinates.
    ///
    /// Group, window-frame, pane-frame, divider, clipped, and otherwise stale
    /// cells deliberately produce no target.
    pub fn pane_content_target_at(&self, row: u16, column: u16) -> Option<PanePresentationTarget> {
        self.panes.iter().find_map(|pane| {
            pane.content_region
                .contains(row, column)
                .then_some(PanePresentationTarget {
                    source_index: pane.source_index,
                    row: row.saturating_sub(pane.content_region.row),
                    column: column.saturating_sub(pane.content_region.column),
                })
        })
    }

    /// Maps an absolute terminal row to the drawable window-body row.
    pub fn body_row_at(&self, row: u16) -> Option<u16> {
        let body_row = row.checked_sub(self.body_row_offset)?;
        (body_row < self.body_size.rows).then_some(body_row)
    }

    /// Returns body-local pane geometries in presentation order.
    pub fn pane_geometries(&self) -> Vec<PaneGeometry> {
        self.panes.iter().map(|pane| pane.geometry).collect()
    }
}

/// Plans group/window/pane frames, focus, zoom, geometry, clipping, and hit regions.
///
/// Returns `None` when the window has no panes or references a missing zoomed
/// pane. Product renderers remain responsible for preparing pane content,
/// resolving frame text and styles, applying overlays, and writing to the host
/// terminal.
pub fn plan_window_presentation(
    window: &crate::layout::Window,
    options: WindowPresentationOptions,
) -> Option<WindowPresentationPlan> {
    if window.panes().is_empty() {
        return None;
    }
    let active_source_index = window.panes().get(window.active_pane_index())?.index;
    let group_rows = u16::from(options.group_frame_visible);
    let display_size = TerminalSize {
        columns: window.size.columns,
        rows: window.size.rows.saturating_sub(group_rows).max(1),
    };
    let body_size = rendered_window_body_size(display_size, options.window_frame_visible);
    let window_frame_local_row =
        options
            .window_frame_visible
            .then_some(match options.window_frame_position {
                TerminalFramePosition::Top => 0,
                TerminalFramePosition::Bottom => display_size.rows.saturating_sub(1),
            });
    let window_frame_row = window_frame_local_row
        .and_then(|row| visible_absolute_row(window.size.rows, group_rows.saturating_add(row)));
    let body_row_offset = group_rows.saturating_add(u16::from(
        options.window_frame_visible && options.window_frame_position == TerminalFramePosition::Top,
    ));
    let geometries = if let Some(zoomed_id) = window.zoomed_pane_id() {
        let pane = window.panes().iter().find(|pane| pane.id == *zoomed_id)?;
        vec![PaneGeometry {
            index: pane.index,
            column: 0,
            row: 0,
            columns: body_size.columns,
            rows: body_size.rows,
        }]
    } else {
        window.pane_geometries_for_size(body_size)
    };
    if geometries.iter().any(|geometry| {
        !window
            .panes()
            .iter()
            .any(|pane| pane.index == geometry.index)
    }) {
        return None;
    }
    let panes = geometries
        .iter()
        .copied()
        .map(|geometry| {
            let render_region_size = pane_render_region_size_for_geometry(&geometry, &geometries);
            let frame_merges_into_divider = options.pane_frames_visible
                && pane_frame_merges_into_divider(
                    &geometry,
                    &geometries,
                    options.pane_frame_position,
                );
            let content_size = pane_content_size_for_geometry(
                &geometry,
                &geometries,
                options.pane_frames_visible,
                options.pane_frame_position,
            );
            let pane_frame_top_rows = u16::from(
                options.pane_frames_visible
                    && options.pane_frame_position == TerminalFramePosition::Top
                    && !frame_merges_into_divider,
            );
            let render_row = body_row_offset.saturating_add(geometry.row);
            let content_row = render_row.saturating_add(pane_frame_top_rows);
            let frame_row = options.pane_frames_visible.then(|| {
                pane_frame_row_for_geometry(
                    &geometry,
                    &geometries,
                    options.pane_frame_position,
                    body_row_offset,
                )
            });
            PanePresentationPlan {
                source_index: geometry.index,
                active: geometry.index == active_source_index,
                geometry,
                render_region_size,
                render_region: clipped_presentation_region(
                    window.size,
                    render_row,
                    geometry.column,
                    render_region_size,
                ),
                content_size,
                content_region: clipped_presentation_region(
                    window.size,
                    content_row,
                    geometry.column,
                    content_size,
                ),
                frame_row: frame_row.and_then(|row| visible_absolute_row(window.size.rows, row)),
                frame_merges_into_divider,
            }
        })
        .collect();
    Some(WindowPresentationPlan {
        authoritative_size: window.size,
        display_size,
        body_size,
        group_frame_row: options
            .group_frame_visible
            .then_some(0)
            .and_then(|row| visible_absolute_row(window.size.rows, row)),
        window_frame_row,
        body_row_offset,
        panes,
    })
}

fn visible_absolute_row(authoritative_rows: u16, row: u16) -> Option<u16> {
    (row < authoritative_rows).then_some(row)
}

fn clipped_presentation_region(
    authoritative_size: TerminalSize,
    row: u16,
    column: u16,
    size: TerminalSize,
) -> PresentationRegion {
    PresentationRegion {
        row,
        column,
        columns: size
            .columns
            .min(authoritative_size.columns.saturating_sub(column)),
        rows: size.rows.min(authoritative_size.rows.saturating_sub(row)),
    }
}

/// Returns whether a pane has a shared horizontal divider immediately below it.
pub fn geometry_has_bottom_divider(geometry: &PaneGeometry, geometries: &[PaneGeometry]) -> bool {
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

/// Returns whether a pane has a shared vertical divider immediately to its right.
pub fn geometry_has_right_divider(geometry: &PaneGeometry, geometries: &[PaneGeometry]) -> bool {
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

/// Returns the visible pane region after reserving shared divider cells.
pub fn pane_render_region_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
) -> TerminalSize {
    TerminalSize {
        columns: geometry
            .columns
            .saturating_sub(u16::from(geometry_has_right_divider(geometry, geometries)))
            .max(1),
        rows: geometry
            .rows
            .saturating_sub(u16::from(geometry_has_bottom_divider(geometry, geometries)))
            .max(1),
    }
}

/// Returns the pane body size available after divider and frame reservations.
pub fn pane_content_size_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    pane_frames_enabled: bool,
    pane_frame_position: TerminalFramePosition,
) -> TerminalSize {
    let render_region = pane_render_region_size_for_geometry(geometry, geometries);
    let frame_rows = if pane_frames_enabled
        && !pane_frame_merges_into_divider(geometry, geometries, pane_frame_position)
    {
        1
    } else {
        0
    };
    TerminalSize {
        columns: render_region.columns,
        rows: render_region.rows.saturating_sub(frame_rows).max(1),
    }
}

/// Returns the rendered terminal row occupied by one pane frame.
///
/// Frames that merge into a shared horizontal divider use that divider row;
/// standalone frames use the pane render region's top or bottom edge.
pub fn pane_frame_row_for_geometry(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    position: TerminalFramePosition,
    row_offset: u16,
) -> u16 {
    if pane_frame_merges_into_divider(geometry, geometries, position) {
        return row_offset.saturating_add(match position {
            TerminalFramePosition::Top => geometry.row.saturating_sub(1),
            TerminalFramePosition::Bottom => {
                geometry.row.saturating_add(geometry.rows).saturating_sub(1)
            }
        });
    }
    row_offset.saturating_add(match position {
        TerminalFramePosition::Top => geometry.row,
        TerminalFramePosition::Bottom => {
            let render_rows = pane_render_region_size_for_geometry(geometry, geometries).rows;
            geometry.row.saturating_add(render_rows).saturating_sub(1)
        }
    })
}

/// Bounded destination for one pane frame rendered on a shared divider row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedPaneFramePlacement {
    /// Pane index in the owning window.
    pub pane_index: usize,
    /// Destination row in the window body canvas.
    pub row: usize,
    /// First destination column in the window body canvas.
    pub column_start: usize,
    /// Maximum number of terminal cells available to the frame.
    pub width: usize,
}

/// Computes pane-frame placements that occupy shared horizontal dividers.
pub fn merged_pane_frame_placements(
    geometries: &[PaneGeometry],
    position: TerminalFramePosition,
) -> Vec<MergedPaneFramePlacement> {
    geometries
        .iter()
        .filter(|geometry| pane_frame_merges_into_divider(geometry, geometries, position))
        .map(|geometry| MergedPaneFramePlacement {
            pane_index: geometry.index,
            row: usize::from(pane_frame_row_for_geometry(
                geometry, geometries, position, 0,
            )),
            column_start: usize::from(geometry.column),
            width: usize::from(pane_render_region_size_for_geometry(geometry, geometries).columns),
        })
        .collect()
}

/// Bounded destination for one rendered pane inside a window-body canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneCanvasPlacement {
    /// Index of the pane's geometry in the source geometry slice.
    pub source_index: usize,
    /// First destination row in the window body canvas.
    pub row_start: usize,
    /// First destination column in the window body canvas.
    pub column_start: usize,
    /// Number of pane rows visible in the destination canvas.
    pub pane_rows: usize,
    /// Number of pane columns visible in the destination canvas.
    pub pane_columns: usize,
}

/// Computes pane-to-canvas placements with divider reservations and clipping.
pub fn pane_canvas_placements(
    size: TerminalSize,
    geometries: &[PaneGeometry],
) -> Vec<PaneCanvasPlacement> {
    let rows = usize::from(size.rows);
    let columns = usize::from(size.columns);
    let mut placements = Vec::with_capacity(geometries.len());
    for (source_index, geometry) in geometries.iter().enumerate() {
        let row_start = usize::from(geometry.row);
        let column_start = usize::from(geometry.column);
        if row_start >= rows || column_start >= columns {
            continue;
        }
        let region_size = pane_render_region_size_for_geometry(geometry, geometries);
        placements.push(PaneCanvasPlacement {
            source_index,
            row_start,
            column_start,
            pane_rows: usize::from(region_size.rows).min(rows.saturating_sub(row_start)),
            pane_columns: usize::from(region_size.columns)
                .min(columns.saturating_sub(column_start)),
        });
    }
    placements
}

/// Returns whether a pane frame should occupy an adjacent shared divider row.
///
/// Top frames merge into a horizontal divider immediately above the pane;
/// bottom frames merge into one immediately below it. Keeping this geometry
/// decision in the mux lets product renderers consume the result without
/// owning split-layout policy.
pub fn pane_frame_merges_into_divider(
    geometry: &PaneGeometry,
    geometries: &[PaneGeometry],
    frame_position: TerminalFramePosition,
) -> bool {
    geometries.iter().any(|candidate| {
        if candidate.index == geometry.index {
            return false;
        }
        let shares_boundary = match frame_position {
            TerminalFramePosition::Top => {
                candidate.row.saturating_add(candidate.rows) == geometry.row
            }
            TerminalFramePosition::Bottom => {
                geometry.row.saturating_add(geometry.rows) == candidate.row
            }
        };
        shares_boundary
            && range_overlap_u16(
                geometry.column,
                geometry.column.saturating_add(geometry.columns),
                candidate.column,
                candidate.column.saturating_add(candidate.columns),
            ) > 0
    })
}

/// One mux-managed pane-divider cell and its box-drawing glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneDividerCell {
    /// Zero-based terminal column occupied by the divider.
    pub column: u16,
    /// Zero-based terminal row occupied by the divider.
    pub row: u16,
    /// Thin box-drawing glyph selected from neighboring divider strokes.
    pub glyph: char,
}

/// Directional strokes that meet in one mux-managed pane-divider cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PaneDividerConnections {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    vertical: bool,
    horizontal: bool,
}

impl PaneDividerConnections {
    fn add_vertical(&mut self, up: bool, down: bool) {
        self.vertical = true;
        self.up |= up;
        self.down |= down;
    }

    fn add_horizontal(&mut self, left: bool, right: bool) {
        self.horizontal = true;
        self.left |= left;
        self.right |= right;
    }

    fn has_vertical(&self) -> bool {
        self.vertical
    }

    fn has_horizontal(&self) -> bool {
        self.horizontal
    }

    fn glyph(self) -> char {
        pane_divider_glyph(self.up, self.down, self.left, self.right)
    }
}

/// Selects the thin box-drawing glyph for explicit divider connections.
pub fn pane_divider_glyph(mut up: bool, mut down: bool, mut left: bool, mut right: bool) -> char {
    if !up && !down && !left && !right {
        return ' ';
    }
    if !up && !down {
        left = true;
        right = true;
    }
    if !left && !right {
        up = true;
        down = true;
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

/// Composes mux-managed divider cells from neighboring pane geometry.
pub fn pane_divider_cells(
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
                    cells
                        .entry((boundary, row))
                        .or_insert_with(PaneDividerConnections::default)
                        .add_vertical(row > start, row.saturating_add(1) < end);
                }
            }
            if include_horizontal && (first_bottom == second.row || second_bottom == first.row) {
                let boundary = first_bottom.min(second_bottom).saturating_sub(1);
                let start = first.column.max(second.column);
                let end = first_right.min(second_right);
                for column in start..end {
                    cells
                        .entry((column, boundary))
                        .or_insert_with(PaneDividerConnections::default)
                        .add_horizontal(column > start, column.saturating_add(1) < end);
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

fn connect_touching_divider_cells(cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>) {
    let snapshot = cells.clone();
    for (&(column, row), connections) in &snapshot {
        if connections.has_vertical() {
            if let Some(below_row) = row.checked_add(1) {
                connect_vertical_neighbors(cells, &snapshot, (column, row), (column, below_row));
            }
            if column > 0 {
                connect_crossing_neighbors(
                    cells,
                    &snapshot,
                    (column, row),
                    (column.saturating_sub(1), row),
                    true,
                );
            }
            if let Some(right_column) = column.checked_add(1) {
                connect_crossing_neighbors(
                    cells,
                    &snapshot,
                    (column, row),
                    (right_column, row),
                    false,
                );
            }
        }
        if connections.has_horizontal() {
            if let Some(right_column) = column.checked_add(1) {
                connect_horizontal_neighbors(cells, &snapshot, (column, row), (right_column, row));
            }
            if row > 0 {
                connect_vertical_crossing(
                    cells,
                    &snapshot,
                    (column, row),
                    (column, row.saturating_sub(1)),
                    true,
                );
            }
            if let Some(below_row) = row.checked_add(1) {
                connect_vertical_crossing(
                    cells,
                    &snapshot,
                    (column, row),
                    (column, below_row),
                    false,
                );
            }
        }
    }
}

fn connect_vertical_neighbors(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    below: (u16, u16),
) {
    if snapshot
        .get(&below)
        .is_some_and(PaneDividerConnections::has_vertical)
    {
        cells.get_mut(&current).expect("current divider cell").down = true;
        cells.get_mut(&below).expect("neighbor divider cell").up = true;
    }
}

fn connect_horizontal_neighbors(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    right: (u16, u16),
) {
    if snapshot
        .get(&right)
        .is_some_and(PaneDividerConnections::has_horizontal)
    {
        cells.get_mut(&current).expect("current divider cell").right = true;
        cells.get_mut(&right).expect("neighbor divider cell").left = true;
    }
}

fn connect_crossing_neighbors(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    horizontal: (u16, u16),
    is_left: bool,
) {
    if snapshot
        .get(&horizontal)
        .is_some_and(PaneDividerConnections::has_horizontal)
    {
        if is_left {
            cells.get_mut(&current).expect("current divider cell").left = true;
            cells
                .get_mut(&horizontal)
                .expect("neighbor divider cell")
                .right = true;
        } else {
            cells.get_mut(&current).expect("current divider cell").right = true;
            cells
                .get_mut(&horizontal)
                .expect("neighbor divider cell")
                .left = true;
        }
    }
}

fn connect_vertical_crossing(
    cells: &mut BTreeMap<(u16, u16), PaneDividerConnections>,
    snapshot: &BTreeMap<(u16, u16), PaneDividerConnections>,
    current: (u16, u16),
    vertical: (u16, u16),
    is_above: bool,
) {
    if snapshot
        .get(&vertical)
        .is_some_and(PaneDividerConnections::has_vertical)
    {
        if is_above {
            cells.get_mut(&current).expect("current divider cell").up = true;
            cells
                .get_mut(&vertical)
                .expect("neighbor divider cell")
                .down = true;
        } else {
            cells.get_mut(&current).expect("current divider cell").down = true;
            cells.get_mut(&vertical).expect("neighbor divider cell").up = true;
        }
    }
}
