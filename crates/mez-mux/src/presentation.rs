//! Dependency-neutral multiplexer presentation contracts.
//!
//! This module owns small value types used to compose terminal surfaces into
//! pane and window presentation. Product configuration and agent-specific
//! frame metadata remain in the Mezzanine composition crate.

use std::collections::BTreeMap;

use crate::layout::{PaneGeometry, range_overlap_u16};

/// Per-pane metadata consumed by mux frame and body presentation.
///
/// Scalar fields are presentation-only values. The prompt and supplemental
/// body-line types remain generic so the product can supply richer UI state
/// without making the mux depend on product-owned prompt or agent types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPaneFrameContext<Prompt = (), DisplayLines = Vec<String>> {
    /// Primary process id shown by `pane.primary_pid`.
    pub primary_pid: Option<u32>,
    /// Primary process name shown by `pane.process_name` when known.
    pub process_name: Option<String>,
    /// Primary process exit status shown by `pane.exit_status` when known.
    pub exit_status: Option<String>,
    /// Home-relative current working directory shown by `pane.pwd` when known.
    pub current_working_directory: Option<String>,
    /// Current pane interaction mode shown by `pane.mode`.
    pub mode: Option<String>,
    /// Opaque agent identity shown by `agent.id`.
    pub agent_id: Option<String>,
    /// Human-readable agent label shown by `agent.name`.
    pub agent_name: Option<String>,
    /// Opaque agent state shown by `agent.status`.
    pub agent_status: Option<String>,
    /// Active model label shown by `agent.model`.
    pub agent_model: Option<String>,
    /// Active reasoning label shown by `agent.reasoning`.
    pub agent_reasoning: Option<String>,
    /// Provider thinking-mode label shown by `agent.thinking`.
    pub agent_thinking: Option<String>,
    /// Pane-local routing label shown by `agent.routing`.
    pub agent_routing: Option<String>,
    /// Active latency label shown by `agent.latency`.
    pub agent_latency: Option<String>,
    /// Active model preset label shown by `agent.preset`.
    pub agent_preset: Option<String>,
    /// Last known input-context usage shown by `agent.context_usage`.
    pub agent_context_usage: Option<String>,
    /// Scrollback position shown by `history.position` when not at the live bottom.
    pub history_position: Option<String>,
    /// Product-owned prompt state rendered inside the pane body.
    pub agent_prompt: Option<Prompt>,
    /// Product-owned supplemental lines rendered above the prompt.
    pub agent_display_lines: DisplayLines,
}

impl<Prompt, DisplayLines: Default> Default for TerminalPaneFrameContext<Prompt, DisplayLines> {
    fn default() -> Self {
        Self {
            primary_pid: None,
            process_name: None,
            exit_status: None,
            current_working_directory: None,
            mode: None,
            agent_id: None,
            agent_name: None,
            agent_status: None,
            agent_model: None,
            agent_reasoning: None,
            agent_thinking: None,
            agent_routing: None,
            agent_latency: None,
            agent_preset: None,
            agent_context_usage: None,
            history_position: None,
            agent_prompt: None,
            agent_display_lines: DisplayLines::default(),
        }
    }
}

/// Runtime fields available to the right side of a window status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowStatusContext {
    /// Named-field template for the right status line.
    pub template: String,
    /// Home-relative active pane working directory shown by `pane.pwd`.
    pub active_pane_working_directory: Option<String>,
    /// Cached command-backed pill values keyed by configured pill name.
    pub status_pills: BTreeMap<String, String>,
    /// Human-readable system uptime shown by `system.uptime`.
    pub system_uptime: String,
    /// Human-readable local datetime shown by `datetime.local`.
    pub datetime_local: String,
}

/// Runtime window metadata made available to default window-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowFrameContext {
    /// Stable window identity.
    pub id: String,
    /// Display index in the session window list.
    pub index: usize,
    /// User-facing title or name for the window.
    pub title: String,
    /// Whether this window is currently focused.
    pub active: bool,
    /// Whether this window is dedicated to spawned subagent panes.
    pub subagent: bool,
}

/// Runtime window-group metadata made available to group-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowGroupFrameContext {
    /// Stable group identity.
    pub id: String,
    /// Display index in the session group list.
    pub index: usize,
    /// User-facing title or name for the group.
    pub title: String,
    /// Whether this group is currently focused.
    pub active: bool,
}

/// Placement of a one-row terminal frame within its owning region.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFramePosition {
    /// Render the frame before region body content.
    #[default]
    Top,
    /// Render the frame after region body content.
    Bottom,
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

/// Style applied to a rendered frame row when styled terminal output is used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFrameStyle {
    /// Leave frame text unstyled.
    #[default]
    Default,
    /// Render the frame with bold/intense text.
    Bold,
    /// Render the frame with underline text.
    Underline,
    /// Render the frame with inverse video.
    Inverse,
}

#[cfg(test)]
mod tests {
    use crate::layout::PaneGeometry;

    use super::{
        TerminalFramePosition, TerminalFrameStyle, pane_divider_cells, pane_divider_glyph,
        pane_frame_merges_into_divider,
    };

    /// Verifies neutral frame contracts retain the product's established
    /// top-positioned and unstyled defaults after ownership moves to the mux.
    #[test]
    fn frame_contract_defaults_remain_stable() {
        assert_eq!(TerminalFramePosition::default(), TerminalFramePosition::Top);
        assert_eq!(TerminalFrameStyle::default(), TerminalFrameStyle::Default);
    }

    /// Verifies pane-frame placement consumes shared split geometry without
    /// depending on product rendering, prompt state, or agent metadata.
    #[test]
    fn pane_frames_merge_only_with_adjacent_horizontal_dividers() {
        let top = PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 20,
            rows: 5,
        };
        let bottom = PaneGeometry {
            index: 1,
            column: 0,
            row: 5,
            columns: 20,
            rows: 5,
        };
        let side = PaneGeometry {
            index: 2,
            column: 20,
            row: 0,
            columns: 20,
            rows: 10,
        };
        let geometries = [top, bottom, side];

        assert!(pane_frame_merges_into_divider(
            &bottom,
            &geometries,
            TerminalFramePosition::Top,
        ));
        assert!(pane_frame_merges_into_divider(
            &top,
            &geometries,
            TerminalFramePosition::Bottom,
        ));
        assert!(!pane_frame_merges_into_divider(
            &top,
            &geometries,
            TerminalFramePosition::Top,
        ));
    }

    /// Verifies mux divider composition selects stable line, corner, and
    /// junction glyphs without relying on product canvases or mouse types.
    #[test]
    fn pane_divider_glyphs_match_connection_shapes() {
        assert_eq!(pane_divider_glyph(true, true, false, false), '│');
        assert_eq!(pane_divider_glyph(false, false, true, true), '─');
        assert_eq!(pane_divider_glyph(false, true, false, true), '┌');
        assert_eq!(pane_divider_glyph(true, true, true, true), '┼');

        let geometries = [
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 10,
                rows: 5,
            },
            PaneGeometry {
                index: 1,
                column: 10,
                row: 0,
                columns: 10,
                rows: 5,
            },
        ];
        let cells = pane_divider_cells(&geometries, true);
        assert_eq!(cells.len(), 5);
        assert!(cells.iter().all(|cell| cell.column == 9));
    }
}
