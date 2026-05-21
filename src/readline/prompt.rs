//! Prompt rendering and prompt-scoped terminal input application.
//!
//! Prompt behavior wraps a buffer with surface-specific rendering rules for
//! command and pane-local agent input.

use crate::error::Result;
use crate::selector::{
    ActiveSelector, SelectorExtraCandidate, SelectorShadowHint, SelectorSurface,
    shadow_hint_with_extra,
};

use super::decoder::{
    apply_readline_terminal_input, readline_input_is_ctrl_r, readline_input_is_ctrl_shift_r,
};
use super::types::{
    ReadlineBuffer, ReadlineEdit, ReadlineOutcome, ReadlinePrompt, ReadlinePromptKind,
    ReadlineReverseSearch,
};
use unicode_width::UnicodeWidthStr;

impl ReadlinePrompt {
    /// Create a prompt with an empty buffer and default history retention.
    pub fn new(kind: ReadlinePromptKind) -> Self {
        Self {
            kind,
            buffer: ReadlineBuffer::new(),
            selector: None,
            selector_extra_candidates: Vec::new(),
            reverse_search: None,
            prompt_body_columns: None,
        }
    }

    /// Records display cells available for the editable prompt body.
    ///
    /// # Parameters
    /// - `columns`: Width after subtracting the prompt prefix gutter.
    pub fn set_prompt_body_columns(&mut self, columns: usize) {
        self.prompt_body_columns = Some(columns.max(1));
    }

    /// Replaces runtime-provided selector candidates for this prompt.
    pub fn set_selector_extra_candidates(
        &mut self,
        candidates: impl IntoIterator<Item = SelectorExtraCandidate>,
    ) {
        self.selector_extra_candidates = candidates.into_iter().collect();
    }

    /// Render the prompt line as plain text for a terminal status/prompt row.
    pub fn render(&self) -> String {
        if let Some(search) = self.reverse_search.as_ref() {
            return self.render_reverse_search(search);
        }
        format!("{}{}", self.prefix(), self.buffer.rendered_line())
    }

    /// Render the prompt line with transient selector shadow text.
    pub fn render_with_shadow_hint(&self) -> String {
        if let Some(search) = self.reverse_search.as_ref() {
            return self.render_reverse_search(search);
        }
        format!("{}{}", self.prefix(), self.buffer_line_with_shadow_hint())
    }

    /// Shadow hint column and length within `render_with_shadow_hint`.
    pub fn rendered_shadow_hint_columns(&self) -> Option<(usize, usize)> {
        if self.reverse_search.is_some() {
            return None;
        }
        let hint = self.shadow_hint()?;
        let line = self.buffer.line();
        let insert_at = hint.insert_at.min(line.len());
        if !line.is_char_boundary(insert_at) {
            return None;
        }
        let start = UnicodeWidthStr::width(self.prefix())
            .saturating_add(self.buffer.rendered_columns_before(insert_at));
        Some((start, UnicodeWidthStr::width(hint.text.as_str())))
    }

    /// Cursor column within the rendered prompt line.
    pub fn rendered_cursor_column(&self) -> usize {
        if let Some(search) = self.reverse_search.as_ref() {
            return UnicodeWidthStr::width("(reverse-i-search'")
                .saturating_add(UnicodeWidthStr::width(search.query.as_str()));
        }
        UnicodeWidthStr::width(self.prefix())
            .saturating_add(self.buffer.rendered_columns_before(self.buffer.cursor()))
    }

    /// Apply raw terminal input bytes using baseline readline-style bindings.
    pub fn apply_terminal_input(&mut self, input: &[u8]) -> Result<ReadlineOutcome> {
        if let Some(outcome) = self.apply_reverse_search_input(input)? {
            return Ok(outcome);
        }
        if input == b"\t" {
            return Ok(self.apply_selector_input(false));
        }
        if input == b"\x1b[Z" {
            return Ok(self.apply_selector_input(true));
        }
        self.selector = None;
        if self.kind == ReadlinePromptKind::Agent && input == b"\n" {
            return Ok(self
                .buffer
                .apply(ReadlineEdit::InsertText("\n".to_string())));
        }
        if self.kind == ReadlinePromptKind::Agent {
            match input {
                b"\x1b[A" | b"\x1bOA" => {
                    if let Some(columns) = self.prompt_body_columns {
                        return Ok(ReadlineOutcome::from(
                            self.buffer.move_visual_row_up_or_history_previous(columns),
                        ));
                    }
                }
                b"\x1b[B" | b"\x1bOB" => {
                    if let Some(columns) = self.prompt_body_columns {
                        return Ok(ReadlineOutcome::from(
                            self.buffer.move_visual_row_down_or_history_next(columns),
                        ));
                    }
                }
                _ => {}
            }
        }
        apply_readline_terminal_input(&mut self.buffer, input)
    }

    /// Reports whether the prompt is currently in incremental reverse search.
    pub fn reverse_search_active(&self) -> bool {
        self.reverse_search.is_some()
    }

    /// Renders the active reverse-search prompt.
    ///
    /// # Parameters
    /// - `search`: Search state to render.
    fn render_reverse_search(&self, search: &ReadlineReverseSearch) -> String {
        let item = search
            .matched_index
            .and_then(|index| self.buffer.history().get(index))
            .map(String::as_str)
            .unwrap_or_default();
        format!("(reverse-i-search'{}'): {}", search.query, item)
    }

