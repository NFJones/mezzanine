//! Terminal Copy implementation.
//!
//! This module owns the terminal copy boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};

use super::{Result, TerminalScreen};
use mez_mux::copy::{
    COPY_SKIP_LINE as AGENT_COPY_SKIP_LINE,
    COPY_SOURCE_LINE_PREFIX as AGENT_COPY_SOURCE_LINE_PREFIX, CopyPosition, StyledCopyMode,
    normalize_selection, validate_position,
};
#[cfg(test)]
use mez_mux::paste::PasteBuffers;
use mez_mux::render::{char_count, line_slice};

// Copy mode, selection, and search primitives.

/// Display prefix used by pane-local agent transcript lines.
const AGENT_COPY_INDICATOR_PREFIX: &str = "▐ ";
/// Speaker label used by assistant response lines.
const AGENT_COPY_ASSISTANT_LABEL: &str = "mez> ";
/// Product adapter over mux-owned styled copy-mode state.
///
/// Navigation, viewport, search, selection, terminal-derived styled rows, and
/// source-aware copy metadata belong to `mez-mux`. This adapter retains only
/// Mezzanine transcript/Markdown normalization and host clipboard integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyMode(StyledCopyMode);

/// Selects the representation written for an active copy-mode selection.
///
/// Rendered selection preserves the visible terminal cells while removing
/// Mezzanine-owned transcript decoration. Source selection emits every
/// intersected source-backed group once in display order and omits rows that
/// have no source association.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopySelectionFormat {
    /// Copy visible terminal text without presentation-to-source recovery.
    Rendered,
    /// Copy complete raw source groups that intersect the selection.
    Source,
}

