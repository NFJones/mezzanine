//! Bounded scrollback history for one emulated terminal surface.
//!
//! History records presented text, styling, copy-source text, and physical-line
//! wrapping as one aligned record. Overflow removes oldest records in bounded
//! batches while preserving that metadata alignment.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use crate::{TerminalStyleSpan, TerminalStyledLine};

/// Default maximum number of terminal history lines.
pub const DEFAULT_HISTORY_LIMIT: usize = 10_000;

/// Default number of oldest lines removed when history overflows.
pub const DEFAULT_HISTORY_ROTATE_LINES: usize = 1_000;

/// Reports an invalid bounded-history configuration value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryConfigError {
    message: &'static str,
}

impl HistoryConfigError {
    /// Returns the stable validation message for product-level error adapters.
    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for HistoryConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for HistoryConfigError {}

/// Stores bounded terminal scrollback and aligned presentation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryBuffer {
    limit: usize,
    rotate_lines: usize,
    lines: VecDeque<String>,
    line_style_spans: VecDeque<Vec<TerminalStyleSpan>>,
    line_copy_texts: VecDeque<Option<String>>,
    line_wraps: VecDeque<bool>,
}

impl HistoryBuffer {
    /// Builds a history buffer with the default overflow rotation batch.
    pub fn new(limit: usize) -> Result<Self, HistoryConfigError> {
        Self::new_with_rotation(limit, DEFAULT_HISTORY_ROTATE_LINES)
    }

    /// Builds a history buffer with explicit positive limit and rotation values.
    pub fn new_with_rotation(
        limit: usize,
        rotate_lines: usize,
    ) -> Result<Self, HistoryConfigError> {
        if limit == 0 {
            return Err(HistoryConfigError {
                message: "history buffer limit must be greater than zero",
            });
        }
        if rotate_lines == 0 {
            return Err(HistoryConfigError {
                message: "history buffer rotation line count must be greater than zero",
            });
        }
        Ok(Self {
            limit,
            rotate_lines,
            lines: VecDeque::new(),
            line_style_spans: VecDeque::new(),
            line_copy_texts: VecDeque::new(),
            line_wraps: VecDeque::new(),
        })
    }

    /// Builds a history buffer with the terminal compatibility defaults.
    pub fn default_limit() -> Self {
        Self::new(DEFAULT_HISTORY_LIMIT).expect("default history limit is non-zero")
    }

    /// Appends a non-wrapping presented line to history.
    pub fn push_styled_line(&mut self, line: TerminalStyledLine) {
        self.push_styled_line_with_wrap(line, false);
    }

    /// Appends a presented line and its physical-line wrapping state.
    #[doc(hidden)]
    pub fn push_styled_line_with_wrap(&mut self, line: TerminalStyledLine, wraps: bool) {
        self.lines.push_back(line.text);
        self.line_style_spans.push_back(line.style_spans);
        self.line_copy_texts.push_back(line.copy_text);
        self.line_wraps.push_back(wraps);
        self.enforce_limit();
    }

    /// Removes the newest presented line and its wrapping state.
    #[doc(hidden)]
    pub fn pop_styled_line(&mut self) -> Option<(TerminalStyledLine, bool)> {
        let text = self.lines.pop_back()?;
        let style_spans = self.line_style_spans.pop_back().unwrap_or_default();
        let copy_text = self.line_copy_texts.pop_back().flatten();
        let wraps = self.line_wraps.pop_back().unwrap_or(false);
        Some((
            TerminalStyledLine {
                text,
                style_spans,
                copy_text,
            },
            wraps,
        ))
    }

    /// Changes the positive history limit and immediately enforces it.
    pub fn set_limit(&mut self, limit: usize) -> Result<(), HistoryConfigError> {
        if limit == 0 {
            return Err(HistoryConfigError {
                message: "history buffer limit must be greater than zero",
            });
        }
        self.limit = limit;
        self.enforce_limit();
        Ok(())
    }

    /// Changes the positive overflow rotation batch and enforces the limit.
    pub fn set_rotate_lines(&mut self, rotate_lines: usize) -> Result<(), HistoryConfigError> {
        if rotate_lines == 0 {
            return Err(HistoryConfigError {
                message: "history buffer rotation line count must be greater than zero",
            });
        }
        self.rotate_lines = rotate_lines;
        self.enforce_limit();
        Ok(())
    }

    /// Returns the configured history line limit.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the configured overflow rotation batch size.
    pub fn rotate_lines(&self) -> usize {
        self.rotate_lines
    }

