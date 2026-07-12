//! Terminal-surface geometry contracts.
//!
//! This module owns dimensions measured in terminal cells. It deliberately
//! excludes pane placement, split layout, and viewport composition, which are
//! multiplexer responsibilities.

use std::fmt;

/// Positive dimensions for one terminal surface, measured in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    /// Number of terminal columns.
    pub columns: u16,
    /// Number of terminal rows.
    pub rows: u16,
}

impl TerminalSize {
    /// Builds positive terminal dimensions.
    ///
    /// Returns [`TerminalSizeError`] when either dimension is zero because a
    /// terminal surface cannot contain an empty axis.
    pub fn new(columns: u16, rows: u16) -> Result<Self, TerminalSizeError> {
        if columns == 0 || rows == 0 {
            return Err(TerminalSizeError);
        }
        Ok(Self { columns, rows })
    }
}

/// Error returned when terminal dimensions contain a zero axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSizeError;

impl TerminalSizeError {
    /// Returns the stable user-facing validation diagnostic.
    pub const fn message(self) -> &'static str {
        "terminal dimensions must be positive non-zero cells"
    }
}

impl fmt::Display for TerminalSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for TerminalSizeError {}
