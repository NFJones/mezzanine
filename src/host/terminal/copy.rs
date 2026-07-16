//! Terminal Copy implementation.
//!
//! This module owns the terminal copy boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

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

    /// Runs the copy selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn copy_selection(&self) -> Result<String> {
        let Some((start, end)) = self.selection() else {
            return Ok(String::new());
        };
        let (start, end) = normalize_selection(start, end);
        validate_position(self.0.lines(), start)?;
        validate_position(self.0.lines(), end)?;

        let mut copied = Vec::new();
        let mut line = start.line;
        while line <= end.line {
            if let Some((group_end, raw_line)) =
                self.copy_selected_markdown_source_line(start, end, line)
            {
                copied.push(raw_line);
                line = group_end.saturating_add(1);
                continue;
            }
            let line_start = if line == start.line { start.column } else { 0 };
            let line_end = if line == end.line {
                end.column
            } else {
                char_count(&self.0.lines()[line])
            };
            copied.push(self.copy_line_slice(line, line_start, line_end));
            line = line.saturating_add(1);
        }
        Ok(normalize_copied_selection_lines(copied).join("\n"))
    }

    /// Returns the raw markdown source line when the selection fully covers one
    /// rendered source-line group.
    fn copy_selected_markdown_source_line(
        &self,
        selection_start: CopyPosition,
        selection_end: CopyPosition,
        line: usize,
    ) -> Option<(usize, String)> {
        let copy_line = self.0.copy_lines().get(line)?;
        let (_, raw_line) = decode_agent_copy_source_line(copy_line)?;
        let (group_start, group_end) = self.markdown_source_group_bounds(line, copy_line);
        if line != group_start
            || !selection_fully_covers_markdown_source_group(
                self.0.lines(),
                selection_start,
                selection_end,
                group_start,
                group_end,
            )
        {
            return None;
        }
        Some((group_end, raw_line.to_string()))
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

    /// Slices a displayed line unless the selection covers a full transformed
    /// presentation line with a non-markdown raw-copy override.
    fn copy_line_slice(&self, line: usize, start: usize, end: usize) -> String {
        let Some(display_line) = self.0.lines().get(line) else {
            return String::new();
        };
        let Some(copy_line) = self.0.copy_lines().get(line) else {
            return line_slice(display_line, start, end);
        };
        if copy_line == AGENT_COPY_SKIP_LINE {
            return AGENT_COPY_SKIP_LINE.to_string();
        }
        if decode_agent_copy_source_line(copy_line).is_some() {
            return line_slice(display_line, start, end);
        }
        let display_end = char_count(display_line);
        if copy_line != display_line {
            if start == 0 && end >= display_end {
                return copy_line.clone();
            }
            if let Some(raw_line) = copy_line.strip_prefix(AGENT_COPY_INDICATOR_PREFIX) {
                return format!(
                    "{AGENT_COPY_INDICATOR_PREFIX}{}",
                    line_slice(raw_line, start, end)
                );
            }
        }
        line_slice(display_line, start, end)
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

/// Returns whether a selection fully covers every rendered row of one markdown
/// source line.
fn selection_fully_covers_markdown_source_group(
    lines: &[String],
    selection_start: CopyPosition,
    selection_end: CopyPosition,
    group_start: usize,
    group_end: usize,
) -> bool {
    if selection_start.line > group_start || selection_end.line < group_end {
        return false;
    }
    if selection_start.line == group_start && selection_start.column > 0 {
        return false;
    }
    if selection_end.line == group_end {
        let group_end_width = lines
            .get(group_end)
            .map(|line| char_count(line))
            .unwrap_or_default();
        if selection_end.column < group_end_width {
            return false;
        }
    }
    true
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
