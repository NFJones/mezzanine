//! In-memory window and pane layout model.
//!
//! This module models split sizing, active pane tracking, and stable pane/window
//! identity independently from terminal rendering and PTY resize propagation.

use crate::error::{MezError, Result};
use crate::ids::{IdFactory, PaneId, WindowId};

/// Exposes the sizing module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod sizing;
/// Exposes the targeting module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod targeting;
/// Exposes the tree module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod tree;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;
/// Exposes the window module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod window;

pub use sizing::split_size;
pub(crate) use sizing::{
    EVEN_GRID_TARGET_COLUMNS, EVEN_GRID_TARGET_ROWS, even_layout_minimum_pane_size,
};
pub use tree::LayoutNode;
pub(crate) use types::RestoredWindowLayout;
pub use types::{
    LayoutPolicy, MIN_PANE_COLUMNS, MIN_PANE_ROWS, Pane, PaneGeometry, PaneNavigationDirection,
    PaneSizeSpec, PaneTitleSource, ResizeAxis, ResizeDirection, Size, SplitDirection, Window,
    WindowNameSource,
};

/// Returns the overlap length between two half-open `u16` ranges.
///
/// The ranges are interpreted as `[start, end)`. Empty, reversed, or disjoint
/// ranges return zero, matching pane-geometry callers that treat a zero overlap
/// as no shared edge coverage.
pub(crate) fn range_overlap_u16(
    first_start: u16,
    first_end: u16,
    second_start: u16,
    second_end: u16,
) -> u16 {
    first_end
        .min(second_end)
        .saturating_sub(first_start.max(second_start))
}

use sizing::{
    even_grid_dimensions, percent_size_for_axis, split_dimension_evenly, split_size_with_spec,
};
use targeting::pane_matches_target;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
