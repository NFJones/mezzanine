//! Prompt editing state independent of product selector and rendering policy.
//!
//! This module owns the state transitions shared by command and multiline
//! prompt surfaces: buffer ownership, reverse history search, baseline terminal
//! input, multiline insertion, Escape clearing, and visible-row navigation.
//! Product crates remain responsible for prefixes, completion candidates, and
//! selector presentation.

use super::{
    ReadlineBuffer, ReadlineEdit, ReadlineOutcome, apply_readline_terminal_input,
    readline_input_is_ctrl_r, readline_input_is_ctrl_shift_r,
};
use crate::{MuxError, Result};

/// Editing behavior selected by the product prompt adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadlinePromptMode {
    /// Newline submits the current buffer and Escape follows baseline handling.
    SingleLine,
    /// Newline inserts text, Escape clears a non-empty draft, and vertical
    /// arrows traverse visible rows before prompt history.
    Multiline,
}

/// Product-independent state for one readline prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlinePromptState {
    /// Editable prompt contents and retained history.
    pub buffer: ReadlineBuffer,
    reverse_search: Option<ReadlineReverseSearch>,
    prompt_body_columns: Option<usize>,
}

/// Incremental reverse-history-search state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadlineReverseSearch {
    draft_line: String,
    draft_cursor: usize,
    query: String,
    matched_index: Option<usize>,
}

impl Default for ReadlinePromptState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadlinePromptState {
    /// Creates empty prompt state with default history retention.
    pub fn new() -> Self {
        Self {
            buffer: ReadlineBuffer::new(),
            reverse_search: None,
            prompt_body_columns: None,
        }
    }

    /// Records display cells available for a multiline editable body.
    pub fn set_prompt_body_columns(&mut self, columns: usize) {
        self.prompt_body_columns = Some(columns.max(1));
    }

    /// Returns whether incremental reverse search is active.
    pub fn reverse_search_active(&self) -> bool {
        self.reverse_search.is_some()
    }

    /// Renders the active reverse-search line, if any.
    pub fn rendered_reverse_search(&self) -> Option<String> {
        let search = self.reverse_search.as_ref()?;
        let item = search
            .matched_index
            .and_then(|index| self.buffer.history().get(index))
            .map(String::as_str)
            .unwrap_or_default();
        Some(format!("(reverse-i-search'{}'): {item}", search.query))
    }

    /// Returns the cursor column within an active reverse-search line.
    pub fn reverse_search_cursor_column(&self) -> Option<usize> {
        let search = self.reverse_search.as_ref()?;
        Some(
            unicode_width::UnicodeWidthStr::width("(reverse-i-search'")
                .saturating_add(unicode_width::UnicodeWidthStr::width(search.query.as_str())),
        )
    }

    /// Applies reverse-search input before product selector policy runs.
    pub fn apply_reverse_search_input(&mut self, input: &[u8]) -> Result<Option<ReadlineOutcome>> {
        if readline_input_is_ctrl_shift_r(input) {
            return Ok(Some(self.reverse_search_step(true)));
        }
        if readline_input_is_ctrl_r(input) {
            return Ok(Some(self.reverse_search_step(false)));
        }
        if self.reverse_search.is_none() {
            return Ok(None);
        }
        match input {
            b"\r" | b"\n" | b"\r\n" => {
                self.reverse_search = None;
                return Ok(Some(ReadlineOutcome::Edited));
            }
            b"\x03" | b"\x1b" => {
                self.cancel_reverse_search();
                return Ok(Some(ReadlineOutcome::Edited));
            }
            b"\t" => return Ok(Some(self.reverse_search_step(true))),
            b"\x1b[Z" => return Ok(Some(self.reverse_search_step(false))),
            b"\x7f" | b"\x08" => return Ok(Some(self.reverse_search_backspace())),
            b"\x06" | b"\x1b[C" => {
                self.reverse_search = None;
                return Ok(Some(ReadlineOutcome::Edited));
            }
            b"\x1b[A" | b"\x1b[B" | b"\x02" | b"\x1b[D" => {
                self.cancel_reverse_search();
                return Ok(Some(ReadlineOutcome::Edited));
            }
            _ => {}
        }
        if input.iter().any(|byte| byte.is_ascii_control()) {
            return Ok(Some(ReadlineOutcome::Noop));
        }
        let text = std::str::from_utf8(input)
            .map_err(|_| MuxError::invalid_args("readline input is not valid UTF-8 text"))?;
        if text.is_empty() {
            return Ok(Some(ReadlineOutcome::Noop));
        }
        self.ensure_reverse_search();
        if let Some(search) = self.reverse_search.as_mut() {
            search.query.push_str(text);
            search.matched_index = None;
        }
        self.refresh_reverse_search_match();
        Ok(Some(ReadlineOutcome::Edited))
    }

