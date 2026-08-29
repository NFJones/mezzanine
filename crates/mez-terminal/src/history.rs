//! Bounded scrollback history for one emulated terminal surface.
//!
//! History records presented text, styling, copy-source text, and physical-line
//! wrapping as one aligned record. Overflow removes oldest records in bounded
//! batches while preserving that metadata alignment.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::TerminalStyledLine;

/// Default maximum number of terminal history lines.
pub const DEFAULT_HISTORY_LIMIT: usize = 10_000;

/// Default number of oldest lines removed when history overflows.
pub const DEFAULT_HISTORY_ROTATE_LINES: usize = 1_000;

/// Maximum number of mutable history records copied by a screen snapshot.
const HISTORY_CHUNK_RECORDS: usize = 128;

/// One aligned retained terminal row.
#[derive(Debug, PartialEq, Eq)]
struct HistoryRecord {
    line: TerminalStyledLine,
    wraps: bool,
}

impl Clone for HistoryRecord {
    fn clone(&self) -> Self {
        #[cfg(test)]
        HISTORY_RECORD_CLONES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            line: self.line.clone(),
            wraps: self.wraps,
        }
    }
}

#[cfg(test)]
static HISTORY_RECORD_CLONES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

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

/// Stores bounded terminal scrollback as shared immutable chunks and a mutable tail.
///
/// Cloning a buffer shares sealed chunks and copies at most one bounded tail.
/// Mutations therefore preserve value isolation without making pane snapshots
/// copy all retained scrollback.
#[derive(Debug, Clone)]
pub struct HistoryBuffer {
    limit: usize,
    rotate_lines: usize,
    chunks: Arc<Vec<Arc<[HistoryRecord]>>>,
    front_offset: usize,
    tail: Vec<HistoryRecord>,
    len: usize,
}

impl PartialEq for HistoryBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.limit == other.limit
            && self.rotate_lines == other.rotate_lines
            && self.records().eq(other.records())
    }
}

impl Eq for HistoryBuffer {}

