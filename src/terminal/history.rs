//! Terminal History implementation.
//!
//! This module owns the terminal history boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, MezError, Result, TerminalStyleSpan,
    TerminalStyledLine, VecDeque,
};

// Bounded terminal history buffers.

/// Carries History Buffer state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryBuffer {
    /// Stores the limit value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) limit: usize,
    /// Stores the number of oldest lines removed when normal history overflow
    /// crosses the configured bound.
    pub(super) rotate_lines: usize,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) lines: VecDeque<String>,
    /// Stores the line style spans value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) line_style_spans: VecDeque<Vec<TerminalStyleSpan>>,
    /// Stores optional raw-copy text for presented history lines.
    ///
    /// `None` means copy mode should use the presented line text.
    pub(super) line_copy_texts: VecDeque<Option<String>>,
    /// Stores the line wraps value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) line_wraps: VecDeque<bool>,
}

impl HistoryBuffer {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(limit: usize) -> Result<Self> {
        Self::new_with_rotation(limit, DEFAULT_HISTORY_ROTATE_LINES)
    }

    /// Builds a history buffer with an explicit limit and overflow rotation
    /// batch. Both values must be greater than zero.
    pub fn new_with_rotation(limit: usize, rotate_lines: usize) -> Result<Self> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "history buffer limit must be greater than zero",
            ));
        }
        if rotate_lines == 0 {
            return Err(MezError::invalid_args(
                "history buffer rotation line count must be greater than zero",
            ));
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

    /// Runs the default limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn default_limit() -> Self {
        Self::new(DEFAULT_HISTORY_LIMIT).expect("default history limit is non-zero")
    }

    /// Runs the push line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn push_line(&mut self, line: impl Into<String>) {
        self.push_styled_line(TerminalStyledLine::plain(line.into()));
    }

    /// Pushes a line with non-default SGR style spans into bounded history.
    pub fn push_styled_line(&mut self, line: TerminalStyledLine) {
        self.push_styled_line_with_wrap(line, false);
    }

    /// Runs the push styled line with wrap operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn push_styled_line_with_wrap(&mut self, line: TerminalStyledLine, wraps: bool) {
        self.lines.push_back(line.text);
        self.line_style_spans.push_back(line.style_spans);
        self.line_copy_texts.push_back(line.copy_text);
        self.line_wraps.push_back(wraps);
        self.enforce_limit();
    }

    /// Runs the set limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_limit(&mut self, limit: usize) -> Result<()> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "history buffer limit must be greater than zero",
            ));
        }
        self.limit = limit;
        self.enforce_limit();
        Ok(())
    }

    /// Updates the number of oldest history lines removed when an append
    /// crosses the configured limit.
    pub fn set_rotate_lines(&mut self, rotate_lines: usize) -> Result<()> {
        if rotate_lines == 0 {
            return Err(MezError::invalid_args(
                "history buffer rotation line count must be greater than zero",
            ));
        }
        self.rotate_lines = rotate_lines;
        self.enforce_limit();
        Ok(())
    }

    /// Runs the limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the configured normal-overflow history rotation batch size.
    pub fn rotate_lines(&self) -> usize {
        self.rotate_lines
    }

    /// Runs the enforce limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn enforce_limit(&mut self) {
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

    /// Removes a batch of oldest history records while keeping parallel
    /// metadata buffers aligned.
    fn discard_front_lines(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let line_count = count.min(self.lines.len());
        let style_count = count.min(self.line_style_spans.len());
        let copy_text_count = count.min(self.line_copy_texts.len());
        let wrap_count = count.min(self.line_wraps.len());
        let _ = self.lines.drain(..line_count);
        let _ = self.line_style_spans.drain(..style_count);
        let _ = self.line_copy_texts.drain(..copy_text_count);
        let _ = self.line_wraps.drain(..wrap_count);
    }

    /// Runs the clear operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.line_style_spans.clear();
        self.line_copy_texts.clear();
        self.line_wraps.clear();
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Runs the lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// Returns history lines with their non-default SGR style spans.
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

    /// Runs the styled lines with wraps operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn styled_lines_with_wraps(
        &self,
    ) -> impl Iterator<Item = (TerminalStyledLine, bool)> + '_ {
        self.styled_lines().zip(
            self.line_wraps
                .iter()
                .copied()
                .chain(std::iter::repeat(false)),
        )
    }
}
