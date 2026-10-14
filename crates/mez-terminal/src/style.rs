//! Styled terminal-cell contracts for one emulated terminal surface.
//!
//! These value types describe colors and rendition spans produced by terminal
//! parsing. They contain no multiplexer layout or host-rendering policy.

pub use mez_core::presentation::{
    PresentationEntry, PresentationOccurrence, PresentationOccurrenceBasis,
};

mod copy_source;
pub use copy_source::{
    TerminalCopySource, TerminalCopySourceGeneration, TerminalCopySourceGroup, TerminalCopySourceId,
};

/// Terminal color recorded from SGR foreground or background parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    /// An indexed ANSI or 256-color palette entry.
    Indexed(u8),
    /// A true-color RGB value.
    Rgb(u8, u8, u8),
}

/// Graphic rendition attributes recorded for a terminal screen cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphicRendition {
    /// Whether bold/intense text is active for the cell.
    pub bold: bool,
    /// Whether dim/faint text is active for the cell.
    pub dim: bool,
    /// Whether italic text is active for the cell.
    pub italic: bool,
    /// Whether underline text is active for the cell.
    pub underline: bool,
    /// Whether double-underline text is active for the cell.
    pub double_underline: bool,
    /// Whether strikethrough text is active for the cell.
    pub strikethrough: bool,
    /// Whether inverse-video rendering is active for the cell.
    pub inverse: bool,
    /// Whether text is hidden/concealed (invisible but occupies space).
    pub hidden: bool,
    /// Optional foreground color for the cell.
    pub foreground: Option<TerminalColor>,
    /// Optional background color for the cell.
    pub background: Option<TerminalColor>,
}

/// A contiguous non-default style run in a visible terminal line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalStyleSpan {
    /// Zero-based terminal-cell column where this style run begins.
    pub start: usize,
    /// Number of terminal cells covered by this style run.
    pub length: usize,
    /// Graphic rendition applied to the covered cells.
    pub rendition: GraphicRendition,
}

/// Plain text plus style spans for one visible terminal line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalStyledLine {
    /// Plain text content for the line.
    pub text: String,
    /// Non-default style runs keyed by terminal-cell columns.
    pub style_spans: Vec<TerminalStyleSpan>,
    /// Optional raw text to use when this presented line is copied.
    ///
    /// Presentation may transform a line while preserving source text for
    /// paste buffers or host clipboards.
    pub copy_text: Option<String>,
    /// Immutable proven source-group ownership, independent of decoration or
    /// physical wrapping. Shared payloads survive clipping without a registry.
    pub copy_source: Option<std::sync::Arc<TerminalCopySource>>,
    /// Optional host annotation; arbitrary PTY output has no entry boundary.
    pub presentation: Option<PresentationEntry>,
}

impl TerminalStyledLine {
    /// Builds an unstyled line from plain text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style_spans: Vec::new(),
            copy_text: None,
            copy_source: None,
            presentation: None,
        }
    }
}
