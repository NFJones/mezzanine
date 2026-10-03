//! Immutable draft display/source associations for explicit selection.
//!
//! Readline remains the authority for entered bytes. Literal ranges map one to
//! one, collapsed paste is atomic for source export, and injected completion
//! text has no entered-source authority. Coordinates are UTF-8 byte ranges;
//! terminal-cell wrapping is owned by the prompt layout adapter.

use std::ops::Range;

/// One rendered range and its entered-source association.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlineSourceSpan {
    /// UTF-8 range in the display string.
    pub display: Range<usize>,
    /// UTF-8 range in entered source; absent for provisional completion text.
    pub source: Option<Range<usize>>,
    /// Whether intersecting this display selects a whole collapsed source block.
    pub collapsed: bool,
}

/// Immutable projection of one exact editable revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlineSourceProjection {
    /// Collapsed display text, optionally including an unaccepted shadow.
    pub display: String,
    /// Exact entered text with opaque paste markers expanded.
    pub source: String,
    /// Ordered nonoverlapping display/source associations.
    pub spans: Vec<ReadlineSourceSpan>,
}

impl ReadlineSourceProjection {
    /// Copies a UTF-8-aligned display selection, excluding unaccepted text.
    /// Rendered copy keeps selected paste-label text; explicit source copy
    /// expands each intersected collapsed block atomically. Invalid boundaries
    /// return None rather than slicing unrelated or stale bytes.
    pub fn copy_range(&self, selection: Range<usize>, source: bool) -> Option<String> {
        if selection.start > selection.end
            || !self.display.is_char_boundary(selection.start)
            || !self.display.is_char_boundary(selection.end)
        {
            return None;
        }
        let mut copied = String::new();
        for span in &self.spans {
            let start = selection.start.max(span.display.start);
            let end = selection.end.min(span.display.end);
            if start >= end {
                continue;
            }
            let Some(raw) = &span.source else { continue };
            if source && span.collapsed {
                copied.push_str(self.source.get(raw.clone())?);
            } else if source {
                let offset = start.checked_sub(span.display.start)?;
                let raw_start = raw.start.checked_add(offset)?;
                copied.push_str(
                    self.source
                        .get(raw_start..raw_start.checked_add(end - start)?)?,
                );
            } else {
                copied.push_str(self.display.get(start..end)?);
            }
        }
        Some(copied)
    }
}
