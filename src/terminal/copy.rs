//! Terminal Copy implementation.
//!
//! This module owns the terminal copy boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{MezError, Result, TerminalScreen, TerminalStyledLine, char_count, line_slice};
use mez_mux::copy::{
    CopyBuffer, CopyModeActionOutcome, CopyModeKeyAction, CopyPosition, SearchDirection,
    normalize_selection, validate_position,
};
use mez_mux::paste::PasteBuffers;

// Copy mode, selection, and search primitives.

/// Display prefix used by pane-local agent transcript lines.
const AGENT_COPY_INDICATOR_PREFIX: &str = "▐ ";
/// Speaker label used by assistant response lines.
const AGENT_COPY_ASSISTANT_LABEL: &str = "mez> ";
/// Copy-text marker for presentation-only continuation rows.
pub(crate) const AGENT_COPY_SKIP_LINE: &str = "\u{1e}mez-copy-skip-line";
/// Copy-text marker carrying one markdown source-line identity and raw text.
pub(crate) const AGENT_COPY_SOURCE_LINE_PREFIX: &str = "\u{1e}mez-copy-source-line:";
/// Copy-text marker for wrapped markdown continuation rows.
pub(crate) const AGENT_COPY_WRAP_CONTINUATION: &str = "\u{1e}mez-copy-wrap-continuation";

/// Encodes one markdown source-line identity with its raw copy text.
pub(crate) fn encode_agent_copy_source_line(source_index: usize, copy_line: &str) -> String {
    format!("{AGENT_COPY_SOURCE_LINE_PREFIX}{source_index}:{copy_line}")
}

/// Carries Copy Mode state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyMode {
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    buffer: CopyBuffer,
    /// Raw-copy lines used when presentation-only text differs from display.
    ///
    /// This remains parallel to `lines`; selections are navigated on displayed
    /// text, but full-line copied output can preserve source markup.
    pub(super) copy_lines: Vec<String>,
    /// Stores the styled lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) styled_lines: Vec<TerminalStyledLine>,
    /// Stores the viewport rows value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the scroll top value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the selection value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the selection anchor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the alternate screen was active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) alternate_screen_was_active: bool,
}

impl CopyMode {
    /// Runs the from screen operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_screen(screen: &TerminalScreen, viewport_rows: usize) -> Result<Self> {
        if viewport_rows == 0 {
            return Err(MezError::invalid_args(
                "copy mode viewport must have at least one row",
            ));
        }
        let styled_lines = screen.normal_styled_content_lines();
        let lines = styled_lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let copy_lines = styled_lines
            .iter()
            .map(|line| line.copy_text.clone().unwrap_or_else(|| line.text.clone()))
            .collect::<Vec<_>>();
        let (scroll_top, cursor) = initial_copy_mode_view(screen, &lines, viewport_rows);

