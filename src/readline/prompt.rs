//! Product prompt adapter for mux-owned readline editing state.
//!
//! The mux crate owns prompt-buffer transitions, reverse search, multiline
//! navigation, and baseline terminal input. Mezzanine retains command/agent
//! prefixes plus selector discovery and completion policy.

use crate::error::Result;
use crate::selector::{
    ActiveSelector, SelectorExtraCandidate, SelectorShadowHint, SelectorSurface,
    shadow_hint_with_extra_in_working_directory,
};
use std::path::PathBuf;

use super::types::{ReadlineOutcome, ReadlinePrompt, ReadlinePromptKind, ReadlinePromptMode};
use unicode_width::UnicodeWidthStr;

impl ReadlinePrompt {
    /// Creates an empty prompt using mux-owned editing state.
    pub fn new(kind: ReadlinePromptKind) -> Self {
        Self {
            kind,
            state: Default::default(),
            selector: None,
            selector_extra_candidates: Vec::new(),
            selector_working_directory: None,
        }
    }

    /// Records display cells available for the editable prompt body.
    pub fn set_prompt_body_columns(&mut self, columns: usize) {
        self.state.set_prompt_body_columns(columns);
    }

    /// Replaces runtime-provided selector candidates for this prompt.
    pub fn set_selector_extra_candidates(
        &mut self,
        candidates: impl IntoIterator<Item = SelectorExtraCandidate>,
    ) {
        self.selector_extra_candidates = candidates.into_iter().collect();
    }

    /// Replaces the prompt-local working directory used for completion.
    pub fn set_selector_working_directory(&mut self, working_directory: Option<PathBuf>) {
        self.selector_working_directory = working_directory;
    }

    /// Renders the prompt as plain text for a terminal row.
    pub fn render(&self) -> String {
        self.state
            .rendered_reverse_search()
            .unwrap_or_else(|| format!("{}{}", self.prefix(), self.state.buffer.rendered_line()))
    }

    /// Renders the prompt with transient selector shadow text.
    pub fn render_with_shadow_hint(&self) -> String {
        if let Some(search) = self.state.rendered_reverse_search() {
            return search;
        }
        format!("{}{}", self.prefix(), self.buffer_line_with_shadow_hint())
    }

    /// Returns the shadow-hint column and width in the rendered prompt.
    pub fn rendered_shadow_hint_columns(&self) -> Option<(usize, usize)> {
        if self.state.reverse_search_active() {
            return None;
        }
        let hint = self.shadow_hint()?;
        let line = self.state.buffer.line();
        let insert_at = hint.insert_at.min(line.len());
        if !line.is_char_boundary(insert_at) {
            return None;
        }
        let start = UnicodeWidthStr::width(self.prefix())
            .saturating_add(self.state.buffer.rendered_columns_before(insert_at));
        Some((start, UnicodeWidthStr::width(hint.text.as_str())))
    }

    /// Returns the cursor column in the rendered prompt line.
    pub fn rendered_cursor_column(&self) -> usize {
        if let Some(column) = self.state.reverse_search_cursor_column() {
            return column;
        }
        UnicodeWidthStr::width(self.prefix()).saturating_add(
            self.state
                .buffer
                .rendered_columns_before(self.state.buffer.cursor()),
        )
    }

    /// Applies raw terminal input with product selector policy around the
    /// mux-owned prompt transition engine.
    pub fn apply_terminal_input(&mut self, input: &[u8]) -> Result<ReadlineOutcome> {
        if let Some(outcome) = self.state.apply_reverse_search_input(input)? {
            self.selector = None;
            return Ok(outcome);
        }
        if input == b"\t" {
            return Ok(self.apply_selector_input(false));
        }
        if input == b"\x1b[Z" {
            return Ok(self.apply_selector_input(true));
        }
        self.selector = None;
        let mode = match self.kind {
            ReadlinePromptKind::Command => ReadlinePromptMode::SingleLine,
            ReadlinePromptKind::Agent => ReadlinePromptMode::Multiline,
        };
        if let Some(outcome) = self.state.apply_mode_input(mode, input) {
            return Ok(outcome);
        }
        Ok(self.state.apply_terminal_input(input)?)
    }

    /// Reports whether incremental reverse search is active.
    pub fn reverse_search_active(&self) -> bool {
        self.state.reverse_search_active()
    }

    fn shadow_hint(&self) -> Option<SelectorShadowHint> {
        let surface = self.selector_surface()?;
        shadow_hint_with_extra_in_working_directory(
            surface,
            self.state.buffer.line(),
            self.state.buffer.cursor(),
            &self.selector_extra_candidates,
            self.selector_working_directory.as_deref(),
        )
    }

    fn buffer_line_with_shadow_hint(&self) -> String {
        let line = self.state.buffer.line();
        let Some(hint) = self.shadow_hint() else {
            return self.state.buffer.rendered_line();
        };
        if hint.insert_at > line.len() || !line.is_char_boundary(hint.insert_at) {
            return self.state.buffer.rendered_line();
        }
        self.state
            .buffer
            .rendered_line_with_insert(hint.insert_at, &hint.text)
            .unwrap_or_else(|| self.state.buffer.rendered_line())
    }

    fn apply_selector_input(&mut self, reverse: bool) -> ReadlineOutcome {
        let Some(surface) = self.selector_surface() else {
            return ReadlineOutcome::Noop;
        };
        if self.selector.as_ref().is_some_and(|selector| {
            selector.should_refresh_from_selected_directory(
                self.state.buffer.line(),
                self.state.buffer.cursor(),
            )
        }) {
            self.selector = None;
        }
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
                let Some(selector) = ActiveSelector::start_with_extra_in_working_directory(
                    surface,
                    self.state.buffer.line(),
                    self.state.buffer.cursor(),
                    reverse,
                    &self.selector_extra_candidates,
                    self.selector_working_directory.as_deref(),
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
        self.state.buffer.set_line_and_cursor(line, cursor);
        ReadlineOutcome::Edited
    }

    fn selector_surface(&self) -> Option<SelectorSurface> {
        match self.kind {
            ReadlinePromptKind::Command => Some(SelectorSurface::MezzanineCommand),
            ReadlinePromptKind::Agent => Some(SelectorSurface::AgentCommand),
        }
    }

    fn prefix(&self) -> &'static str {
        match self.kind {
            ReadlinePromptKind::Command => ":",
            ReadlinePromptKind::Agent => "mez> ",
        }
    }
}
