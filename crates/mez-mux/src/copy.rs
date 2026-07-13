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
