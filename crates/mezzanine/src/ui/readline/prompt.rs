//! Product prompt adapter for mux-owned readline editing state.
//!
//! The mux crate owns prompt-buffer transitions, reverse search, multiline
//! navigation, and baseline terminal input. Mezzanine retains command/agent
//! prefixes plus selector discovery and completion policy.

use crate::error::Result;
use crate::ui::selector::{
    AsyncFilesystemSelectorCandidates, AsyncFilesystemSelectorSnapshot, SelectorExtraCandidate,
    SelectorSurface, shadow_hint_with_extra_and_filesystem_candidates,
    start_active_selector_with_extra_and_filesystem_candidates,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::types::{ReadlinePrompt, ReadlinePromptKind};
use mez_mux::readline::{ReadlineOutcome, ReadlinePromptMode};
use mez_mux::selector::SelectorShadowHint;
use unicode_width::UnicodeWidthStr;

/// One immutable prompt presentation computed from a single shadow-hint result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadlinePromptRenderSnapshot {
    /// Complete prompt text including any transient shadow hint.
    pub(crate) text: String,
    /// Cursor column in terminal display cells.
    pub(crate) cursor_column: usize,
    /// Shadow hint start and width in terminal display cells.
    pub(crate) shadow_hint_columns: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadlinePromptRenderSnapshotKey {
    kind: ReadlinePromptKind,
    rendered_reverse_search: Option<String>,
    rendered_line: String,
    line: String,
    cursor: usize,
    selector_revision: u64,
    filesystem_completion_generation: Option<u64>,
}

#[derive(Debug, Default)]
struct ReadlinePromptRenderSnapshotCacheState {
    entry: Option<(
        ReadlinePromptRenderSnapshotKey,
        ReadlinePromptRenderSnapshot,
    )>,
    misses: u64,
}

/// Clone-safe operational cache for prompt render projections.
#[derive(Debug, Clone, Default)]
pub(super) struct ReadlinePromptRenderSnapshotCache {
    state: Arc<Mutex<ReadlinePromptRenderSnapshotCacheState>>,
}

impl ReadlinePromptRenderSnapshotCache {
    fn get(&self, key: &ReadlinePromptRenderSnapshotKey) -> Option<ReadlinePromptRenderSnapshot> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry
            .as_ref()
            .filter(|(cached_key, _)| cached_key == key)
            .map(|(_, snapshot)| snapshot.clone())
    }

    fn store(&self, key: ReadlinePromptRenderSnapshotKey, snapshot: ReadlinePromptRenderSnapshot) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.misses = state.misses.saturating_add(1);
        state.entry = Some((key, snapshot));
    }

    #[cfg(test)]
    fn misses(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .misses
    }
}

