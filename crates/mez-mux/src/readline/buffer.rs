//! Editable line buffer behavior and UTF-8 cursor movement.
//!
//! The buffer owns history navigation and text mutation invariants. It never
//! reads terminal bytes directly; decoded edits are applied by callers.

use std::collections::BTreeSet;
use std::sync::Arc;

use unicode_width::UnicodeWidthChar;

/// Default number of submitted prompt entries retained by a readline buffer.
pub const DEFAULT_READLINE_HISTORY_LIMIT: usize = 1000;
/// Maximum bytes retained for one submitted prompt-history entry.
pub const MAX_READLINE_HISTORY_ENTRY_BYTES: usize = 64 * 1024;
/// Maximum aggregate bytes retained by one readline history.
pub const MAX_READLINE_HISTORY_BYTES: usize = 256 * 1024;
/// Minimum pasted text byte count rendered as one collapsed prompt block.
const READLINE_PASTE_BLOCK_THRESHOLD_BYTES: usize = 1024;

const READLINE_PASTE_BLOCK_MARKER_BASE: u32 = 0xf0000;
const READLINE_PASTE_BLOCK_THRESHOLD_LINES: usize = 6;

/// A normalized editing command for a readline-style prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadlineEdit {
    /// Insert one character.
    Insert(char),
    /// Insert a text fragment.
    InsertText(String),
    /// Insert text decoded from a bracketed-paste sequence.
    InsertPaste(String),
    /// Move one character left.
    MoveLeft,
    /// Move one character right.
    MoveRight,
    /// Move left by one shell-style word.
    MoveWordLeft,
    /// Move right by one shell-style word.
    MoveWordRight,
    /// Move to the beginning of the logical line.
    MoveHome,
    /// Move to the end of the logical line.
    MoveEnd,
    /// Move to the beginning of the editable buffer.
    MoveBufferStart,
    /// Move to the end of the editable buffer.
    MoveBufferEnd,
    /// Move to the previous prompt row or history entry.
    MoveRowUpOrHistoryPrevious,
    /// Move to the next prompt row or history entry.
    MoveRowDownOrHistoryNext,
    /// Delete the character before the cursor.
    Backspace,
    /// Delete the character under the cursor.
    DeleteForward,
    /// Delete the shell-style word before the cursor.
    KillWordLeft,
    /// Delete the shell-style word after the cursor.
    KillWordRight,
    /// Delete text before the cursor.
    KillToStart,
    /// Delete text after the cursor.
    KillToEnd,
    /// Recall the previous history entry.
    HistoryPrevious,
    /// Recall the next history entry.
    HistoryNext,
    /// Search backward through history.
    HistorySearchBackward,
    /// Submit the editable text.
    Submit,
}

/// The observable result of applying an editing command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadlineOutcome {
    /// The editable state changed.
    Edited,
    /// Text was submitted.
    Submitted(String),
    /// Raw text was submitted with a collapsed display representation.
    SubmittedWithDisplay {
        /// Exact submitted text.
        text: String,
        /// Human-readable collapsed display text.
        display: String,
        /// Byte ranges of pasted payloads collapsed in `display`.
        collapsed_paste_ranges: Vec<ReadlinePasteRange>,
    },
    /// The prompt was cancelled.
    Cancelled,
    /// End-of-input was requested.
    Eof,
    /// No editable state changed.
    Noop,
}

impl From<bool> for ReadlineOutcome {
    /// Converts whether an edit changed state into its observable outcome.
    fn from(changed: bool) -> Self {
        if changed { Self::Edited } else { Self::Noop }
    }
}

/// One byte range in raw prompt text that originated as a large bracketed paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadlinePasteRange {
    /// Inclusive UTF-8 byte offset in the raw prompt.
    pub start: usize,
    /// Exclusive UTF-8 byte offset in the raw prompt.
    pub end: usize,
}

/// One retained prompt together with its collapsed-paste provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadlineHistoryEntry {
    /// Exact text submitted to the command or agent backend.
    pub text: String,
    /// Ordered raw-text ranges rendered as compact pasted blocks.
    pub collapsed_paste_ranges: Vec<ReadlinePasteRange>,
}

impl ReadlineHistoryEntry {
    /// Creates a provenance-free history entry from ordinary text.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            collapsed_paste_ranges: Vec::new(),
        }
    }

    /// Reports whether every collapsed range is ordered and UTF-8 aligned.
    pub fn is_valid(&self) -> bool {
        valid_paste_ranges(&self.text, &self.collapsed_paste_ranges)
    }

    /// Renders only explicitly recorded pasted ranges as compact labels.
    pub fn rendered(&self) -> String {
        render_history_entry(self)
    }
}

/// Opaque editable-state snapshot used to restore reverse-search drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlineDraft {
    line: String,
    cursor: usize,
    paste_blocks: Vec<ReadlinePasteBlock>,
    next_paste_block_id: u32,
}

/// Editable line state with bounded submission history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlineBuffer {
    line: String,
    cursor: usize,
    history: SharedReadlineHistory,
    history_limit: usize,
    history_cursor: Option<usize>,
    history_entry_cursor_navigation: bool,
    vertical_navigation_column: Option<usize>,
    draft_before_history: String,
    paste_blocks: Vec<ReadlinePasteBlock>,
    next_paste_block_id: u32,
    draft_before_history_paste_blocks: Vec<ReadlinePasteBlock>,
    draft_before_history_next_paste_block_id: u32,
}

/// Immutable retained history shared by prompt snapshots until submission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ReadlineHistory {
    entries: Vec<String>,
    /// Lowercased entries kept index-aligned for reverse search.
    normalized_entries: Vec<String>,
    /// Collapsed-paste ranges kept index-aligned with raw entries.
    collapsed_paste_ranges: Vec<Vec<ReadlinePasteRange>>,
    bytes: usize,
}

/// Clone-cheap retained history with pointer-fast equality for snapshots.
#[derive(Debug, Clone, Default)]
struct SharedReadlineHistory(Arc<ReadlineHistory>);

