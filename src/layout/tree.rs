//! First-class split-tree state for window layouts.
//!
//! The tree records the ancestry of pane splits independently from the flat pane
//! vector. Pane slots remain indexed by `Pane.index`; operations that move
//! process identity between slots keep the same tree, while operations that add
//! or remove slots update the tree shape.

use super::{
    MIN_PANE_COLUMNS, MIN_PANE_ROWS, MezError, Pane, PaneGeometry, Result, Size, SplitDirection,
};

/// Recursive layout node stored by each window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutNode {
    /// A leaf pane slot in the owning window's pane vector.
    Pane {
        /// Current pane slot index represented by this leaf.
        index: usize,
    },
    /// A split containing child nodes in visual order.
    Split {
        /// Direction used to place children.
        direction: SplitDirection,
        /// Child nodes created by split operations.
        children: Vec<LayoutNode>,
    },
}

impl LayoutNode {
    /// Creates a single-pane layout node for the supplied pane slot.
    pub fn single_pane(index: usize) -> Self {
        Self::Pane { index }
    }

    /// Creates a flat layout node for all pane slots in a window.
    pub fn flat(direction: SplitDirection, pane_count: usize) -> Self {
        if pane_count <= 1 {
            return Self::single_pane(0);
        }
        Self::Split {
            direction,
            children: (0..pane_count).map(Self::single_pane).collect(),
        }
    }

    /// Returns the leaf pane slot index when this node is a pane.
    pub fn pane_index(&self) -> Option<usize> {
        match self {
            Self::Pane { index } => Some(*index),
            Self::Split { .. } => None,
        }
    }

    /// Returns the split direction when this node is a split.
    pub fn direction(&self) -> Option<SplitDirection> {
        match self {
            Self::Pane { .. } => None,
            Self::Split { direction, .. } => Some(*direction),
        }
    }

    /// Returns child nodes for a split, or an empty slice for a pane.
    pub fn children(&self) -> &[LayoutNode] {
        match self {
            Self::Pane { .. } => &[],
            Self::Split { children, .. } => children,
        }
    }

    /// Splits an existing pane slot and shifts later slots for the inserted pane.
    pub(super) fn split_pane(
        &mut self,
        target_index: usize,
        inserted_index: usize,
        direction: SplitDirection,
    ) -> Result<()> {
        if self.split_pane_inner(target_index, inserted_index, direction) {
            Ok(())
        } else {
            Err(MezError::invalid_state(
                "layout tree does not contain target pane",
            ))
        }
    }

    /// Removes a pane slot and shifts later slots down.
    pub(super) fn remove_pane(
        &mut self,
        removed_index: usize,
        removed_size: Size,
        panes: &mut [Pane],
    ) -> bool {
        let (found, remove_self) = self.remove_pane_inner(removed_index, removed_size, panes);
        if remove_self {
            *self = Self::single_pane(0);
        }
        found
    }

    /// Returns pane rectangles by traversing the stored split ancestry.
    pub(super) fn pane_geometries(&self, panes: &[Pane]) -> Vec<PaneGeometry> {
        let mut geometries = Vec::with_capacity(panes.len());
        self.collect_geometries(0, 0, panes, &mut geometries);
        geometries.sort_by_key(|geometry| geometry.index);
        geometries
    }

    /// Returns the smallest rectangle that can contain every pane in this tree.
    pub(super) fn minimum_size(&self) -> Size {
        match self {
            Self::Pane { .. } => Size {
                columns: MIN_PANE_COLUMNS,
                rows: MIN_PANE_ROWS,
            },
            Self::Split {
                direction,
                children,
            } => {
                let child_sizes = children.iter().map(Self::minimum_size).collect::<Vec<_>>();
                match direction {
                    SplitDirection::Vertical => Size {
                        columns: child_sizes.iter().map(|size| size.columns).sum(),
                        rows: child_sizes.iter().map(|size| size.rows).max().unwrap_or(1),
                    },
                    SplitDirection::Horizontal => Size {
                        columns: child_sizes
                            .iter()
                            .map(|size| size.columns)
                            .max()
                            .unwrap_or(1),
                        rows: child_sizes.iter().map(|size| size.rows).sum(),
                    },
                }
            }
        }
    }