        Ok(Self {
            buffer: CopyBuffer::new(lines, viewport_rows, scroll_top, cursor)?,
            copy_lines,
            styled_lines,
            alternate_screen_was_active: screen.alternate_screen_active(),
        })
    }

    /// Builds a copy-mode buffer from the currently visible terminal rows.
    ///
    /// This constructor is intentionally narrower than `from_screen`: it does
    /// not include normal scrollback history. Mouse drag selection uses it for
    /// alternate-screen applications so an explicit user selection can copy the
    /// visible full-screen content without adding that content to normal
    /// history or agent context.
    pub fn from_visible_screen(screen: &TerminalScreen, viewport_rows: usize) -> Result<Self> {
        if viewport_rows == 0 {
            return Err(MezError::invalid_args(
                "copy mode viewport must have at least one row",
            ));
        }
        let styled_lines = screen.visible_styled_lines();
        let lines = styled_lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let copy_lines = styled_lines
            .iter()
            .map(|line| line.copy_text.clone().unwrap_or_else(|| line.text.clone()))
            .collect::<Vec<_>>();

        Ok(Self {
            buffer: CopyBuffer::new(lines, viewport_rows, 0, CopyPosition { line: 0, column: 0 })?,
            copy_lines,
            styled_lines,
            alternate_screen_was_active: screen.alternate_screen_active(),
        })
    }

    /// Runs the alternate screen was active operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn alternate_screen_was_active(&self) -> bool {
        self.alternate_screen_was_active
    }

    /// Runs the scroll top operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn scroll_top(&self) -> usize {
        self.buffer.scroll_top()
    }

    /// Returns the number of lines available in the copy-mode buffer.
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// Returns the one-based end line visible at the bottom of the viewport.
    pub fn visible_end_line(&self) -> usize {
        self.buffer.visible_end_line()
    }

    /// Returns whether the viewport is at the live bottom of the buffer.
    pub fn is_at_bottom(&self) -> bool {
        self.buffer.is_at_bottom()
    }

    /// Runs the visible lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn visible_lines(&self) -> &[String] {
        self.buffer.visible_lines()
    }

    /// Returns the styled lines visible in the copy-mode viewport.
    pub fn visible_styled_lines(&self) -> &[TerminalStyledLine] {
        &self.styled_lines[self.scroll_top()..self.visible_end_line()]
    }

    /// Applies one mux-owned keyboard transition to the copy buffer.
    pub fn apply_key_action(&mut self, action: CopyModeKeyAction) -> CopyModeActionOutcome {
        self.buffer.apply_key_action(action)
    }

    /// Updates the copy-mode viewport height after a pane or window resize.
    pub fn resize_viewport_rows(&mut self, viewport_rows: usize) -> Result<()> {
        self.buffer
            .resize_viewport_rows(viewport_rows)
            .map_err(Into::into)
    }

    /// Runs the scroll by operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn scroll_by(&mut self, delta: isize) {
        self.buffer.scroll_by(delta);
    }

    /// Runs the page up operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn page_up(&mut self) {
        self.buffer.page_up();
    }

    /// Runs the page down operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn page_down(&mut self) {
        self.buffer.page_down();
    }

    /// Runs the scroll to top operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn scroll_to_top(&mut self) {
        self.buffer.scroll_to_top();
    }

    /// Runs the scroll to bottom operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn scroll_to_bottom(&mut self) {
        self.buffer.scroll_to_bottom();
    }

    /// Runs the cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn cursor(&self) -> CopyPosition {
        self.buffer.cursor()
    }

    /// Clamps one copy position to the available copy-mode buffer.
    ///
    /// # Parameters
    /// - `position`: Position to clamp.
    pub fn clamp_position(&self, position: CopyPosition) -> CopyPosition {
        self.buffer.clamp_position(position)
    }

    /// Runs the move cursor by operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn move_cursor_by(&mut self, line_delta: isize, column_delta: isize) {
        self.buffer.move_cursor_by(line_delta, column_delta);
    }

    /// Moves the copy-mode cursor to the beginning of the current line.
    pub fn move_cursor_to_line_start(&mut self) {
        self.buffer.move_cursor_to_line_start();
    }

    /// Moves the copy-mode cursor to the end of the current line.
    pub fn move_cursor_to_line_end(&mut self) {
        self.buffer.move_cursor_to_line_end();
    }

    /// Moves the copy-mode cursor to the previous word boundary on the current
    /// line, matching readline-style modified horizontal movement.
    pub fn move_cursor_word_left(&mut self) {
        self.buffer.move_cursor_word_left();
    }

    /// Moves the copy-mode cursor to the next word boundary on the current
    /// line, matching readline-style modified horizontal movement.
    pub fn move_cursor_word_right(&mut self) {
        self.buffer.move_cursor_word_right();
    }

    /// Runs the begin keyboard selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn begin_keyboard_selection(&mut self) {
        self.buffer.begin_keyboard_selection();
    }

    /// Runs the search operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn search(
        &mut self,
        query: &str,
        direction: SearchDirection,
    ) -> Result<Option<CopyPosition>> {
        self.buffer.search(query, direction).map_err(Into::into)
    }

    /// Runs the select range operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_range(&mut self, start: CopyPosition, end: CopyPosition) -> Result<()> {
        self.buffer.select_range(start, end).map_err(Into::into)
    }

    /// Selects the readline-style word segment surrounding one copy position.
    ///
    /// # Parameters
    /// - `position`: Copy-mode position whose line and column identify the
    ///   clicked terminal cell.
    pub fn select_word_at(&mut self, position: CopyPosition) -> Result<()> {
        self.buffer.select_word_at(position).map_err(Into::into)
    }

    /// Runs the clear selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_selection(&mut self) {
        self.buffer.clear_selection();
    }

    /// Runs the selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn selection(&self) -> Option<(CopyPosition, CopyPosition)> {
        self.buffer.selection()
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
        validate_position(self.buffer.lines(), start)?;
        validate_position(self.buffer.lines(), end)?;

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
                char_count(&self.buffer.lines()[line])
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
        let copy_line = self.copy_lines.get(line)?;
        let (_, raw_line) = decode_agent_copy_source_line(copy_line)?;
        let (group_start, group_end) = self.markdown_source_group_bounds(line, copy_line);
        if line != group_start
            || !selection_fully_covers_markdown_source_group(
                self.buffer.lines(),
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
                .copy_lines
                .get(start.saturating_sub(1))
                .is_some_and(|candidate| candidate == copy_line)
        {
            start = start.saturating_sub(1);
        }
        let mut end = line;
        while end.saturating_add(1) < self.copy_lines.len()
            && self
                .copy_lines
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
        let Some(display_line) = self.buffer.lines().get(line) else {
            return String::new();
        };
        let Some(copy_line) = self.copy_lines.get(line) else {
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

/// Finds the grapheme cluster covering a display column and returns its
/// starting display column and total cell width.
/// Computes the initial copy-mode viewport and keyboard cursor from a pane
/// screen. Copy mode opens over the live terminal view, so the keyboard cursor
/// should begin at the same cell as the pane cursor rather than the first
/// visible history cell.
fn initial_copy_mode_view(
    screen: &TerminalScreen,
    lines: &[String],
    viewport_rows: usize,
) -> (usize, CopyPosition) {
    let mut scroll_top = lines.len().saturating_sub(viewport_rows);
    if lines.is_empty() || screen.alternate_screen_active() {
        return (
            scroll_top,
            CopyPosition {
                line: scroll_top,
                column: 0,
            },
        );
    }

    let live_rows = usize::from(screen.size().rows).max(1);
    let live_base_line = lines.len().saturating_sub(live_rows);
    let terminal_cursor = screen.cursor_state();
    let cursor_line = live_base_line
        .saturating_add(terminal_cursor.row.min(live_rows.saturating_sub(1)))
        .min(lines.len().saturating_sub(1));
    let cursor_column = lines
        .get(cursor_line)
        .map(|line| terminal_cursor.column.min(char_count(line)))
        .unwrap_or_default();

    let viewport_bottom = scroll_top.saturating_add(viewport_rows);
    if cursor_line < scroll_top {
        scroll_top = cursor_line;
    } else if cursor_line >= viewport_bottom {
        scroll_top = cursor_line.saturating_sub(viewport_rows.saturating_sub(1));
    }

    (
        scroll_top,
        CopyPosition {
            line: cursor_line,
            column: cursor_column,
        },
    )
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