impl std::ops::Deref for SharedReadlineHistory {
    type Target = ReadlineHistory;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for SharedReadlineHistory {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Eq for SharedReadlineHistory {}

/// One large pasted payload collapsed in prompt rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadlinePasteBlock {
    marker: char,
    content: String,
}

/// Returns the shell-style word range surrounding a character column.
///
/// Columns are counted in Unicode scalar values so terminal copy-mode callers
/// can share readline delimiter rules without depending on prompt buffer byte
/// cursors. Whitespace columns select an empty range at the clicked column.
pub fn readline_word_column_range(text: &str, column: usize) -> (usize, usize) {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return (0, 0);
    }
    let index = column.min(chars.len().saturating_sub(1));
    let Some(ch) = chars.get(index).copied() else {
        return (chars.len(), chars.len());
    };
    if ch.is_whitespace() {
        return (index, index);
    }

    let identifier = readline_word_is_identifier(ch);
    let mut start = index;
    while start > 0 {
        let previous = chars[start.saturating_sub(1)];
        if previous.is_whitespace() || readline_word_is_identifier(previous) != identifier {
            break;
        }
        start = start.saturating_sub(1);
    }
    let mut end = index.saturating_add(1);
    while end < chars.len() {
        let next = chars[end];
        if next.is_whitespace() || readline_word_is_identifier(next) != identifier {
            break;
        }
        end = end.saturating_add(1);
    }
    (start, end)
}

impl Default for ReadlineBuffer {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self::new()
    }
}

impl ReadlineBuffer {
    /// Create an empty buffer with the default history limit.
    pub fn new() -> Self {
        Self::with_history_limit(DEFAULT_READLINE_HISTORY_LIMIT)
    }

    /// Create an empty buffer with a caller-selected history limit.
    pub fn with_history_limit(history_limit: usize) -> Self {
        Self {
            line: String::new(),
            cursor: 0,
            history: SharedReadlineHistory::default(),
            history_limit,
            history_cursor: None,
            history_entry_cursor_navigation: false,
            vertical_navigation_column: None,
            draft_before_history: String::new(),
            paste_blocks: Vec::new(),
            next_paste_block_id: 0,
            draft_before_history_paste_blocks: Vec::new(),
            draft_before_history_next_paste_block_id: 0,
        }
    }

    /// Current editable text.
    pub fn line(&self) -> &str {
        &self.line
    }

    /// Current cursor position as a byte offset on a UTF-8 boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Current bounded history, ordered from oldest to newest without
    /// consecutively equal submissions.
    pub fn history(&self) -> &[String] {
        &self.history.entries
    }

    /// Number of retained prompt-history entries.
    pub fn history_len(&self) -> usize {
        self.history.entries.len()
    }

    /// Returns one retained prompt-history entry by index.
    pub fn history_entry(&self, index: usize) -> Option<&str> {
        self.history.entries.get(index).map(AsRef::as_ref)
    }

    /// Returns one retained prompt with its collapsed-paste provenance.
    pub fn structured_history_entry(&self, index: usize) -> Option<ReadlineHistoryEntry> {
        Some(ReadlineHistoryEntry {
            text: self.history.entries.get(index)?.clone(),
            collapsed_paste_ranges: self.history.collapsed_paste_ranges.get(index)?.clone(),
        })
    }

    /// Aggregate bytes retained by prompt history.
    pub fn history_bytes(&self) -> usize {
        self.history.bytes
    }

