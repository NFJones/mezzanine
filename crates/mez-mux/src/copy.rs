//! Dependency-neutral copy-mode contracts for multiplexer presentation.
//!
//! This module owns coordinates and state contracts shared by copy-mode input,
//! rendering, and runtime adapters. Product-specific copy-text normalization
//! remains in the Mezzanine composition crate.

use crate::{MuxError, Result};
use mez_terminal::{terminal_emoji_width, terminal_text_width};

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

/// Searches copy-buffer lines in the requested direction, wrapping once.
///
/// Returns the terminal-cell position and display width of the first match.
pub fn search_lines(
    lines: &[String],
    start_line: usize,
    query: &str,
    direction: SearchDirection,
) -> Option<(CopyPosition, usize)> {
    if lines.is_empty() {
        return None;
    }

    let start = start_line.min(lines.len().saturating_sub(1));
    let indices: Box<dyn Iterator<Item = usize>> = match direction {
        SearchDirection::Forward => Box::new((start..lines.len()).chain(0..start)),
        SearchDirection::Backward => {
            Box::new((0..=start).rev().chain(((start + 1)..lines.len()).rev()))
        }
    };

    for line_index in indices {
        let line = &lines[line_index];
        let Some(byte_index) = (match direction {
            SearchDirection::Forward => line.find(query),
            SearchDirection::Backward => line.rfind(query),
        }) else {
            continue;
        };
        return Some((
            CopyPosition {
                line: line_index,
                column: terminal_text_width(&line[..byte_index], terminal_emoji_width()),
            },
            terminal_text_width(query, terminal_emoji_width()),
        ));
    }
    None
}

/// Orders a copy-mode selection from its earlier position to its later one.
pub fn normalize_selection(start: CopyPosition, end: CopyPosition) -> (CopyPosition, CopyPosition) {
    if start <= end {
        (start, end)
    } else {
        (end, start)
    }
}

/// Validates that a copy position references an existing buffer line.
pub fn validate_position(lines: &[String], position: CopyPosition) -> Result<()> {
    if lines.get(position.line).is_none() {
        return Err(MuxError::invalid_args(
            "copy mode selection line is out of range",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies forward and backward searches wrap around the copy buffer while
    /// reporting terminal-cell columns rather than UTF-8 byte offsets.
    #[test]
    fn copy_search_wraps_and_reports_terminal_columns() {
        let lines = vec!["first target".to_string(), "中 target".to_string()];

        assert_eq!(
            search_lines(&lines, 1, "target", SearchDirection::Forward),
            Some((CopyPosition { line: 1, column: 3 }, 6))
        );
        assert_eq!(
            search_lines(&lines, 1, "first", SearchDirection::Forward),
            Some((CopyPosition { line: 0, column: 0 }, 5))
        );
        assert_eq!(
            search_lines(&lines, 0, "target", SearchDirection::Backward),
            Some((CopyPosition { line: 0, column: 6 }, 6))
        );
        assert_eq!(
            search_lines(&lines, 0, "中", SearchDirection::Backward),
            Some((CopyPosition { line: 1, column: 0 }, 2))
        );
    }

    /// Verifies selection normalization preserves already ordered ranges and
    /// reverses ranges created by backward mouse drags.
    #[test]
    fn copy_selection_normalizes_position_order() {
        let earlier = CopyPosition { line: 1, column: 2 };
        let later = CopyPosition { line: 3, column: 4 };

        assert_eq!(normalize_selection(earlier, later), (earlier, later));
        assert_eq!(normalize_selection(later, earlier), (earlier, later));
    }

    /// Verifies copy-position validation accepts existing lines and reports a
    /// mux-owned invalid-argument error for positions outside the buffer.
    #[test]
    fn copy_position_validation_rejects_missing_lines() {
        let lines = vec!["only line".to_string()];

        assert!(
            validate_position(
                &lines,
                CopyPosition {
                    line: 0,
                    column: 99
                }
            )
            .is_ok()
        );
        let error = validate_position(&lines, CopyPosition { line: 1, column: 0 }).unwrap_err();
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert_eq!(error.message(), "copy mode selection line is out of range");
    }
}