    /// Applies input while the prompt is entering or running reverse search.
    ///
    /// # Parameters
    /// - `input`: Raw terminal bytes for one prompt input event.
    fn apply_reverse_search_input(&mut self, input: &[u8]) -> Result<Option<ReadlineOutcome>> {
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
                self.accept_reverse_search();
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
                self.accept_reverse_search();
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
        let text = std::str::from_utf8(input).map_err(|_| {
            crate::error::MezError::invalid_args("readline input is not valid UTF-8 text")
        })?;
        if text.is_empty() {
            return Ok(Some(ReadlineOutcome::Noop));
        }
        self.reverse_search_insert(text);
        Ok(Some(ReadlineOutcome::Edited))
    }

    /// Starts reverse search when needed and steps to an older or newer match.
    ///
    /// # Parameters
    /// - `forward`: Whether to search toward newer history entries.
    fn reverse_search_step(&mut self, forward: bool) -> ReadlineOutcome {
        self.ensure_reverse_search();
        let Some(search) = self.reverse_search.as_ref() else {
            return ReadlineOutcome::Noop;
        };
        let query = search.query.clone();
        let next = if forward {
            search
                .matched_index
                .and_then(|index| self.buffer.history_fuzzy_match_after(&query, index))
        } else {
            let before = search.matched_index.unwrap_or(self.buffer.history().len());
            self.buffer.history_fuzzy_match_before(&query, before)
        };
        self.set_reverse_search_match(next)
    }

    /// Inserts text into the active reverse-search query.
    ///
    /// # Parameters
    /// - `text`: Text to append to the query.
    fn reverse_search_insert(&mut self, text: &str) {
        self.ensure_reverse_search();
        if let Some(search) = self.reverse_search.as_mut() {
            search.query.push_str(text);
            search.matched_index = None;
        }
        self.refresh_reverse_search_match();
    }

    /// Deletes one character from the active reverse-search query.
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

    /// Ensures an incremental search state exists.
    fn ensure_reverse_search(&mut self) {
        if self.reverse_search.is_some() {
            return;
        }
        self.selector = None;
        let draft_line = self.buffer.expanded_line();
        let draft_cursor = self.buffer.cursor();
        self.reverse_search = Some(ReadlineReverseSearch {
            query: draft_line.clone(),
            draft_line,
            draft_cursor,
            matched_index: None,
        });
    }

    /// Refreshes the selected match for the active query from newest history.
    fn refresh_reverse_search_match(&mut self) {
        let Some(search) = self.reverse_search.as_ref() else {
            return;
        };
        let query = search.query.clone();
        let next = self
            .buffer
            .history_fuzzy_match_before(&query, self.buffer.history().len());
        let _ = self.set_reverse_search_match(next);
    }

    /// Applies one selected match to the prompt buffer.
    ///
    /// # Parameters
    /// - `next`: History index to select, or `None` when no match exists.
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

    /// Accepts the selected search result and leaves incremental search mode.
    fn accept_reverse_search(&mut self) {
        self.reverse_search = None;
    }

    /// Cancels incremental search and restores the original draft.
    fn cancel_reverse_search(&mut self) {
        let Some(search) = self.reverse_search.take() else {
            return;
        };
        self.buffer
            .restore_history_search_draft(&search.draft_line, search.draft_cursor);
    }

    /// Runs the shadow hint operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn shadow_hint(&self) -> Option<SelectorShadowHint> {
        let surface = self.selector_surface()?;
        shadow_hint_with_extra(
            surface,
            self.buffer.line(),
            self.buffer.cursor(),
            &self.selector_extra_candidates,
        )
    }

    /// Runs the buffer line with shadow hint operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn buffer_line_with_shadow_hint(&self) -> String {
        let line = self.buffer.line();
        let Some(hint) = self.shadow_hint() else {
            return self.buffer.rendered_line();
        };
        if hint.insert_at > line.len() || !line.is_char_boundary(hint.insert_at) {
            return self.buffer.rendered_line();
        }
        self.buffer
            .rendered_line_with_insert(hint.insert_at, &hint.text)
            .unwrap_or_else(|| self.buffer.rendered_line())
    }

    /// Runs the apply selector input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_selector_input(&mut self, reverse: bool) -> ReadlineOutcome {
        let Some(surface) = self.selector_surface() else {
            return ReadlineOutcome::Noop;
        };
        let selector = match self.selector.as_mut() {
            Some(selector) if selector.surface == surface => {
                if reverse {
                    selector.select_previous();
                } else {
                    selector.select_next();
                }
                selector
            }
            _ => {
                let Some(selector) = ActiveSelector::start_with_extra(
                    surface,
                    self.buffer.line(),
                    self.buffer.cursor(),
                    reverse,
                    &self.selector_extra_candidates,
                ) else {
                    self.selector = None;
                    return ReadlineOutcome::Noop;
                };
                self.selector = Some(selector);
                let Some(selector) = self.selector.as_mut() else {
                    return ReadlineOutcome::Noop;
                };
                selector
            }
        };
        let Some((line, cursor)) = selector.selected_line() else {
            self.selector = None;
            return ReadlineOutcome::Noop;
        };
        self.buffer.set_line_and_cursor(line, cursor);
        ReadlineOutcome::Edited
    }

    /// Runs the selector surface operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn selector_surface(&self) -> Option<SelectorSurface> {
        match self.kind {
            ReadlinePromptKind::Command => Some(SelectorSurface::MezzanineCommand),
            ReadlinePromptKind::Agent => Some(SelectorSurface::AgentCommand),
        }
    }

    /// Runs the prefix operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn prefix(&self) -> &'static str {
        match self.kind {
            ReadlinePromptKind::Command => ":",
            ReadlinePromptKind::Agent => "agent> ",
        }
    }
}

impl From<bool> for ReadlineOutcome {
    fn from(changed: bool) -> Self {
        if changed {
            ReadlineOutcome::Edited
        } else {
            ReadlineOutcome::Noop
        }
    }
}