impl Deref for CopyMode {
    type Target = StyledCopyMode;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for CopyMode {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl CopyMode {
    /// Builds copy state from normal-screen history and live terminal rows.
    pub fn from_screen(screen: &TerminalScreen, viewport_rows: usize) -> Result<Self> {
        Ok(Self(StyledCopyMode::from_screen(screen, viewport_rows)?))
    }

    /// Builds copy state from only the currently visible terminal rows.
    pub fn from_visible_screen(screen: &TerminalScreen, viewport_rows: usize) -> Result<Self> {
        Ok(Self(StyledCopyMode::from_visible_screen(
            screen,
            viewport_rows,
        )?))
    }

    /// Copies the active selection in the default rendered representation.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn copy_selection(&self) -> Result<String> {
        self.copy_selection_with_format(CopySelectionFormat::Rendered)
    }

    /// Copies the active selection in the requested representation.
    ///
    /// Source selection uses only explicit rich-text source associations. Rows
    /// such as terminal output, transcript gutters, and presentation-only
    /// decoration have no source association and are deliberately omitted.
    pub fn copy_selection_with_format(&self, format: CopySelectionFormat) -> Result<String> {
        let Some((start, end)) = self.selection() else {
            return Ok(String::new());
        };
        let (start, end) = normalize_selection(start, end);
        validate_position(self.0.lines(), start)?;
        validate_position(self.0.lines(), end)?;

        match format {
            CopySelectionFormat::Rendered => self.copy_rendered_selection(start, end),
            CopySelectionFormat::Source => self.copy_source_selection(start, end),
        }
    }

    /// Copies terminal display text while preserving the user's selected cells.
    fn copy_rendered_selection(&self, start: CopyPosition, end: CopyPosition) -> Result<String> {
        let mut copied = Vec::new();
        for line in start.line..=end.line {
            let line_start = if line == start.line { start.column } else { 0 };
            let line_end = if line == end.line {
                end.column
            } else {
                self.0
                    .lines()
                    .get(line)
                    .map(|display_line| char_count(display_line))
                    .unwrap_or_default()
            };
            copied.push(self.copy_rendered_line_slice(line, line_start, line_end));
        }
        Ok(normalize_copied_selection_lines(copied).join("\n"))
    }

    /// Copies each selected raw source group once, in display order.
    fn copy_source_selection(&self, start: CopyPosition, end: CopyPosition) -> Result<String> {
        let mut copied = Vec::new();
        let mut emitted_group_starts = BTreeSet::new();
        for line in start.line..=end.line {
            let Some(copy_line) = self.0.copy_lines().get(line) else {
                continue;
            };
            if let Some((group_start, group_end, raw_line)) =
                self.source_group_for_line(line, copy_line)
                && group_start <= end.line
                && group_end >= start.line
                && emitted_group_starts.insert(group_start)
            {
                copied.push(raw_line);
            }
        }
        Ok(copied.join("\n"))
    }

    /// Returns source-group metadata for one rendered row when available.
    fn source_group_for_line(
        &self,
        line: usize,
        copy_line: &str,
    ) -> Option<(usize, usize, String)> {
        if let Some((_, raw_line)) = decode_agent_copy_source_line(copy_line) {
            let (group_start, group_end) = self.markdown_source_group_bounds(line, copy_line);
            return Some((group_start, group_end, raw_line.to_string()));
        }
        let (group_start, group_end) = self.transformed_source_group_bounds_for_line(line)?;
        let raw_line = self.0.copy_lines().get(group_start)?;
        Some((group_start, group_end, raw_line.clone()))
    }

    /// Returns transformed source-group bounds for any row in that group.
    fn transformed_source_group_bounds_for_line(&self, line: usize) -> Option<(usize, usize)> {
        let copy_lines = self.0.copy_lines();
        let mut group_start = line;
        while group_start > 0
            && copy_lines
                .get(group_start)
                .is_some_and(|candidate| candidate == AGENT_COPY_SKIP_LINE)
        {
            group_start = group_start.saturating_sub(1);
        }
        let copy_line = copy_lines.get(group_start)?;
        let (candidate_start, group_end) =
            self.transformed_source_group_bounds(group_start, copy_line)?;
        (line >= candidate_start && line <= group_end).then_some((candidate_start, group_end))
    }

    /// Slices one displayed row without applying source-recovery metadata.
    fn copy_rendered_line_slice(&self, line: usize, start: usize, end: usize) -> String {
        self.0
            .lines()
            .get(line)
            .map(|display_line| line_slice(display_line, start, end))
            .unwrap_or_default()
    }

    /// Returns the rows owned by a transformed source block whose first row
    /// retains raw source and whose later rows are presentation-only.
    fn transformed_source_group_bounds(
        &self,
        line: usize,
        copy_line: &str,
    ) -> Option<(usize, usize)> {
        let copy_lines = self.0.copy_lines();
        if copy_line == AGENT_COPY_SKIP_LINE
            || decode_agent_copy_source_line(copy_line).is_some()
            || copy_lines.get(line.saturating_add(1))? != AGENT_COPY_SKIP_LINE
        {
            return None;
        }
        let mut group_end = line.saturating_add(1);
        while copy_lines
            .get(group_end.saturating_add(1))
            .is_some_and(|candidate| candidate == AGENT_COPY_SKIP_LINE)
        {
            group_end = group_end.saturating_add(1);
        }
        Some((line, group_end))
    }

    /// Returns the rendered row bounds belonging to one markdown source line.
    fn markdown_source_group_bounds(&self, line: usize, copy_line: &str) -> (usize, usize) {
        let mut start = line;
        while start > 0
            && self
                .0
                .copy_lines()
                .get(start.saturating_sub(1))
                .is_some_and(|candidate| candidate == copy_line)
        {
            start = start.saturating_sub(1);
        }
        let mut end = line;
        while end.saturating_add(1) < self.0.copy_lines().len()
            && self
                .0
                .copy_lines()
                .get(end.saturating_add(1))
                .is_some_and(|candidate| candidate == copy_line)
        {
            end = end.saturating_add(1);
        }
        (start, end)
    }

    /// Runs the copy selection to buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn copy_selection_to_buffer(
        &self,
        buffers: &mut PasteBuffers,
        name: impl Into<String>,
    ) -> Result<()> {
        Ok(buffers.set(name, self.copy_selection()?)?)
    }
}

/// Decodes one markdown source-line copy marker into its source identity and
/// raw line text.
fn decode_agent_copy_source_line(line: &str) -> Option<(usize, &str)> {
    let encoded = line.strip_prefix(AGENT_COPY_SOURCE_LINE_PREFIX)?;
    let (source_index, raw_line) = encoded.split_once(':')?;
    Some((source_index.parse().ok()?, raw_line))
}