    /// Returns the logical node size derived from pane sizes below this node.
    pub fn logical_size(&self, panes: &[Pane]) -> Size {
        match self {
            Self::Pane { index } => panes.get(*index).map(|pane| pane.size).unwrap_or(Size {
                columns: 0,
                rows: 0,
            }),
            Self::Split {
                direction,
                children,
            } => {
                let child_sizes = children
                    .iter()
                    .map(|child| child.logical_size(panes))
                    .collect::<Vec<_>>();
                match direction {
                    SplitDirection::Vertical => Size {
                        columns: child_sizes.iter().map(|size| size.columns).sum(),
                        rows: child_sizes.iter().map(|size| size.rows).max().unwrap_or(0),
                    },
                    SplitDirection::Horizontal => Size {
                        columns: child_sizes
                            .iter()
                            .map(|size| size.columns)
                            .max()
                            .unwrap_or(0),
                        rows: child_sizes.iter().map(|size| size.rows).sum(),
                    },
                }
            }
        }
    }

    /// Returns a split child's allocation on the supplied split axis.
    pub fn allocation_on_axis(&self, panes: &[Pane], direction: SplitDirection) -> u16 {
        let size = self.logical_size(panes);
        match direction {
            SplitDirection::Vertical => size.columns,
            SplitDirection::Horizontal => size.rows,
        }
    }

    /// Reapportions pane sizes under this node to fit a new rectangle.
    pub(super) fn resize_panes(&self, panes: &mut [Pane], size: Size) {
        match self {
            Self::Pane { index } => {
                if let Some(pane) = panes.get_mut(*index) {
                    pane.size = size;
                }
            }
            Self::Split {
                direction,
                children,
            } => {
                let weights = children
                    .iter()
                    .map(|child| child.allocation_on_axis(panes, *direction))
                    .collect::<Vec<_>>();
                let allocations = match direction {
                    SplitDirection::Vertical => distribute_dimension(size.columns, &weights),
                    SplitDirection::Horizontal => distribute_dimension(size.rows, &weights),
                };
                for (child, allocation) in children.iter().zip(allocations) {
                    let child_size = match direction {
                        SplitDirection::Vertical => Size {
                            columns: allocation,
                            rows: size.rows,
                        },
                        SplitDirection::Horizontal => Size {
                            columns: size.columns,
                            rows: allocation,
                        },
                    };
                    child.resize_panes(panes, child_size);
                }
            }
        }
    }

    /// Reconstructs a split tree from complete pane rectangle metadata.
    pub(super) fn from_geometries(geometries: &[PaneGeometry]) -> Result<Self> {
        if geometries.is_empty() {
            return Err(MezError::invalid_args(
                "layout tree requires at least one pane geometry",
            ));
        }
        let mut nodes = geometries
            .iter()
            .copied()
            .map(LayoutGeometryNode::from)
            .collect::<Vec<_>>();
        nodes.sort_by_key(|node| node.index);
        let bounds = LayoutBounds::covering(&nodes);
        Ok(layout_node_from_geometries(&nodes, bounds))
    }

    /// Verifies that this tree references each pane slot exactly once.
    pub fn validate_pane_indices(&self, pane_count: usize) -> Result<()> {
        let mut seen = vec![false; pane_count];
        self.collect_pane_indices(pane_count, &mut seen)?;
        if seen.iter().any(|present| !present) {
            return Err(MezError::invalid_args(
                "layout tree must reference every pane exactly once",
            ));
        }
        Ok(())
    }