impl ReadlinePrompt {
    /// Creates an empty prompt using mux-owned editing state.
    pub fn new(kind: ReadlinePromptKind) -> Self {
        Self {
            kind,
            state: Default::default(),
            selector: None,
            selector_extra_candidates: Vec::new(),
            selector_working_directory: None,
            filesystem_selector_candidates: AsyncFilesystemSelectorCandidates::default(),
            selector_revision: 0,
            render_snapshot_cache: Default::default(),
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
        let candidates = candidates.into_iter().collect::<Vec<_>>();
        if self.selector_extra_candidates != candidates {
            self.selector_extra_candidates = candidates;
            self.selector_revision = self.selector_revision.wrapping_add(1);
        }
    }

    /// Replaces the prompt-local working directory used for completion.
    pub fn set_selector_working_directory(&mut self, working_directory: Option<PathBuf>) {
        if self.selector_working_directory != working_directory {
            self.selector_revision = self.selector_revision.wrapping_add(1);
        }
        self.selector_working_directory = working_directory;
    }

    /// Computes or reuses one complete prompt render snapshot.
    pub(crate) fn render_snapshot(&self) -> ReadlinePromptRenderSnapshot {
        let filesystem_snapshot = self.filesystem_selector_snapshot();
        let key = ReadlinePromptRenderSnapshotKey {
            kind: self.kind,
            rendered_reverse_search: self.state.rendered_reverse_search(),
            rendered_line: self.state.buffer.rendered_line(),
            line: self.state.buffer.line().to_string(),
            cursor: self.state.buffer.cursor(),
            selector_revision: self.selector_revision,
            filesystem_completion_generation: filesystem_snapshot.completion_generation(),
        };
        if let Some(snapshot) = self.render_snapshot_cache.get(&key) {
            return snapshot;
        }
        let snapshot = self.compute_render_snapshot(&key, &filesystem_snapshot);
        self.render_snapshot_cache.store(key, snapshot.clone());
        snapshot
    }

    /// Renders the prompt as plain text for a terminal row.
    pub fn render(&self) -> String {
        self.state
            .rendered_reverse_search()
            .unwrap_or_else(|| format!("{}{}", self.prefix(), self.state.buffer.rendered_line()))
    }

    /// Renders the prompt with transient selector shadow text.
    #[cfg(test)]
    pub fn render_with_shadow_hint(&self) -> String {
        self.render_snapshot().text
    }

    /// Returns the shadow-hint column and width in the rendered prompt.
    #[cfg(test)]
    pub fn rendered_shadow_hint_columns(&self) -> Option<(usize, usize)> {
        self.render_snapshot().shadow_hint_columns
    }

    /// Returns the cursor column in the rendered prompt line.
    pub fn rendered_cursor_column(&self) -> usize {
        self.render_snapshot().cursor_column
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

    /// Clears selector and reverse-search state after whole-buffer replacement.
    pub fn clear_transient_editing_state(&mut self) {
        self.selector = None;
        self.state.clear_reverse_search();
    }

    fn filesystem_selector_snapshot(&self) -> AsyncFilesystemSelectorSnapshot {
        let Some(surface) = self.selector_surface() else {
            return AsyncFilesystemSelectorSnapshot::default();
        };
        self.filesystem_selector_candidates.snapshot(
            surface,
            self.state.buffer.line(),
            self.state.buffer.cursor(),
            self.selector_working_directory.as_deref(),
        )
    }

    fn shadow_hint(
        &self,
        filesystem_snapshot: &AsyncFilesystemSelectorSnapshot,
    ) -> Option<SelectorShadowHint> {
        let surface = self.selector_surface()?;
        shadow_hint_with_extra_and_filesystem_candidates(
            surface,
            self.state.buffer.line(),
            self.state.buffer.cursor(),
            &self.selector_extra_candidates,
            filesystem_snapshot.candidates(),
        )
    }

    fn compute_render_snapshot(
        &self,
        key: &ReadlinePromptRenderSnapshotKey,
        filesystem_snapshot: &AsyncFilesystemSelectorSnapshot,
    ) -> ReadlinePromptRenderSnapshot {
        if let Some(search) = key.rendered_reverse_search.as_ref() {
            return ReadlinePromptRenderSnapshot {
                text: search.clone(),
                cursor_column: self.state.reverse_search_cursor_column().unwrap_or(0),
                shadow_hint_columns: None,
            };
        }
        let line = self.state.buffer.line();
        let hint = self
            .shadow_hint(filesystem_snapshot)
            .filter(|hint| hint.insert_at <= line.len() && line.is_char_boundary(hint.insert_at));
        let rendered_line = hint
            .as_ref()
            .and_then(|hint| {
                self.state
                    .buffer
                    .rendered_line_with_insert(hint.insert_at, &hint.text)
            })
            .unwrap_or_else(|| key.rendered_line.clone());
        let shadow_hint_columns = hint.as_ref().map(|hint| {
            let start = UnicodeWidthStr::width(self.prefix())
                .saturating_add(self.state.buffer.rendered_columns_before(hint.insert_at));
            (start, UnicodeWidthStr::width(hint.text.as_str()))
        });
        ReadlinePromptRenderSnapshot {
            text: format!("{}{}", self.prefix(), rendered_line),
            cursor_column: UnicodeWidthStr::width(self.prefix()).saturating_add(
                self.state
                    .buffer
                    .rendered_columns_before(self.state.buffer.cursor()),
            ),
            shadow_hint_columns,
        }
    }

    #[cfg(test)]
    pub(crate) fn render_snapshot_misses_for_tests(&self) -> u64 {
        self.render_snapshot_cache.misses()
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
                let filesystem_snapshot = self.filesystem_selector_candidates.snapshot(
                    surface,
                    self.state.buffer.line(),
                    self.state.buffer.cursor(),
                    self.selector_working_directory.as_deref(),
                );
                let Some(selector) = start_active_selector_with_extra_and_filesystem_candidates(
                    surface,
                    self.state.buffer.line(),
                    self.state.buffer.cursor(),
                    reverse,
                    &self.selector_extra_candidates,
                    filesystem_snapshot.candidates(),
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
        self.state
            .buffer
            .set_line_and_cursor_preserving_paste_blocks(line, cursor);
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