    /// Reports whether two snapshots share the same immutable history storage.
    #[cfg(test)]
    pub(crate) fn shares_history_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.history.0, &other.history.0)
    }

    /// Replace retained submission history while preserving the active edit line.
    pub fn set_history(&mut self, history: impl IntoIterator<Item = String>) {
        self.history = SharedReadlineHistory::default();
        for entry in history {
            self.remember_submission(ReadlineHistoryEntry::literal(entry));
        }
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.vertical_navigation_column = None;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
    }

    /// Replace retained history with raw text and explicit paste provenance.
    pub fn set_structured_history(
        &mut self,
        history: impl IntoIterator<Item = ReadlineHistoryEntry>,
    ) {
        self.history = SharedReadlineHistory::default();
        for entry in history {
            self.remember_submission(entry);
        }
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.vertical_navigation_column = None;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
    }

    /// Replace the current editable text and place the cursor at the end.
    pub fn set_line(&mut self, line: impl Into<String>) {
        self.replace_current_line_with_text(line.into());
        self.cursor = self.line.len();
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
    }

    /// Replace the current editable text and place the cursor at a boundary.
    pub fn set_line_and_cursor(&mut self, line: impl Into<String>, cursor: usize) {
        self.replace_current_line_with_text(line.into());
        self.cursor = cursor.min(self.line.len());
        while self.cursor > 0 && !self.line.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
    }

    /// Replace the current editable text while retaining paste blocks whose
    /// private markers remain in the replacement.
    pub fn set_line_and_cursor_preserving_paste_blocks(
        &mut self,
        line: impl Into<String>,
        cursor: usize,
    ) {
        self.line = line.into();
        self.cursor = cursor.min(self.line.len());
        while self.cursor > 0 && !self.line.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
        self.cleanup_unused_paste_blocks();
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.vertical_navigation_column = None;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
    }

    /// Replace a byte range in the current line and place the cursor after it.
    ///
    /// The range must be ordered and aligned to UTF-8 boundaries. Invalid ranges
    /// are ignored and return `false` so selector callers can fail closed.
    pub fn replace_range(&mut self, start: usize, end: usize, replacement: &str) -> bool {
        if start > end
            || end > self.line.len()
            || !self.line.is_char_boundary(start)
            || !self.line.is_char_boundary(end)
        {
            return false;
        }
        self.leave_history_navigation_for_edit();
        self.line.replace_range(start..end, replacement);
        self.cursor = start.saturating_add(replacement.len());
        self.cleanup_unused_paste_blocks();
        true
    }

    /// Apply an editing command and report whether it changed or submitted text.
    pub fn apply(&mut self, edit: ReadlineEdit) -> ReadlineOutcome {
        match edit {
            ReadlineEdit::Insert(ch) => {
                self.insert_char(ch);
                ReadlineOutcome::Edited
            }
            ReadlineEdit::InsertText(text) => {
                if text.is_empty() {
                    ReadlineOutcome::Noop
                } else {
                    self.insert_text(&text);
                    ReadlineOutcome::Edited
                }
            }
            ReadlineEdit::InsertPaste(text) => {
                if text.is_empty() {
                    ReadlineOutcome::Noop
                } else {
                    self.insert_pasted_text(&text);
                    ReadlineOutcome::Edited
                }
            }
            ReadlineEdit::MoveLeft => Self::bool_outcome(self.move_left()),
            ReadlineEdit::MoveRight => Self::bool_outcome(self.move_right()),
            ReadlineEdit::MoveWordLeft => Self::bool_outcome(self.move_word_left()),
            ReadlineEdit::MoveWordRight => Self::bool_outcome(self.move_word_right()),
            ReadlineEdit::MoveHome => Self::bool_outcome(self.move_home()),
            ReadlineEdit::MoveEnd => Self::bool_outcome(self.move_end()),
            ReadlineEdit::MoveBufferStart => Self::bool_outcome(self.move_buffer_start()),
            ReadlineEdit::MoveBufferEnd => Self::bool_outcome(self.move_buffer_end()),
            ReadlineEdit::MoveRowUpOrHistoryPrevious => {
                Self::bool_outcome(self.move_row_up_or_history_previous())
            }
            ReadlineEdit::MoveRowDownOrHistoryNext => {
                Self::bool_outcome(self.move_row_down_or_history_next())
            }
            ReadlineEdit::Backspace => Self::bool_outcome(self.backspace()),
            ReadlineEdit::DeleteForward => Self::bool_outcome(self.delete_forward()),
            ReadlineEdit::KillWordLeft => Self::bool_outcome(self.kill_word_left()),
            ReadlineEdit::KillWordRight => Self::bool_outcome(self.kill_word_right()),
            ReadlineEdit::KillToStart => Self::bool_outcome(self.kill_to_start()),
            ReadlineEdit::KillToEnd => Self::bool_outcome(self.kill_to_end()),
            ReadlineEdit::HistoryPrevious => Self::bool_outcome(self.history_previous()),
            ReadlineEdit::HistoryNext => Self::bool_outcome(self.history_next()),
            ReadlineEdit::HistorySearchBackward => {
                Self::bool_outcome(self.history_search_backward())
            }
            ReadlineEdit::Submit => {
                let (text, display, collapsed_paste_ranges) = self.submit_with_display();
                if text == display {
                    ReadlineOutcome::Submitted(text)
                } else {
                    ReadlineOutcome::SubmittedWithDisplay {
                        text,
                        display,
                        collapsed_paste_ranges,
                    }
                }
            }
        }
    }

    /// Insert one character at the cursor.
    pub fn insert_char(&mut self, ch: char) {
        self.leave_history_navigation_for_edit();
        self.line.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Insert text at the cursor.
    pub fn insert_text(&mut self, text: &str) {
        self.leave_history_navigation_for_edit();
        self.line.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Insert text known to originate from one bracketed-paste sequence.
    pub fn insert_pasted_text(&mut self, text: &str) {
        self.leave_history_navigation_for_edit();
        if text.len() >= READLINE_PASTE_BLOCK_THRESHOLD_BYTES
            || Self::pasted_text_exceeds_visible_prompt_height(text)
        {
            self.insert_paste_block(text.to_string());
        } else {
            self.line.insert_str(self.cursor, text);
            self.cursor += text.len();
        }
    }

    /// Returns the line with large pasted blocks expanded to their exact text.
    pub fn expanded_line(&self) -> String {
        render_line_with_blocks(&self.line, &self.paste_blocks, RenderLineMode::Expanded)
    }

    /// Returns the line with large pasted blocks rendered as compact labels.
    pub fn rendered_line(&self) -> String {
        render_line_with_blocks(&self.line, &self.paste_blocks, RenderLineMode::Collapsed)
    }

    /// Returns the line rendered with text inserted at an internal byte offset.
    ///
    /// # Parameters
    /// - `insert_at`: UTF-8 byte offset in the internal editable line.
    /// - `insert_text`: Display text to inject at the offset.
    pub fn rendered_line_with_insert(&self, insert_at: usize, insert_text: &str) -> Option<String> {
        if insert_at > self.line.len() || !self.line.is_char_boundary(insert_at) {
            return None;
        }
        Some(render_line_with_insert(
            &self.line,
            &self.paste_blocks,
            insert_at,
            insert_text,
        ))
    }

    /// Returns collapsed display columns before one internal byte offset.
    pub fn rendered_columns_before(&self, byte_offset: usize) -> usize {
        let bounded = byte_offset.min(self.line.len());
        if !self.line.is_char_boundary(bounded) {
            return 0;
        }
        render_line_with_blocks(
            &self.line[..bounded],
            &self.paste_blocks,
            RenderLineMode::Collapsed,
        )
        .chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0).max(1))
        .sum()
    }

    /// Move the cursor one character left.
    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = previous_boundary(&self.line, self.cursor);
        true
    }

    /// Move the cursor one character right.
    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.line.len() {
            return false;
        }

        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = next_boundary(&self.line, self.cursor);
        true
    }

    /// Move the cursor to the beginning of the line.
    pub fn move_home(&mut self) -> bool {
        let target = line_start_before_cursor(&self.line, self.cursor);
        if self.cursor == target {
            return false;
        }

        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = target;
        true
    }

    /// Move the cursor to the end of the line.
    pub fn move_end(&mut self) -> bool {
        let target = line_end_after_cursor(&self.line, self.cursor);
        if self.cursor == target {
            return false;
        }

        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = target;
        true
    }

    /// Move the cursor to the beginning of the whole editable buffer.
    pub fn move_buffer_start(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = 0;
        true
    }

    /// Move the cursor to the end of the whole editable buffer.
    pub fn move_buffer_end(&mut self) -> bool {
        if self.cursor == self.line.len() {
            return false;
        }
        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = self.line.len();
        true
    }

    /// Move the cursor left by one shell-style word.
    pub fn move_word_left(&mut self) -> bool {
        let target = previous_word_boundary(&self.line, self.cursor);
        if target == self.cursor {
            return false;
        }
        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = target;
        true
    }

    /// Move the cursor right by one shell-style word.
    pub fn move_word_right(&mut self) -> bool {
        let target = next_word_boundary(&self.line, self.cursor);
        if target == self.cursor {
            return false;
        }
        if self.history_cursor.is_some() {
            self.history_entry_cursor_navigation = true;
        }
        self.vertical_navigation_column = None;
        self.cursor = target;
        true
    }

    /// Move up within a multiline prompt, falling back to history navigation.
    pub fn move_row_up_or_history_previous(&mut self) -> bool {
        if self.history_cursor.is_some() && !self.history_entry_cursor_navigation {
            return self.history_previous();
        }
        let current_start = line_start_before_cursor(&self.line, self.cursor);
        let current_column = self.line[current_start..self.cursor].chars().count();
        let preferred_column = self.vertical_navigation_column.unwrap_or(current_column);
        if let Some(target) =
            previous_row_cursor_position(&self.line, self.cursor, preferred_column)
        {
            self.vertical_navigation_column = Some(preferred_column);
            self.cursor = target;
            return true;
        }
        self.vertical_navigation_column = None;
        self.history_previous()
    }

    /// Move down within a multiline prompt, falling back to history navigation.
    pub fn move_row_down_or_history_next(&mut self) -> bool {
        if self.history_cursor.is_some() && !self.history_entry_cursor_navigation {
            return self.history_next();
        }
        let current_start = line_start_before_cursor(&self.line, self.cursor);
        let current_column = self.line[current_start..self.cursor].chars().count();
        let preferred_column = self.vertical_navigation_column.unwrap_or(current_column);
        if let Some(target) = next_row_cursor_position(&self.line, self.cursor, preferred_column) {
            self.vertical_navigation_column = Some(preferred_column);
            self.cursor = target;
            return true;
        }
        self.vertical_navigation_column = None;
        self.history_next()
    }

    /// Move up within a soft-wrapped prompt row before falling back to history.
    ///
    /// # Parameters
    /// - `columns`: Display cells available for the editable prompt body.
    pub fn move_visual_row_up_or_history_previous(&mut self, columns: usize) -> bool {
        if self.history_cursor.is_some() && !self.history_entry_cursor_navigation {
            return self.history_previous();
        }
        let columns = columns.max(1);
        let current_start = line_start_before_cursor(&self.line, self.cursor);
        let current_end = line_end_after_cursor(&self.line, self.cursor);
        let rows = visual_rows_for_logical_line(&self.line, current_start, current_end, columns);
        let Some((_, current_column)) = visual_row_index_and_column(&self.line, self.cursor, &rows)
        else {
            self.vertical_navigation_column = None;
            return self.move_row_up_or_history_previous();
        };
        let preferred_column = self.vertical_navigation_column.unwrap_or(current_column);
        if let Some(target) =
            previous_visual_row_cursor_position(&self.line, self.cursor, columns, preferred_column)
        {
            self.vertical_navigation_column = Some(preferred_column);
            self.cursor = target;
            return true;
        }
        self.vertical_navigation_column = None;
        self.move_row_up_or_history_previous()
    }

    /// Move down within a soft-wrapped prompt row before falling back to history.
    ///
    /// # Parameters
    /// - `columns`: Display cells available for the editable prompt body.
    pub fn move_visual_row_down_or_history_next(&mut self, columns: usize) -> bool {
        if self.history_cursor.is_some() && !self.history_entry_cursor_navigation {
            return self.history_next();
        }
        let columns = columns.max(1);
        let current_start = line_start_before_cursor(&self.line, self.cursor);
        let current_end = line_end_after_cursor(&self.line, self.cursor);
        let rows = visual_rows_for_logical_line(&self.line, current_start, current_end, columns);
        let Some((_, current_column)) = visual_row_index_and_column(&self.line, self.cursor, &rows)
        else {
            self.vertical_navigation_column = None;
            return self.move_row_down_or_history_next();
        };
        let preferred_column = self.vertical_navigation_column.unwrap_or(current_column);
        if let Some(target) =
            next_visual_row_cursor_position(&self.line, self.cursor, columns, preferred_column)
        {
            self.vertical_navigation_column = Some(preferred_column);
            self.cursor = target;
            return true;
        }
        self.vertical_navigation_column = None;
        self.move_row_down_or_history_next()
    }

    /// Delete the shell-style word before the cursor.
    pub fn kill_word_left(&mut self) -> bool {
        let target = previous_word_boundary(&self.line, self.cursor);
        if target == self.cursor {
            return false;
        }
        self.leave_history_navigation_for_edit();
        self.line.replace_range(target..self.cursor, "");
        self.cursor = target;
        self.cleanup_unused_paste_blocks();
        true
    }

    /// Delete the shell-style word after the cursor.
    pub fn kill_word_right(&mut self) -> bool {
        let target = next_word_boundary(&self.line, self.cursor);
        if target == self.cursor {
            return false;
        }
        self.leave_history_navigation_for_edit();
        self.line.replace_range(self.cursor..target, "");
        self.cleanup_unused_paste_blocks();
        true
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        self.leave_history_navigation_for_edit();
        let start = previous_boundary(&self.line, self.cursor);
        self.line.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.cleanup_unused_paste_blocks();
        true
    }

    /// Delete the character under the cursor.
    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.line.len() {
            return false;
        }

        self.leave_history_navigation_for_edit();
        let end = next_boundary(&self.line, self.cursor);
        self.line.replace_range(self.cursor..end, "");
        self.cleanup_unused_paste_blocks();
        true
    }

    /// Delete all text before the cursor.
    pub fn kill_to_start(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        self.leave_history_navigation_for_edit();
        self.line.replace_range(0..self.cursor, "");
        self.cursor = 0;
        self.cleanup_unused_paste_blocks();
        true
    }

    /// Delete all text after the cursor.
    pub fn kill_to_end(&mut self) -> bool {
        if self.cursor == self.line.len() {
            return false;
        }

        self.leave_history_navigation_for_edit();
        self.line.truncate(self.cursor);
        self.cleanup_unused_paste_blocks();
        true
    }

    /// Move to the previous submitted history entry.
    pub fn history_previous(&mut self) -> bool {
        if self.history.entries.is_empty() {
            return false;
        }

        let next_index = match self.history_cursor {
            Some(0) => return false,
            Some(index) => index - 1,
            None => {
                self.draft_before_history = self.line.clone();
                self.draft_before_history_paste_blocks = self.paste_blocks.clone();
                self.draft_before_history_next_paste_block_id = self.next_paste_block_id;
                self.history.entries.len() - 1
            }
        };
        self.load_history_index(next_index);
        true
    }

    /// Move to the next submitted history entry or restore the draft line.
    pub fn history_next(&mut self) -> bool {
        let Some(index) = self.history_cursor else {
            return false;
        };

        if index + 1 < self.history.entries.len() {
            self.load_history_index(index + 1);
            return true;
        }

        self.line = self.draft_before_history.clone();
        self.paste_blocks = self.draft_before_history_paste_blocks.clone();
        self.next_paste_block_id = self.draft_before_history_next_paste_block_id;
        self.cursor = self.line.len();
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.vertical_navigation_column = None;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
        true
    }

    /// Search backward through submitted history using the current draft as a
    /// case-insensitive substring query.
    pub fn history_search_backward(&mut self) -> bool {
        if self.history.entries.is_empty() {
            return false;
        }
        let query = match self.history_cursor {
            Some(_) => self.draft_before_history.clone(),
            None => {
                self.draft_before_history = self.line.clone();
                self.draft_before_history_paste_blocks = self.paste_blocks.clone();
                self.draft_before_history_next_paste_block_id = self.next_paste_block_id;
                self.line.clone()
            }
        };
        if query.is_empty() {
            return self.history_previous();
        }
        let normalized_query = query.to_lowercase();
        let mut index = self.history_cursor.unwrap_or(self.history.entries.len());
        while index > 0 {
            index -= 1;
            if normalized_history_entry_contains_query_substring(
                &self.history.normalized_entries[index],
                &normalized_query,
            ) {
                self.load_history_index(index);
                return true;
            }
        }
        false
    }

    /// Returns the nearest earlier history entry that contains `query` as a
    /// case-insensitive substring.
    ///
    /// # Parameters
    /// - `query`: Search text to match. Empty queries match any history entry.
    /// - `before`: Exclusive upper-bound index for the search.
    pub fn history_substring_match_before(&self, query: &str, before: usize) -> Option<usize> {
        let normalized_query = query.to_lowercase();
        let mut index = before.min(self.history.entries.len());
        while index > 0 {
            index -= 1;
            if normalized_history_entry_contains_query_substring(
                &self.history.normalized_entries[index],
                &normalized_query,
            ) {
                return Some(index);
            }
        }
        None
    }

    /// Returns the nearest later history entry that contains `query` as a
    /// case-insensitive substring.
    ///
    /// # Parameters
    /// - `query`: Search text to match. Empty queries match any history entry.
    /// - `after`: Exclusive lower-bound index for the search.
    pub fn history_substring_match_after(&self, query: &str, after: usize) -> Option<usize> {
        let normalized_query = query.to_lowercase();
        let start = after.saturating_add(1);
        (start..self.history.entries.len()).find(|index| {
            normalized_history_entry_contains_query_substring(
                &self.history.normalized_entries[*index],
                &normalized_query,
            )
        })
    }

    /// Loads one history entry as an incremental search match.
    ///
    /// # Parameters
    /// - `index`: History entry index to load.
    /// - `draft_line`: Prompt text to restore when history navigation returns
    ///   beyond the newest match.
    pub fn load_history_search_match(&mut self, index: usize, draft: &ReadlineDraft) -> bool {
        if self.history.entries.get(index).is_none() {
            return false;
        }
        self.draft_before_history = draft.line.clone();
        self.draft_before_history_paste_blocks = draft.paste_blocks.clone();
        self.draft_before_history_next_paste_block_id = draft.next_paste_block_id;
        self.load_history_index(index);
        true
    }

    /// Captures the exact editable representation active before incremental search.
    pub fn draft_snapshot(&self) -> ReadlineDraft {
        ReadlineDraft {
            line: self.line.clone(),
            cursor: self.cursor,
            paste_blocks: self.paste_blocks.clone(),
            next_paste_block_id: self.next_paste_block_id,
        }
    }

    /// Returns the expanded raw text represented by one draft snapshot.
    pub fn expanded_draft(draft: &ReadlineDraft) -> String {
        render_line_with_blocks(&draft.line, &draft.paste_blocks, RenderLineMode::Expanded)
    }

    /// Restores the exact draft that was active before incremental search.
    pub fn restore_history_search_draft(&mut self, draft: &ReadlineDraft) {
        self.line = draft.line.clone();
        self.cursor = draft.cursor;
        self.paste_blocks = draft.paste_blocks.clone();
        self.next_paste_block_id = draft.next_paste_block_id;
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.vertical_navigation_column = None;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
    }

    /// Submit the current line, reset editing state, and append to history.
    pub fn submit(&mut self) -> String {
        self.submit_with_display().0
    }

    /// Submit the current line while retaining raw, display, and paste-range forms.
    pub fn submit_with_display(&mut self) -> (String, String, Vec<ReadlinePasteRange>) {
        let submitted = self.expanded_line();
        let display = self.rendered_line();
        let collapsed_paste_ranges = self.collapsed_paste_ranges();
        self.line.clear();
        self.cursor = 0;
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.vertical_navigation_column = None;
        self.draft_before_history.clear();
        self.paste_blocks.clear();
        self.draft_before_history_paste_blocks.clear();

        self.remember_submission(ReadlineHistoryEntry {
            text: submitted.clone(),
            collapsed_paste_ranges: collapsed_paste_ranges.clone(),
        });

        (submitted, display, collapsed_paste_ranges)
    }

    /// Runs the remember submission operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn remember_submission(&mut self, submitted: ReadlineHistoryEntry) {
        if self.history_limit == 0
            || submitted.text.is_empty()
            || submitted.text.len() > MAX_READLINE_HISTORY_ENTRY_BYTES
            || !valid_paste_ranges(&submitted.text, &submitted.collapsed_paste_ranges)
        {
            return;
        }
        if self.history.entries.last() == Some(&submitted.text) {
            if let Some(ranges) = Arc::make_mut(&mut self.history.0)
                .collapsed_paste_ranges
                .last_mut()
            {
                *ranges = submitted.collapsed_paste_ranges;
            }
            return;
        }
        let history = Arc::make_mut(&mut self.history.0);
        history.bytes = history.bytes.saturating_add(submitted.text.len());
        history
            .normalized_entries
            .push(submitted.text.to_lowercase());
        history.entries.push(submitted.text);
        history
            .collapsed_paste_ranges
            .push(submitted.collapsed_paste_ranges);
        while history.entries.len() > self.history_limit
            || history.bytes > MAX_READLINE_HISTORY_BYTES
        {
            let removed = history.entries.remove(0);
            history.bytes = history.bytes.saturating_sub(removed.len());
            history.normalized_entries.remove(0);
            history.collapsed_paste_ranges.remove(0);
        }
    }

    /// Runs the load history index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn load_history_index(&mut self, index: usize) {
        let Some(entry) = self.structured_history_entry(index) else {
            return;
        };
        self.replace_current_line_with_history_entry(&entry);
        self.cursor = self.line.len();
        self.history_cursor = Some(index);
        self.history_entry_cursor_navigation = false;
    }

    /// Runs the leave history navigation for edit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn leave_history_navigation_for_edit(&mut self) {
        self.history_cursor = None;
        self.history_entry_cursor_navigation = false;
        self.vertical_navigation_column = None;
        self.draft_before_history.clear();
        self.draft_before_history_paste_blocks.clear();
    }

    /// Replaces the active line with literal programmatic text.
    fn replace_current_line_with_text(&mut self, text: String) {
        self.line = text;
        self.cursor = self.line.len();
        self.vertical_navigation_column = None;
        self.paste_blocks.clear();
    }

    /// Reconstructs one history entry from explicit raw-text paste ranges.
    fn replace_current_line_with_history_entry(&mut self, entry: &ReadlineHistoryEntry) {
        self.line.clear();
        self.cursor = 0;
        self.vertical_navigation_column = None;
        self.paste_blocks.clear();
        let mut copied = 0;
        for range in &entry.collapsed_paste_ranges {
            self.line.push_str(&entry.text[copied..range.start]);
            self.cursor = self.line.len();
            self.insert_paste_block(entry.text[range.start..range.end].to_string());
            copied = range.end;
        }
        self.line.push_str(&entry.text[copied..]);
        self.cursor = self.line.len();
    }

    /// Maps internal paste markers to byte ranges in expanded raw text.
    fn collapsed_paste_ranges(&self) -> Vec<ReadlinePasteRange> {
        let mut ranges = Vec::new();
        let mut raw_offset = 0usize;
        for ch in self.line.chars() {
            if let Some(block) = self.paste_block_for_marker(ch) {
                let end = raw_offset.saturating_add(block.content.len());
                ranges.push(ReadlinePasteRange {
                    start: raw_offset,
                    end,
                });
                raw_offset = end;
            } else {
                raw_offset = raw_offset.saturating_add(ch.len_utf8());
            }
        }
        ranges
    }

    /// Returns whether one pasted payload would exceed the visible prompt height.
    fn pasted_text_exceeds_visible_prompt_height(text: &str) -> bool {
        text.split('\n')
            .nth(READLINE_PASTE_BLOCK_THRESHOLD_LINES)
            .is_some()
    }

    /// Inserts one opaque pasted block at the cursor.
    fn insert_paste_block(&mut self, content: String) {
        let marker = self.next_paste_block_marker();
        self.paste_blocks
            .push(ReadlinePasteBlock { marker, content });
        self.line.insert(self.cursor, marker);
        self.cursor += marker.len_utf8();
    }

    /// Allocates one private-use marker for an opaque pasted block.
    fn next_paste_block_marker(&mut self) -> char {
        loop {
            let candidate = char::from_u32(
                READLINE_PASTE_BLOCK_MARKER_BASE.saturating_add(self.next_paste_block_id),
            )
            .unwrap_or('\u{f0000}');
            self.next_paste_block_id = self.next_paste_block_id.saturating_add(1);
            if !self.line.contains(candidate) && self.paste_block_for_marker(candidate).is_none() {
                return candidate;
            }
        }
    }

    /// Removes pasted payloads whose internal markers are no longer present.
    fn cleanup_unused_paste_blocks(&mut self) {
        if self.paste_blocks.is_empty() {
            return;
        }
        let markers = self.line.chars().collect::<BTreeSet<_>>();
        self.paste_blocks
            .retain(|block| markers.contains(&block.marker));
    }

    /// Returns the pasted block associated with one internal marker.
    fn paste_block_for_marker(&self, marker: char) -> Option<&ReadlinePasteBlock> {
        self.paste_blocks
            .iter()
            .find(|block| block.marker == marker)
    }

    /// Runs the bool outcome operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn bool_outcome(changed: bool) -> ReadlineOutcome {
        if changed {
            ReadlineOutcome::Edited
        } else {
            ReadlineOutcome::Noop
        }
    }
}