    /// Restores the configured bound and parallel metadata alignment.
    #[doc(hidden)]
    pub fn enforce_limit(&mut self) {
        if self.lines.len() > self.limit {
            let overflow = self.lines.len().saturating_sub(self.limit);
            let rotation = self.rotate_lines.min(self.limit.saturating_sub(1)).max(1);
            let evict_count = overflow.max(rotation).min(self.lines.len());
            self.discard_front_lines(evict_count);
        }
        while self.lines.len() > self.limit {
            self.discard_front_lines(1);
        }
        while self.line_style_spans.len() > self.lines.len() {
            self.line_style_spans.pop_front();
        }
        while self.line_copy_texts.len() > self.lines.len() {
            self.line_copy_texts.pop_front();
        }
        while self.line_wraps.len() > self.lines.len() {
            self.line_wraps.pop_front();
        }
        while self.line_copy_texts.len() < self.lines.len() {
            self.line_copy_texts.push_front(None);
        }
        while self.line_wraps.len() < self.lines.len() {
            self.line_wraps.push_front(false);
        }
    }

    fn discard_front_lines(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let _ = self.lines.drain(..count.min(self.lines.len()));
        let _ = self
            .line_style_spans
            .drain(..count.min(self.line_style_spans.len()));
        let _ = self
            .line_copy_texts
            .drain(..count.min(self.line_copy_texts.len()));
        let _ = self.line_wraps.drain(..count.min(self.line_wraps.len()));
    }

    /// Removes every history record.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.line_style_spans.clear();
        self.line_copy_texts.clear();
        self.line_wraps.clear();
    }

    /// Returns the number of retained history records.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Returns whether no history records are retained.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Iterates over retained presented text from oldest to newest.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// Iterates over retained presented lines and style metadata.
    pub fn styled_lines(&self) -> impl Iterator<Item = TerminalStyledLine> + '_ {
        self.lines
            .iter()
            .zip(self.line_style_spans.iter())
            .zip(
                self.line_copy_texts
                    .iter()
                    .cloned()
                    .chain(std::iter::repeat(None)),
            )
            .map(|((text, style_spans), copy_text)| TerminalStyledLine {
                text: text.clone(),
                style_spans: style_spans.clone(),
                copy_text,
            })
    }

    /// Replaces the copy-source text for one retained history record.
    #[doc(hidden)]
    pub fn set_copy_text(&mut self, index: usize, copy_text: Option<String>) {
        if let Some(slot) = self.line_copy_texts.get_mut(index) {
            *slot = copy_text;
        }
    }

    /// Iterates over retained presented lines and physical wrapping state.
    #[doc(hidden)]
    pub fn styled_lines_with_wraps(&self) -> impl Iterator<Item = (TerminalStyledLine, bool)> + '_ {
        self.styled_lines().zip(
            self.line_wraps
                .iter()
                .copied()
                .chain(std::iter::repeat(false)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies overflow eviction removes the oldest history record first.
    #[test]
    fn history_buffer_evicts_oldest_lines_first() {
        let mut history = HistoryBuffer::new(2).unwrap();
        for line in ["one", "two", "three"] {
            history.push_styled_line(TerminalStyledLine::plain(line));
        }
        assert_eq!(history.lines().collect::<Vec<_>>(), vec!["two", "three"]);
    }

    /// Verifies lowering the limit immediately evicts the oldest records.
    #[test]
    fn history_buffer_relimits_and_evicts_oldest_lines() {
        let mut history = HistoryBuffer::new(4).unwrap();
        for line in ["one", "two", "three", "four"] {
            history.push_styled_line(TerminalStyledLine::plain(line));
        }
        history.set_limit(2).unwrap();
        assert_eq!(history.lines().collect::<Vec<_>>(), vec!["three", "four"]);
        assert!(HistoryBuffer::new(1).unwrap().set_limit(0).is_err());
    }

    /// Verifies overflow can rotate oldest history records in batches.
    #[test]
    fn history_buffer_rotates_oldest_lines_in_configured_batches() {
        let mut history = HistoryBuffer::new_with_rotation(5, 2).unwrap();
        for line in ["one", "two", "three", "four", "five", "six"] {
            history.push_styled_line(TerminalStyledLine::plain(line));
        }
        assert_eq!(
            history.lines().collect::<Vec<_>>(),
            vec!["three", "four", "five", "six"]
        );
        assert!(HistoryBuffer::new_with_rotation(2, 0).is_err());
    }

    /// Verifies the terminal history defaults remain stable compatibility values.
    #[test]
    fn default_history_limit_matches_spec() {
        let history = HistoryBuffer::default_limit();
        assert_eq!(history.limit(), DEFAULT_HISTORY_LIMIT);
        assert_eq!(history.rotate_lines(), DEFAULT_HISTORY_ROTATE_LINES);
    }
}