/// Formats copied selection lines by removing display-only agent gutters.
fn normalize_copied_selection_lines(lines: Vec<String>) -> Vec<String> {
    let mut output = Vec::with_capacity(lines.len());
    let mut agent_run = Vec::new();
    let mut emitted_markdown_source_lines = Vec::new();
    for line in lines {
        if line == AGENT_COPY_SKIP_LINE {
            continue;
        }
        let line = if let Some((source_index, raw_line)) = decode_agent_copy_source_line(&line) {
            if emitted_markdown_source_lines.contains(&source_index) {
                continue;
            }
            emitted_markdown_source_lines.push(source_index);
            raw_line.to_string()
        } else {
            if line
                .strip_prefix(AGENT_COPY_INDICATOR_PREFIX)
                .unwrap_or(line.as_str())
                == "***"
            {
                emitted_markdown_source_lines.clear();
            }
            line
        };
        if let Some(stripped) = line.strip_prefix(AGENT_COPY_INDICATOR_PREFIX) {
            agent_run.push(stripped.to_string());
            continue;
        }
        flush_agent_copy_run(&mut output, &mut agent_run);
        output.push(line);
    }
    flush_agent_copy_run(&mut output, &mut agent_run);
    output
}

/// Moves one pending agent-copy run into the output after normalization.
fn flush_agent_copy_run(output: &mut Vec<String>, agent_run: &mut Vec<String>) {
    if agent_run.is_empty() {
        return;
    }
    let mut run = std::mem::take(agent_run);
    normalize_agent_copy_run(&mut run);
    output.extend(run);
}

/// Removes assistant labels and visual continuation padding from an agent run.
fn normalize_agent_copy_run(lines: &mut [String]) {
    let assistant_indent =
        AGENT_COPY_ASSISTANT_LABEL.chars().count() + AGENT_COPY_INDICATOR_PREFIX.chars().count();
    let mut saw_assistant_label = false;
    let mut segment_start = None;
    for index in 0..lines.len() {
        let stripped = lines[index]
            .strip_prefix(AGENT_COPY_ASSISTANT_LABEL)
            .map(str::to_string);
        if let Some(rest) = stripped {
            if let Some(start) = segment_start.take() {
                dedent_agent_copy_segment(&mut lines[start..index]);
            }
            lines[index] = rest;
            saw_assistant_label = true;
            segment_start = Some(index.saturating_add(1));
        }
    }
    if let Some(start) = segment_start {
        dedent_agent_copy_segment(&mut lines[start..]);
    }
    if !saw_assistant_label {
        dedent_orphan_agent_continuation_lines(lines, assistant_indent);
    }
}

/// Dedents one assistant-copy continuation segment by its common prefix.
fn dedent_agent_copy_segment(lines: &mut [String]) {
    let common_indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| leading_space_count(line))
        .min()
        .unwrap_or(0);
    if common_indent == 0 {
        return;
    }
    for line in lines {
        if line_has_leading_spaces(line, common_indent) {
            *line = strip_leading_chars(line, common_indent);
        }
    }
}

/// Dedents selected continuation-only agent output without touching content.
fn dedent_orphan_agent_continuation_lines(lines: &mut [String], max_indent: usize) {
    let common_indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| leading_space_count(line))
        .min()
        .unwrap_or(0)
        .min(max_indent);
    if common_indent == 0 {
        return;
    }
    for line in lines {
        if line_has_leading_spaces(line, common_indent) {
            *line = strip_leading_chars(line, common_indent);
        }
    }
}

/// Returns whether a line starts with at least the requested number of spaces.
fn line_has_leading_spaces(line: &str, count: usize) -> bool {
    line.chars().take(count).filter(|ch| *ch == ' ').count() == count
}

/// Counts leading display spaces in a copied line.
fn leading_space_count(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

/// Drops the requested number of leading characters from a copied line.
fn strip_leading_chars(line: &str, count: usize) -> String {
    line.chars().skip(count).collect()
}