/// Returns whether one history entry should appear for an incremental search
/// query.
///
/// Reverse search keeps readline's recency-oriented traversal and requires a
/// case-insensitive substring match anywhere in the history entry.
fn normalized_history_entry_contains_query_substring(
    normalized_entry: &str,
    normalized_query: &str,
) -> bool {
    normalized_entry.contains(normalized_query)
}

/// Validates persisted collapsed-paste ranges against one raw prompt.
fn valid_paste_ranges(text: &str, ranges: &[ReadlinePasteRange]) -> bool {
    let mut previous_end = 0usize;
    ranges.iter().all(|range| {
        let valid = range.start < range.end
            && range.start >= previous_end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);
        previous_end = range.end;
        valid
    })
}

/// Renders a raw history entry from its explicit collapsed-paste ranges.
fn render_history_entry(entry: &ReadlineHistoryEntry) -> String {
    if !entry.is_valid() {
        return entry.text.clone();
    }
    let mut rendered = String::new();
    let mut copied = 0usize;
    for range in &entry.collapsed_paste_ranges {
        rendered.push_str(&entry.text[copied..range.start]);
        rendered.push_str(&paste_block_label(range.end.saturating_sub(range.start)));
        copied = range.end;
    }
    rendered.push_str(&entry.text[copied..]);
    rendered
}

