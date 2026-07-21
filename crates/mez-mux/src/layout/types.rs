//! Layout data types and invariants.
//!
//! These structures describe windows, panes, dimensions, and policies. Mutation
//! behavior lives in sibling modules that preserve these invariants.

use super::{LayoutNode, PaneId, Size, WindowId};

/// Defines the MIN PANE COLUMNS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MIN_PANE_COLUMNS: u16 = 2;
/// Defines the MIN PANE ROWS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MIN_PANE_ROWS: u16 = 2;

/// Stored pane rectangle within a window grid.
///
/// Windows keep this rectangle state aligned with pane order and pane sizes so
/// navigation, rendering metadata, and snapshots can read geometry from the
/// layout model instead of reconstructing it at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGeometry {
    /// Pane index in the owning window.
    pub index: usize,
    /// Zero-based column where the pane starts.
    pub column: u16,
    /// Zero-based row where the pane starts.
    pub row: u16,
    /// Pane width in terminal cells.
    pub columns: u16,
    /// Pane height in terminal cells.
    pub rows: u16,
}

/// Carries Split Direction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
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

impl SplitDirection {
    /// Returns the stable protocol and snapshot name for this split direction.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
        }
    }

    /// Parses a stable protocol and snapshot split direction name.
    ///
    /// Returns `None` when the supplied name is not a known split direction.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "vertical" => Some(Self::Vertical),
            "horizontal" => Some(Self::Horizontal),
            _ => None,
        }
    }
}

/// Carries Pane Navigation Direction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneNavigationDirection {
    /// Represents the Up case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Up,
    /// Represents the Down case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Down,
    /// Represents the Left case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Left,
    /// Represents the Right case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Right,
}

/// Axis affected by a percentage pane resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeAxis {
    /// Resize only columns.
    Columns,
    /// Resize only rows.
    Rows,
    /// Resize both columns and rows.
    Both,
}

/// Direction or edge used by delta and edge pane resizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDirection {
    /// Leftward movement decreases columns.
    Left,
    /// Rightward movement increases columns.
    Right,
    /// Upward movement decreases rows.
    Up,
    /// Downward movement increases rows.
    Down,
}

impl ResizeDirection {
    /// Parses a protocol or command resize direction name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "left" | "L" => Some(Self::Left),
            "right" | "R" => Some(Self::Right),
            "up" | "top" | "U" => Some(Self::Up),
            "down" | "bottom" | "D" => Some(Self::Down),
            _ => None,
        }
    }
}

/// Spec-defined pane size request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSizeSpec {
    /// Absolute cell dimensions; omitted axes keep their current size.
    Cells {
        /// Requested pane columns.
        columns: Option<u16>,
        /// Requested pane rows.
        rows: Option<u16>,
    },
    /// Percentage of the owning window along one or both axes.
    Percent {
        /// Positive percentage to apply.
        percent: u16,
        /// Axis affected by the percentage.
        axis: ResizeAxis,
    },
    /// Relative movement from the current pane size.
    Delta {
        /// Direction that determines which axis grows or shrinks.
        direction: ResizeDirection,
        /// Positive cell amount to apply.
        amount: u16,
    },
    /// Directional edge movement from the current pane size.
    Edge {
        /// Edge to move.
        edge: ResizeDirection,
        /// Positive cell amount to apply.
        amount: u16,
    },
}

/// Carries Layout Policy state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutPolicy {
    /// Represents the Tiled case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Tiled,
    /// Represents the Even Vertical case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EvenVertical,
    /// Represents the Even Horizontal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EvenHorizontal,
    /// Represents the Even Grid case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EvenGrid,
}

impl LayoutPolicy {
    /// Returns the stable payload/config name for this layout policy.
    pub fn name(self) -> &'static str {
        match self {
            Self::Tiled => "tiled",
            Self::EvenVertical => "even-vertical",
            Self::EvenHorizontal => "even-horizontal",
            Self::EvenGrid => "even-grid",
        }
    }

    /// Parses a stable payload/config layout policy name.
    ///
    /// Returns `None` when the supplied name is not a known layout policy.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "tiled" => Some(Self::Tiled),
            "even-vertical" => Some(Self::EvenVertical),
            "even-horizontal" => Some(Self::EvenHorizontal),
            "even-grid" => Some(Self::EvenGrid),
            _ => None,
        }
    }

    /// Runs the next operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn next(self) -> Self {
        match self {
            Self::Tiled => Self::EvenVertical,
            Self::EvenVertical => Self::EvenHorizontal,
            Self::EvenHorizontal => Self::EvenGrid,
            Self::EvenGrid => Self::Tiled,
        }
    }
}

/// Carries Pane state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: PaneId,
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub index: usize,
    /// Stores the title value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub title: String,
    /// Describes whether automatic process metadata or program output owns the
    /// current title.
    pub title_source: PaneTitleSource,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub size: Size,
    /// Stores the active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub active: bool,
    /// Stores the live value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub live: bool,
}

/// Provenance for a pane title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneTitleSource {
    /// The default title assigned before process metadata is available.
    Default,
    /// A title discovered from terminal or foreground-process metadata.
    Automatic,
    /// A title explicitly emitted by the currently running foreground program.
    Program,
    /// A title explicitly assigned by a user or agent command.
    Explicit,
}

impl PaneTitleSource {
    /// Returns whether automatic terminal metadata must not replace this title.
    pub fn is_explicit(self) -> bool {
        matches!(self, Self::Explicit)
    }
}

/// Carries Window state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: WindowId,
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub index: usize,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Describes whether automatic runtime naming may replace the name.
    pub name_source: WindowNameSource,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: Option<u64>,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub size: Size,
    /// Stores the panes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) panes: Vec<Pane>,
    /// Stores the active pane index value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) active_pane_index: usize,
    /// Stores the last active pane index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) last_active_pane_index: Option<usize>,
    /// Bounded oldest-to-newest stable pane identities previously focused in
    /// this window. The history is transient and is never persisted.
    pub(super) pane_focus_history: Vec<PaneId>,
    /// Stores the zoomed pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) zoomed_pane_id: Option<PaneId>,
    /// Stores the layout policy value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) layout_policy: LayoutPolicy,
    /// Stores the layout root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) layout_root: LayoutNode,
    /// Stores the pane geometries value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_geometries: Vec<PaneGeometry>,
}

/// Provenance for a window name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowNameSource {
    /// A runtime-generated name that may be refreshed automatically.
    Generated,
    /// A name explicitly assigned by a user or agent command.
    Explicit,
}

impl WindowNameSource {
    /// Returns whether automatic runtime naming must not replace this name.
    pub fn is_explicit(self) -> bool {
        matches!(self, Self::Explicit)
    }
}

/// Snapshot-provided layout metadata used while restoring a window.
pub struct RestoredWindowLayout {
    /// Complete pane rectangles restored from snapshot metadata.
    pub pane_geometries: Option<Vec<PaneGeometry>>,
    /// Recursive split tree restored from snapshot metadata.
    pub layout_root: Option<LayoutNode>,
    /// Active layout policy restored for later layout cycling and mutation.
    pub layout_policy: LayoutPolicy,
}