    /// Runs the split pane inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn split_pane_inner(
        &mut self,
        target_index: usize,
        inserted_index: usize,
        direction: SplitDirection,
    ) -> bool {
        match self {
            Self::Pane { index } if *index == target_index => {
                *self = Self::Split {
                    direction,
                    children: vec![
                        Self::single_pane(target_index),
                        Self::single_pane(inserted_index),
                    ],
                };
                true
            }
            Self::Pane { index } => {
                if *index >= inserted_index {
                    *index = index.saturating_add(1);
                }
                false
            }
            Self::Split { children, .. } => {
                let mut found = false;
                for child in children {
                    found |= child.split_pane_inner(target_index, inserted_index, direction);
                }
                found
            }
        }
    }

    /// Runs the remove pane inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn remove_pane_inner(
        &mut self,
        removed_index: usize,
        removed_size: Size,
        panes: &mut [Pane],
    ) -> (bool, bool) {
        match self {
            Self::Pane { index } if *index == removed_index => (true, true),
            Self::Pane { index } => {
                if *index > removed_index {
                    *index = index.saturating_sub(1);
                }
                (false, false)
            }
            Self::Split {
                direction,
                children,
            } => {
                let mut found = false;
                let mut child_index = 0;
                let mut expansion_target = None;
                while child_index < children.len() {
                    let (child_found, remove_child) =
                        children[child_index].remove_pane_inner(removed_index, removed_size, panes);
                    found |= child_found;
                    if remove_child {
                        children.remove(child_index);
                        if !children.is_empty() {
                            expansion_target = Some(if child_index > 0 {
                                child_index.saturating_sub(1)
                            } else {
                                0
                            });
                        }
                    } else {
                        child_index += 1;
                    }
                }
                if let Some(target) = expansion_target
                    && let Some(child) = children.get(target.min(children.len().saturating_sub(1)))
                {
                    let amount = match direction {
                        SplitDirection::Vertical => removed_size.columns,
                        SplitDirection::Horizontal => removed_size.rows,
                    };
                    child.expand_on_axis(panes, *direction, amount);
                }
                if children.is_empty() {
                    return (found, true);
                }
                if children.len() == 1 {
                    let only_child = children.remove(0);
                    *self = only_child;
                }
                (found, false)
            }
        }
    }

    /// Runs the expand on axis operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn expand_on_axis(&self, panes: &mut [Pane], direction: SplitDirection, amount: u16) {
        let mut size = self.logical_size(panes);
        match direction {
            SplitDirection::Vertical => {
                size.columns = size.columns.saturating_add(amount);
            }
            SplitDirection::Horizontal => {
                size.rows = size.rows.saturating_add(amount);
            }
        }
        self.resize_panes(panes, size);
    }

    /// Runs the collect geometries operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn collect_geometries(
        &self,
        column: u16,
        row: u16,
        panes: &[Pane],
        geometries: &mut Vec<PaneGeometry>,
    ) {
        match self {
            Self::Pane { index } => {
                if let Some(pane) = panes.get(*index) {
                    geometries.push(PaneGeometry {
                        index: *index,
                        column,
                        row,
                        columns: pane.size.columns,
                        rows: pane.size.rows,
                    });
                }
            }
            Self::Split {
                direction,
                children,
            } => {
                let mut next_column = column;
                let mut next_row = row;
                for child in children {
                    child.collect_geometries(next_column, next_row, panes, geometries);
                    let child_size = child.logical_size(panes);
                    match direction {
                        SplitDirection::Vertical => {
                            next_column = next_column.saturating_add(child_size.columns);
                        }
                        SplitDirection::Horizontal => {
                            next_row = next_row.saturating_add(child_size.rows);
                        }
                    }
                }
            }
        }
    }

    /// Runs the collect pane indices operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn collect_pane_indices(&self, pane_count: usize, seen: &mut [bool]) -> Result<()> {
        match self {
            Self::Pane { index } => {
                let Some(slot) = seen.get_mut(*index) else {
                    return Err(MezError::invalid_args(
                        "layout tree references an unknown pane index",
                    ));
                };
                if *slot {
                    return Err(MezError::invalid_args(
                        "layout tree references a pane index more than once",
                    ));
                }
                *slot = true;
                if pane_count == 0 {
                    return Err(MezError::invalid_args(
                        "layout tree cannot reference panes in an empty window",
                    ));
                }
                Ok(())
            }
            Self::Split { children, .. } => {
                if children.len() < 2 {
                    return Err(MezError::invalid_args(
                        "layout split nodes must contain at least two children",
                    ));
                }
                for child in children {
                    child.collect_pane_indices(pane_count, seen)?;
                }
                Ok(())
            }
        }
    }
}

