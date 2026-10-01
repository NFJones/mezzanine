//! Terminal-surface geometry contracts.
//!
//! This module owns dimensions measured in terminal cells. It deliberately
//! excludes pane placement, split layout, and viewport composition, which are
//! multiplexer responsibilities.
//! Positive axes and a separate visible-cell budget bound per-surface grids
//! and row metadata. Public fields require revalidation at allocation/mutation
//! edges; accepted geometry is not a guarantee of global memory availability.

use std::fmt;

/// Maximum cells on either terminal axis, bounding row metadata and line work.
pub const MAX_TERMINAL_AXIS_CELLS: u16 = 4096;
/// Maximum visible cells per surface, independently bounding cell/style grids.
pub const MAX_TERMINAL_SURFACE_CELLS: u32 = 262_144;

/// Positive dimensions for one terminal surface, measured in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    /// Number of terminal columns.
    pub columns: u16,
    /// Number of terminal rows.
    pub rows: u16,
}

impl TerminalSize {
    /// Builds positive, resource-bounded terminal dimensions.
    ///
    /// Rejects empty axes, axes above 4096, or a product above 262,144 cells.
    pub fn new(columns: u16, rows: u16) -> Result<Self, TerminalSizeError> {
        let size = Self { columns, rows };
        size.validate()?;
        Ok(size)
    }

    /// Validates dimensions even when callers construct or mutate public fields.
    /// Validation performs no grid allocation and precedes screen mutation.
    pub fn validate(self) -> Result<(), TerminalSizeError> {
        let Self { columns, rows } = self;
        if columns == 0 || rows == 0 {
            return Err(TerminalSizeError::NonPositive);
        }
        if columns > MAX_TERMINAL_AXIS_CELLS
            || rows > MAX_TERMINAL_AXIS_CELLS
            || u32::from(columns) * u32::from(rows) > MAX_TERMINAL_SURFACE_CELLS
        {
            return Err(TerminalSizeError::ResourceLimit);
        }
        Ok(())
    }
}

/// Error returned for empty or resource-excessive terminal dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSizeError {
    /// At least one axis contains no cells.
    NonPositive,
    /// An axis or the visible cell product exceeds the allocation safety budget.
    ResourceLimit,
}

impl TerminalSizeError {
    /// Returns the stable user-facing validation diagnostic.
    pub const fn message(self) -> &'static str {
        match self {
            Self::NonPositive => "terminal dimensions must be positive non-zero cells",
            Self::ResourceLimit => {
                "terminal dimensions exceed 4096 cells per axis or 262144 surface cells"
            }
        }
    }
}

impl fmt::Display for TerminalSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for TerminalSizeError {}
