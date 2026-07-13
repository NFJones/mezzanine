//! Dependency-neutral copy-mode contracts for multiplexer presentation.
//!
//! This module owns coordinates and state contracts shared by copy-mode input,
//! rendering, and runtime adapters. Product-specific copy-text normalization
//! remains in the Mezzanine composition crate.

/// Identifies one terminal-cell position in a copy-mode buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CopyPosition {
    /// Zero-based logical line in the copy-mode buffer.
    pub line: usize,
    /// Zero-based terminal-cell column within the line.
    pub column: usize,
}

/// Direction used when searching a multiplexer-owned copy buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    /// Search toward later lines, wrapping to the beginning when needed.
    Forward,
    /// Search toward earlier lines, wrapping to the end when needed.
    Backward,
}