/// Runs the distribute dimension operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn distribute_dimension(total: u16, weights: &[u16]) -> Vec<u16> {
    if weights.is_empty() {
        return Vec::new();
    }
    if weights.len() == 1 {
        return vec![total.max(1)];
    }
    let total_weight = weights.iter().map(|weight| u32::from(*weight)).sum::<u32>();
    if total_weight == 0 {
        return super::split_dimension_evenly(total, weights.len());
    }

    let mut allocations = weights
        .iter()
        .map(|weight| {
            let scaled = u32::from(total).saturating_mul(u32::from(*weight)) / total_weight;
            u16::try_from(scaled.max(1)).unwrap_or(u16::MAX)
        })
        .collect::<Vec<_>>();
    let allocated = allocations
        .iter()
        .map(|allocation| u32::from(*allocation))
        .sum::<u32>();
    let target = u32::from(total.max(1));

    if allocated < target {
        let mut remainder = target - allocated;
        let mut index = 0usize;
        while remainder > 0 {
            allocations[index] = allocations[index].saturating_add(1);
            remainder -= 1;
            index = (index + 1) % allocations.len();
        }
    } else if allocated > target {
        let mut excess = allocated - target;
        while excess > 0 {
            let Some(index) = allocations.iter().rposition(|allocation| *allocation > 1) else {
                break;
            };
            allocations[index] -= 1;
            excess -= 1;
        }
    }

    allocations
}

/// Carries Layout Geometry Node state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayoutGeometryNode {
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    index: usize,
    /// Stores the column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    column: u16,
    /// Stores the row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    row: u16,
    /// Stores the columns value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    columns: u16,
    /// Stores the rows value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    rows: u16,
}

impl From<PaneGeometry> for LayoutGeometryNode {
    /// Runs the from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from(geometry: PaneGeometry) -> Self {
        Self {
            index: geometry.index,
            column: geometry.column,
            row: geometry.row,
            columns: geometry.columns,
            rows: geometry.rows,
        }
    }
}

impl LayoutGeometryNode {
    /// Runs the right operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn right(self) -> u16 {
        self.column.saturating_add(self.columns)
    }

    /// Runs the bottom operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn bottom(self) -> u16 {
        self.row.saturating_add(self.rows)
    }
}

/// Carries Layout Bounds state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy)]
struct LayoutBounds {
    /// Stores the column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    column: u16,
    /// Stores the row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    row: u16,
    /// Stores the columns value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    columns: u16,
    /// Stores the rows value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    rows: u16,
}

impl LayoutBounds {
    /// Runs the covering operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn covering(nodes: &[LayoutGeometryNode]) -> Self {
        let left = nodes.iter().map(|node| node.column).min().unwrap_or(0);
        let top = nodes.iter().map(|node| node.row).min().unwrap_or(0);
        let right = nodes.iter().map(|node| node.right()).max().unwrap_or(left);
        let bottom = nodes.iter().map(|node| node.bottom()).max().unwrap_or(top);
        Self {
            column: left,
            row: top,
            columns: right.saturating_sub(left),
            rows: bottom.saturating_sub(top),
        }
    }

    /// Runs the right operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn right(self) -> u16 {
        self.column.saturating_add(self.columns)
    }

    /// Runs the bottom operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn bottom(self) -> u16 {
        self.row.saturating_add(self.rows)
    }
}