/// Selects whether pasted block markers render as labels or exact content.
enum RenderLineMode {
    /// Render pasted blocks as labels.
    Collapsed,
    /// Render pasted blocks as exact content.
    Expanded,
}

/// Renders one internal line by replacing pasted block markers.
fn render_line_with_blocks(
    line: &str,
    paste_blocks: &[ReadlinePasteBlock],
    mode: RenderLineMode,
) -> String {
    let mut rendered = String::new();
    for ch in line.chars() {
        if let Some(block) = paste_blocks.iter().find(|block| block.marker == ch) {
            match mode {
                RenderLineMode::Collapsed => {
                    rendered.push_str(&paste_block_label(block.content.len()));
                }
                RenderLineMode::Expanded => rendered.push_str(&block.content),
            }
        } else {
            rendered.push(ch);
        }
    }
    rendered
}

/// Renders one internal line while inserting display text at a byte offset.
fn render_line_with_insert(
    line: &str,
    paste_blocks: &[ReadlinePasteBlock],
    insert_at: usize,
    insert_text: &str,
) -> String {
    let mut rendered = String::new();
    for (index, ch) in line.char_indices() {
        if index == insert_at {
            rendered.push_str(insert_text);
        }
        if let Some(block) = paste_blocks.iter().find(|block| block.marker == ch) {
            rendered.push_str(&paste_block_label(block.content.len()));
        } else {
            rendered.push(ch);
        }
    }
    if insert_at == line.len() {
        rendered.push_str(insert_text);
    }
    rendered
}

