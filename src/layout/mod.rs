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

use sizing::{even_grid_dimensions, split_dimension_evenly, split_size_with_spec};
use targeting::pane_matches_target;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