/// Carries Layout Partition Direction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy)]
enum LayoutPartitionDirection {
    /// Represents the Vertical case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Vertical,
    /// Represents the Horizontal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Horizontal,
}

impl LayoutPartitionDirection {
    /// Runs the start operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn start(self, bounds: LayoutBounds) -> u16 {
        match self {
            Self::Vertical => bounds.column,
            Self::Horizontal => bounds.row,
        }
    }

    /// Runs the end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn end(self, bounds: LayoutBounds) -> u16 {
        match self {
            Self::Vertical => bounds.right(),
            Self::Horizontal => bounds.bottom(),
        }
    }
}

/// Runs the layout node from geometries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn layout_node_from_geometries(nodes: &[LayoutGeometryNode], bounds: LayoutBounds) -> LayoutNode {
    if nodes.len() == 1 {
        return LayoutNode::single_pane(nodes[0].index);
    }

    if let Some((left, right, left_bounds, right_bounds)) =
        layout_partition_at_clean_cut(nodes, bounds, LayoutPartitionDirection::Vertical)
    {
        return LayoutNode::Split {
            direction: SplitDirection::Vertical,
            children: vec![
                layout_node_from_geometries(&left, left_bounds),
                layout_node_from_geometries(&right, right_bounds),
            ],
        };
    }
    if let Some((top, bottom, top_bounds, bottom_bounds)) =
        layout_partition_at_clean_cut(nodes, bounds, LayoutPartitionDirection::Horizontal)
    {
        return LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![
                layout_node_from_geometries(&top, top_bounds),
                layout_node_from_geometries(&bottom, bottom_bounds),
            ],
        };
    }

    let direction = if nodes
        .first()
        .is_some_and(|first| nodes.iter().all(|node| node.rows == first.rows))
    {
        SplitDirection::Vertical
    } else {
        SplitDirection::Horizontal
    };
    LayoutNode::Split {
        direction,
        children: nodes
            .iter()
            .map(|node| LayoutNode::single_pane(node.index))
            .collect(),
    }
}

/// Runs the layout partition at clean cut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn layout_partition_at_clean_cut(
    nodes: &[LayoutGeometryNode],
    bounds: LayoutBounds,
    direction: LayoutPartitionDirection,
) -> Option<(
    Vec<LayoutGeometryNode>,
    Vec<LayoutGeometryNode>,
    LayoutBounds,
    LayoutBounds,
)> {
    let mut cuts = nodes
        .iter()
        .flat_map(|node| match direction {
            LayoutPartitionDirection::Vertical => [node.column, node.right()],
            LayoutPartitionDirection::Horizontal => [node.row, node.bottom()],
        })
        .filter(|cut| *cut > direction.start(bounds) && *cut < direction.end(bounds))
        .collect::<Vec<_>>();
    cuts.sort_unstable();
    cuts.dedup();

    for cut in cuts {
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut crossed = false;
        for node in nodes {
            match direction {
                LayoutPartitionDirection::Vertical => {
                    if node.right() <= cut {
                        before.push(*node);
                    } else if node.column >= cut {
                        after.push(*node);
                    } else {
                        crossed = true;
                        break;
                    }
                }
                LayoutPartitionDirection::Horizontal => {
                    if node.bottom() <= cut {
                        before.push(*node);
                    } else if node.row >= cut {
                        after.push(*node);
                    } else {
                        crossed = true;
                        break;
                    }
                }
            }
        }
        if !crossed && !before.is_empty() && !after.is_empty() {
            return Some((
                before.clone(),
                after.clone(),
                LayoutBounds::covering(&before),
                LayoutBounds::covering(&after),
            ));
        }
    }

    None
}