/// Formats the prompt label for one collapsed pasted block.
fn paste_block_label(bytes: usize) -> String {
    if bytes < 1024 {
        return format!("[Pasted {bytes} B]");
    }
    let kib = bytes as f64 / 1024.0;
    if kib < 1024.0 {
        return format!("[Pasted {kib:.1} KiB]");
    }
    let mib = kib / 1024.0;
    format!("[Pasted {mib:.1} MiB]")
}

/// Runs the previous boundary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn previous_boundary(text: &str, cursor: usize) -> usize {
    let bounded = cursor.min(text.len());
    if bounded == 0 {
        return 0;
    }

    text[..bounded]
        .char_indices()
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// Runs the next boundary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn next_boundary(text: &str, cursor: usize) -> usize {
    let bounded = cursor.min(text.len());
    if bounded >= text.len() {
        return text.len();
    }

    text[bounded..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| bounded + offset)
        .unwrap_or(text.len())
}

/// Returns the byte offset for the start of the logical row containing cursor.
fn line_start_before_cursor(text: &str, cursor: usize) -> usize {
    let bounded = cursor.min(text.len());
    text[..bounded]
        .rfind('\n')
        .map(|index| index.saturating_add(1))
        .unwrap_or(0)
}

/// Returns the byte offset for the end of the logical row containing cursor.
fn line_end_after_cursor(text: &str, cursor: usize) -> usize {
    let bounded = cursor.min(text.len());
    text[bounded..]
        .find('\n')
        .map(|offset| bounded.saturating_add(offset))
        .unwrap_or(text.len())
}