    /// Applies prompt-mode-specific input before baseline readline bindings.
    pub fn apply_mode_input(
        &mut self,
        mode: ReadlinePromptMode,
        input: &[u8],
    ) -> Option<ReadlineOutcome> {
        if mode != ReadlinePromptMode::Multiline {
            return None;
        }
        if input == b"\n" {
            return Some(
                self.buffer
                    .apply(ReadlineEdit::InsertText("\n".to_string())),
            );
        }
        if input == b"\x1b" {
            if self.buffer.line().is_empty() {
                return Some(ReadlineOutcome::Noop);
            }
            self.buffer.set_line("");
            return Some(ReadlineOutcome::Edited);
        }
        match input {
            b"\x1b[A" | b"\x1bOA" => self.prompt_body_columns.map(|columns| {
                ReadlineOutcome::from(self.buffer.move_visual_row_up_or_history_previous(columns))
            }),
            b"\x1b[B" | b"\x1bOB" => self.prompt_body_columns.map(|columns| {
                ReadlineOutcome::from(self.buffer.move_visual_row_down_or_history_next(columns))
            }),
            _ => None,
        }
    }

    /// Applies baseline readline bindings to the owned buffer.
    pub fn apply_terminal_input(&mut self, input: &[u8]) -> Result<ReadlineOutcome> {
        apply_readline_terminal_input(&mut self.buffer, input)
    }

    fn ensure_reverse_search(&mut self) {
        if self.reverse_search.is_some() {
            return;
        }
        let draft_line = self.buffer.expanded_line();
        let draft_cursor = self.buffer.cursor();
        self.reverse_search = Some(ReadlineReverseSearch {
            query: draft_line.clone(),
            draft_line,
            draft_cursor,
            matched_index: None,
        });
    }

    fn reverse_search_step(&mut self, forward: bool) -> ReadlineOutcome {
        self.ensure_reverse_search();
        let Some(search) = self.reverse_search.as_ref() else {
            return ReadlineOutcome::Noop;
        };
        let query = search.query.clone();
        let next = if forward {
            search
                .matched_index
                .and_then(|index| self.buffer.history_substring_match_after(&query, index))
        } else {
            let before = search.matched_index.unwrap_or(self.buffer.history().len());
            self.buffer.history_substring_match_before(&query, before)
        };
        self.set_reverse_search_match(next)
    }

    fn reverse_search_backspace(&mut self) -> ReadlineOutcome {
        self.ensure_reverse_search();
        if let Some(search) = self.reverse_search.as_mut() {
            if search.query.pop().is_none() {
                return ReadlineOutcome::Noop;
            }
            search.matched_index = None;
        }
        self.refresh_reverse_search_match();
        ReadlineOutcome::Edited
    }

    fn refresh_reverse_search_match(&mut self) {
        let Some(search) = self.reverse_search.as_ref() else {
            return;
        };
        let next = self
            .buffer
            .history_substring_match_before(&search.query, self.buffer.history().len());
        let _ = self.set_reverse_search_match(next);
    }

    fn set_reverse_search_match(&mut self, next: Option<usize>) -> ReadlineOutcome {
        let Some(search) = self.reverse_search.as_mut() else {
            return ReadlineOutcome::Noop;
        };
        if search.matched_index == next {
            return ReadlineOutcome::Noop;
        }
        search.matched_index = next;
        if let Some(index) = next {
            let _ = self
                .buffer
                .load_history_search_match(index, &search.draft_line);
        } else {
            self.buffer
                .restore_history_search_draft(&search.draft_line, search.draft_cursor);
        }
        ReadlineOutcome::Edited
    }

    fn cancel_reverse_search(&mut self) {
        let Some(search) = self.reverse_search.take() else {
            return;
        };
        self.buffer
            .restore_history_search_draft(&search.draft_line, search.draft_cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies reverse search preserves and restores a draft while cycling
    /// history independently from any product selector implementation.
    #[test]
    fn prompt_state_owns_reverse_search_transitions() {
        let mut state = ReadlinePromptState::new();
        state.buffer.set_history(vec![
            "list files".to_string(),
            "show list sessions".to_string(),
        ]);
        state.buffer.set_line("li");

        assert_eq!(
            state.apply_reverse_search_input(b"\x12").unwrap(),
            Some(ReadlineOutcome::Edited)
        );
        assert_eq!(state.buffer.line(), "show list sessions");
        assert_eq!(
            state.rendered_reverse_search().as_deref(),
            Some("(reverse-i-search'li'): show list sessions")
        );
        assert_eq!(
            state.apply_reverse_search_input(b"\x1b").unwrap(),
            Some(ReadlineOutcome::Edited)
        );
        assert_eq!(state.buffer.line(), "li");
    }

    /// Verifies multiline prompt policy inserts newlines and traverses visible
    /// rows before falling back to retained history.
    #[test]
    fn prompt_state_owns_multiline_input_policy() {
        let mut state = ReadlinePromptState::new();
        state.set_prompt_body_columns(12);
        state
            .buffer
            .set_history(vec!["previous prompt".to_string()]);
        state.buffer.set_line("first line\nsecond line wraps");

        assert_eq!(
            state.apply_mode_input(ReadlinePromptMode::Multiline, b"\x1b[A"),
            Some(ReadlineOutcome::Edited)
        );
        assert_eq!(state.buffer.line(), "first line\nsecond line wraps");
        assert_eq!(
            state.apply_mode_input(ReadlinePromptMode::Multiline, b"\n"),
            Some(ReadlineOutcome::Edited)
        );
        assert!(state.buffer.line().contains('\n'));
    }
}