impl HistoryBuffer {
    /// Builds an empty buffer with the same retention and rotation policy.
    ///
    /// Terminal protocol parsing uses this when retained history must be moved
    /// aside temporarily. Constructing the placeholder directly preserves
    /// already-validated policy values without introducing a fallible runtime
    /// path.
    pub(crate) fn empty_with_same_policy(&self) -> Self {
        Self {
            limit: self.limit,
            rotate_lines: self.rotate_lines,
            chunks: Arc::new(Vec::new()),
            front_offset: 0,
            tail: Vec::new(),
            len: 0,
        }
    }

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
            chunks: Arc::new(Vec::new()),
            front_offset: 0,
            tail: Vec::new(),
            len: 0,
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
        self.seal_full_tail();
        self.tail.push(HistoryRecord { line, wraps });
        self.len = self.len.saturating_add(1);
        self.enforce_limit();
    }

    /// Removes the newest presented line and its wrapping state.
    #[doc(hidden)]
    pub fn pop_styled_line(&mut self) -> Option<(TerminalStyledLine, bool)> {
        if self.tail.is_empty() {
            let chunks = Arc::make_mut(&mut self.chunks);
            let chunk = chunks.pop()?;
            let skipped = if chunks.is_empty() {
                std::mem::take(&mut self.front_offset)
            } else {
                0
            };
            self.tail = chunk.iter().skip(skipped).cloned().collect();
        }
        let record = self.tail.pop()?;
        self.len = self.len.saturating_sub(1);
        Some((record.line, record.wraps))
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

    /// Restores the configured bound.
    #[doc(hidden)]
    pub fn enforce_limit(&mut self) {
        if self.len > self.limit {
            let overflow = self.len.saturating_sub(self.limit);
            let rotation = self.rotate_lines.min(self.limit.saturating_sub(1)).max(1);
            let evict_count = overflow.max(rotation).min(self.len);
            self.discard_front_lines(evict_count);
        }
        while self.len > self.limit {
            self.discard_front_lines(1);
        }
    }

    /// Moves a full mutable tail into immutable shared storage.
    fn seal_full_tail(&mut self) {
        if self.tail.len() < HISTORY_CHUNK_RECORDS {
            return;
        }
        let sealed = Arc::<[HistoryRecord]>::from(std::mem::take(&mut self.tail));
        Arc::make_mut(&mut self.chunks).push(sealed);
    }

    /// Removes oldest records while retaining untouched immutable chunks.
    fn discard_front_lines(&mut self, count: usize) {
        let mut remaining = count.min(self.len);
        if remaining == 0 {
            return;
        }
        let chunks = Arc::make_mut(&mut self.chunks);
        while remaining > 0 && !chunks.is_empty() {
            let available = chunks[0].len().saturating_sub(self.front_offset);
            if remaining < available {
                self.front_offset = self.front_offset.saturating_add(remaining);
                self.len = self.len.saturating_sub(remaining);
                return;
            }
            remaining = remaining.saturating_sub(available);
            self.len = self.len.saturating_sub(available);
            chunks.remove(0);
            self.front_offset = 0;
        }
        if remaining > 0 {
            let drained = remaining.min(self.tail.len());
            self.tail.drain(..drained);
            self.len = self.len.saturating_sub(drained);
        }
    }

    /// Removes every history record.
    pub fn clear(&mut self) {
        self.chunks = Arc::new(Vec::new());
        self.front_offset = 0;
        self.tail.clear();
        self.len = 0;
    }

    /// Returns the number of retained history records.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no history records are retained.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterates over retained presented text from oldest to newest.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.records().map(|record| record.line.text.as_str())
    }

    /// Returns one retained presented row without cloning its text.
    pub(crate) fn line_at(&self, index: usize) -> Option<&str> {
        self.record_at(index)
            .map(|record| record.line.text.as_str())
    }

    /// Reports whether one retained physical row wraps into its successor.
    pub(crate) fn line_wraps_to_next(&self, index: usize) -> bool {
        self.record_at(index).is_some_and(|record| record.wraps)
    }

    /// Returns one retained copy-source override for focused tests.
    #[cfg(test)]
    pub(crate) fn copy_text_at(&self, index: usize) -> Option<&str> {
        self.record_at(index)
            .and_then(|record| record.line.copy_text.as_deref())
    }

    /// Iterates over retained presented lines and style metadata.
    pub fn styled_lines(&self) -> impl Iterator<Item = TerminalStyledLine> + '_ {
        self.records().map(|record| record.line.clone())
    }

    /// Replaces the copy-source text for one retained history record.
    #[doc(hidden)]
    pub fn set_copy_text(&mut self, index: usize, copy_text: Option<String>) {
        if index >= self.len {
            return;
        }
        let mut logical_start = 0usize;
        let chunks = Arc::make_mut(&mut self.chunks);
        for (chunk_index, chunk) in chunks.iter_mut().enumerate() {
            let skipped = if chunk_index == 0 {
                self.front_offset
            } else {
                0
            };
            let available = chunk.len().saturating_sub(skipped);
            if index < logical_start.saturating_add(available) {
                let record_index = skipped.saturating_add(index.saturating_sub(logical_start));
                let mut records = chunk.iter().cloned().collect::<Vec<_>>();
                records[record_index].line.copy_text = copy_text;
                *chunk = Arc::from(records);
                return;
            }
            logical_start = logical_start.saturating_add(available);
        }
        if let Some(record) = self.tail.get_mut(index.saturating_sub(logical_start)) {
            record.line.copy_text = copy_text;
        }
    }

    /// Iterates over retained presented lines and physical wrapping state.
    #[doc(hidden)]
    pub fn styled_lines_with_wraps(&self) -> impl Iterator<Item = (TerminalStyledLine, bool)> + '_ {
        self.records()
            .map(|record| (record.line.clone(), record.wraps))
    }

    /// Iterates over aligned records without flattening shared chunks.
    fn records(&self) -> impl Iterator<Item = &HistoryRecord> {
        self.chunks
            .iter()
            .enumerate()
            .flat_map(move |(index, chunk)| {
                let skipped = if index == 0 { self.front_offset } else { 0 };
                chunk.iter().skip(skipped)
            })
            .chain(self.tail.iter())
    }

    /// Returns one logical retained record.
    fn record_at(&self, index: usize) -> Option<&HistoryRecord> {
        self.records().nth(index)
    }

    /// Resets the test-only count of copied history records.
    #[cfg(test)]
    pub(crate) fn reset_copied_record_count() {
        HISTORY_RECORD_CLONES.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns the test-only count of copied history records.
    #[cfg(test)]
    pub(crate) fn copied_record_count() -> usize {
        HISTORY_RECORD_CLONES.load(std::sync::atomic::Ordering::Relaxed)
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