/// Returns the previous shell-style word boundary before cursor.
fn previous_word_boundary(text: &str, cursor: usize) -> usize {
    let mut position = cursor.min(text.len());
    while let Some((previous, ch)) = previous_char(text, position) {
        if !ch.is_whitespace() {
            break;
        }
        position = previous;
    }
    let Some((_, ch)) = previous_char(text, position) else {
        return position;
    };
    if readline_word_is_identifier(ch) {
        while let Some((previous, ch)) = previous_char(text, position) {
            if !readline_word_is_identifier(ch) {
                break;
            }
            position = previous;
        }
        return position;
    }
    while let Some((previous, ch)) = previous_char(text, position) {
        if !readline_word_is_symbol(ch) {
            break;
        }
        position = previous;
    }
    position
}

/// Returns the next shell-style word boundary after cursor.
fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let mut position = cursor.min(text.len());
    while let Some((next, ch)) = next_char(text, position) {
        if !ch.is_whitespace() {
            break;
        }
        position = next;
    }
    let Some((_, ch)) = next_char(text, position) else {
        return position;
    };
    if readline_word_is_identifier(ch) {
        while let Some((next, ch)) = next_char(text, position) {
            if !readline_word_is_identifier(ch) {
                break;
            }
            position = next;
        }
        return position;
    }
    while let Some((next, ch)) = next_char(text, position) {
        if !readline_word_is_symbol(ch) {
            break;
        }
        position = next;
    }
    position
}

/// Returns whether one character belongs to a readline word token.
fn readline_word_is_identifier(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Returns whether one character belongs to a punctuation token between words.
fn readline_word_is_symbol(ch: char) -> bool {
    !ch.is_whitespace() && !readline_word_is_identifier(ch)
}

/// Returns the previous UTF-8 character start and value before cursor.
fn previous_char(text: &str, cursor: usize) -> Option<(usize, char)> {
    let bounded = cursor.min(text.len());
    text[..bounded].char_indices().last()
}

/// Returns the next UTF-8 character end and value at cursor.
fn next_char(text: &str, cursor: usize) -> Option<(usize, char)> {
    let bounded = cursor.min(text.len());
    let ch = text[bounded..].chars().next()?;
    Some((bounded.saturating_add(ch.len_utf8()), ch))
}

/// Returns the cursor location one logical row above while preserving column.
fn previous_row_cursor_position(text: &str, cursor: usize, column: usize) -> Option<usize> {
    let current_start = line_start_before_cursor(text, cursor);
    if current_start == 0 {
        return None;
    }
    let previous_end = current_start.saturating_sub(1);
    let previous_start = line_start_before_cursor(text, previous_end);
    Some(byte_index_for_column(
        text,
        previous_start,
        previous_end,
        column,
    ))
}

/// Returns the cursor location one logical row below while preserving column.
fn next_row_cursor_position(text: &str, cursor: usize, column: usize) -> Option<usize> {
    let current_end = line_end_after_cursor(text, cursor);
    if current_end >= text.len() {
        return None;
    }
    let next_start = current_end.saturating_add(1);
    let next_end = line_end_after_cursor(text, next_start);
    Some(byte_index_for_column(text, next_start, next_end, column))
}

/// One soft-wrapped visual row inside a logical prompt row.
#[derive(Debug, Clone, Copy)]
struct VisualRow {
    /// Byte offset where this visual row starts.
    start: usize,
    /// Byte offset where this visual row ends.
    end: usize,
    /// Whether a cursor at `end` should be treated as belonging to the next row.
    end_cursor_belongs_to_next: bool,
}

/// Returns the cursor location one visual row above while preserving column.
fn previous_visual_row_cursor_position(
    text: &str,
    cursor: usize,
    columns: usize,
    column: usize,
) -> Option<usize> {
    let columns = columns.max(1);
    let current_start = line_start_before_cursor(text, cursor);
    let current_end = line_end_after_cursor(text, cursor);
    let rows = visual_rows_for_logical_line(text, current_start, current_end, columns);
    let (row_index, _) = visual_row_index_and_column(text, cursor, &rows)?;
    if row_index > 0 {
        return Some(byte_index_for_display_column(
            text,
            rows[row_index - 1].start,
            rows[row_index - 1].end,
            column,
        ));
    }
    if current_start == 0 {
        return None;
    }
    let previous_end = current_start.saturating_sub(1);
    let previous_start = line_start_before_cursor(text, previous_end);
    let previous_rows = visual_rows_for_logical_line(text, previous_start, previous_end, columns);
    let previous_row = previous_rows.last()?;
    Some(byte_index_for_display_column(
        text,
        previous_row.start,
        previous_row.end,
        column,
    ))
}

/// Returns the cursor location one visual row below while preserving column.
fn next_visual_row_cursor_position(
    text: &str,
    cursor: usize,
    columns: usize,
    column: usize,
) -> Option<usize> {
    let columns = columns.max(1);
    let current_start = line_start_before_cursor(text, cursor);
    let current_end = line_end_after_cursor(text, cursor);
    let rows = visual_rows_for_logical_line(text, current_start, current_end, columns);
    let boundary_row_index = rows.iter().enumerate().find_map(|(index, row)| {
        if index > 0
            && cursor == row.start
            && rows[index - 1].end == row.start
            && column >= display_width_between(text, rows[index - 1].start, rows[index - 1].end)
        {
            Some(index - 1)
        } else {
            None
        }
    });
    let row_index = boundary_row_index
        .or_else(|| visual_row_index_and_column(text, cursor, &rows).map(|(index, _)| index))?;
    let next_row = if let Some(next_row) = rows.get(row_index.saturating_add(1)) {
        *next_row
    } else {
        if current_end >= text.len() {
            return None;
        }
        let next_start = current_end.saturating_add(1);
        let next_end = line_end_after_cursor(text, next_start);
        let next_rows = visual_rows_for_logical_line(text, next_start, next_end, columns);
        *next_rows.first()?
    };
    Some(byte_index_for_display_column(
        text,
        next_row.start,
        next_row.end,
        column,
    ))
}

/// Returns visual rows for one logical line using whitespace-preferred wrapping.
fn visual_rows_for_logical_line(
    text: &str,
    start: usize,
    end: usize,
    columns: usize,
) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    let mut row_start = start;
    while row_start < end {
        let (row_end, consumed, end_cursor_belongs_to_next) =
            visual_row_end(text, row_start, end, columns);
        rows.push(VisualRow {
            start: row_start,
            end: row_end,
            end_cursor_belongs_to_next,
        });
        row_start = consumed;
    }
    if rows.is_empty() {
        rows.push(VisualRow {
            start,
            end,
            end_cursor_belongs_to_next: false,
        });
    }
    rows
}

/// Returns the visible row end and consumed byte offset for one soft row.
fn visual_row_end(text: &str, start: usize, end: usize, columns: usize) -> (usize, usize, bool) {
    let mut width = 0usize;
    let mut boundary = start;
    let mut last_space_break = None;
    for (relative, ch) in text[start..end].char_indices() {
        let index = start.saturating_add(relative);
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if width > 0 && width.saturating_add(char_width) > columns {
            break;
        }
        let next = index.saturating_add(ch.len_utf8());
        if ch.is_whitespace() && width > 0 {
            last_space_break = Some((index, next));
        }
        boundary = next;
        width = width.saturating_add(char_width);
        if width >= columns {
            break;
        }
    }
    if boundary >= end {
        return (boundary, boundary, false);
    }
    if let Some((_, space_end)) = last_space_break.filter(|(space_start, _)| *space_start > start) {
        return (space_end, space_end, space_end < end);
    }
    (boundary, boundary, boundary < end)
}

/// Returns the visual row index and display column for the cursor.
fn visual_row_index_and_column(
    text: &str,
    cursor: usize,
    rows: &[VisualRow],
) -> Option<(usize, usize)> {
    for (index, row) in rows.iter().enumerate() {
        if cursor == row.end
            && row.end_cursor_belongs_to_next
            && rows
                .get(index.saturating_add(1))
                .is_some_and(|next| next.start == row.end)
        {
            continue;
        }
        if cursor >= row.start && cursor <= row.end {
            return Some((index, display_width_between(text, row.start, cursor)));
        }
    }
    None
}

/// Converts a display column inside a byte range to a UTF-8 byte offset.
fn byte_index_for_display_column(text: &str, start: usize, end: usize, column: usize) -> usize {
    let mut width = 0usize;
    for (relative, ch) in text[start..end].char_indices() {
        if width >= column {
            return start.saturating_add(relative);
        }
        width = width.saturating_add(UnicodeWidthChar::width(ch).unwrap_or(0).max(1));
    }
    end
}

/// Returns the display width of a byte range.
fn display_width_between(text: &str, start: usize, end: usize) -> usize {
    text[start..end]
        .chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0).max(1))
        .sum()
}

/// Converts a display column inside a logical row to a UTF-8 byte offset.
fn byte_index_for_column(text: &str, start: usize, end: usize, column: usize) -> usize {
    let mut consumed = 0usize;
    for (offset, _) in text[start..end].char_indices() {
        if consumed == column {
            return start.saturating_add(offset);
        }
        consumed = consumed.saturating_add(1);
    }
    end
}
