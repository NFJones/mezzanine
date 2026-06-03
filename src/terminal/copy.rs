//! Terminal Copy implementation.
//!
//! This module owns the terminal copy boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    MezError, PasteBuffers, Result, TerminalScreen, TerminalStyledLine, char_count, line_slice,
    normalize_selection, search_backward, search_forward, terminal_grapheme_width,
    terminal_graphemes, validate_copy_position,
};
use crate::readline::readline_word_column_range;

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

/// Carries Search Direction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    /// Represents the Forward case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Forward,
    /// Represents the Backward case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Backward,
}

/// Carries Copy Position state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CopyPosition {
    /// Stores the line value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub line: usize,
    /// Stores the column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub column: usize,
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
    pub(super) lines: Vec<String>,
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
    pub(super) viewport_rows: usize,
    /// Stores the scroll top value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scroll_top: usize,
    /// Stores the cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) cursor: CopyPosition,
    /// Stores the selection value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) selection: Option<(CopyPosition, CopyPosition)>,
    /// Stores the selection anchor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) selection_anchor: Option<CopyPosition>,
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
            lines,
            copy_lines,
            styled_lines,
            viewport_rows,
            scroll_top,
            cursor,
            selection: None,
            selection_anchor: None,
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
            lines,
            copy_lines,
            styled_lines,
            viewport_rows,
            scroll_top: 0,
            cursor: CopyPosition { line: 0, column: 0 },
            selection: None,
            selection_anchor: None,
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
        self.scroll_top
    }

    /// Returns the number of lines available in the copy-mode buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Returns the one-based end line visible at the bottom of the viewport.
    pub fn visible_end_line(&self) -> usize {
        self.scroll_top
            .saturating_add(self.viewport_rows)
            .min(self.lines.len())
    }

    /// Returns whether the viewport is at the live bottom of the buffer.
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_top >= self.lines.len().saturating_sub(self.viewport_rows)
    }

    /// Runs the visible lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn visible_lines(&self) -> &[String] {
        let end = self
            .scroll_top
            .saturating_add(self.viewport_rows)
            .min(self.lines.len());
        &self.lines[self.scroll_top..end]
    }

    /// Returns the styled lines visible in the copy-mode viewport.
    pub fn visible_styled_lines(&self) -> &[TerminalStyledLine] {
        let end = self
            .scroll_top
            .saturating_add(self.viewport_rows)
            .min(self.styled_lines.len());
        &self.styled_lines[self.scroll_top..end]
    }

    /// Updates the copy-mode viewport height after a pane or window resize.
    pub fn resize_viewport_rows(&mut self, viewport_rows: usize) -> Result<()> {
        if viewport_rows == 0 {
            return Err(MezError::invalid_args(
                "copy mode viewport must have at least one row",
            ));
        }
        self.viewport_rows = viewport_rows;
        self.scroll_top = self
            .scroll_top
            .min(self.lines.len().saturating_sub(self.viewport_rows));
        self.cursor = self.clamp_position(self.cursor);
        self.selection = self
            .selection
            .map(|(start, end)| (self.clamp_position(start), self.clamp_position(end)));
        self.selection_anchor = self
            .selection_anchor
            .map(|anchor| self.clamp_position(anchor));
        self.keep_cursor_visible();
        self.update_keyboard_selection();
        Ok(())
    }

    /// Runs the scroll by operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn scroll_by(&mut self, delta: isize) {
        let max_top = self.lines.len().saturating_sub(self.viewport_rows);
        if delta.is_negative() {
            self.scroll_top = self.scroll_top.saturating_sub(delta.unsigned_abs());
        } else {
            self.scroll_top = self.scroll_top.saturating_add(delta as usize).min(max_top);
        }
        self.cursor.line = self.cursor.line.clamp(
            self.scroll_top,
            self.scroll_top
                .saturating_add(self.viewport_rows.saturating_sub(1))
                .min(self.lines.len().saturating_sub(1)),
        );
        self.update_keyboard_selection();
    }

    /// Runs the page up operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn page_up(&mut self) {
        if self.scroll_top < self.viewport_rows {
            self.scroll_to_top();
        } else {
            self.scroll_by(-(self.viewport_rows as isize));
        }
    }

    /// Runs the page down operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn page_down(&mut self) {
        let max_top = self.lines.len().saturating_sub(self.viewport_rows);
        if max_top.saturating_sub(self.scroll_top) < self.viewport_rows {
            self.scroll_to_bottom();
        } else {
            self.scroll_by(self.viewport_rows as isize);
        }
    }

    /// Runs the scroll to top operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn scroll_to_top(&mut self) {
        self.scroll_top = 0;
        self.cursor.line = 0;
        self.cursor.column = 0;
        self.update_keyboard_selection();
    }

    /// Runs the scroll to bottom operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_top = self.lines.len().saturating_sub(self.viewport_rows);
        self.cursor.line = self.lines.len().saturating_sub(1);
        self.cursor.column = self
            .lines
            .get(self.cursor.line)
            .map(|line| char_count(line))
            .unwrap_or_default();
        self.update_keyboard_selection();
    }

    /// Runs the cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn cursor(&self) -> CopyPosition {
        self.cursor
    }

    /// Clamps one copy position to the available copy-mode buffer.
    ///
    /// # Parameters
    /// - `position`: Position to clamp.
    pub fn clamp_position(&self, position: CopyPosition) -> CopyPosition {
        let line = position.line.min(self.lines.len().saturating_sub(1));
        CopyPosition {
            line,
            column: position.column.min(self.line_width(line)),
        }
    }

    /// Runs the move cursor by operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn move_cursor_by(&mut self, line_delta: isize, column_delta: isize) {
        let max_line = self.lines.len().saturating_sub(1);
        self.cursor.line = if line_delta.is_negative() {
            self.cursor.line.saturating_sub(line_delta.unsigned_abs())
        } else {
            self.cursor
                .line
                .saturating_add(line_delta as usize)
                .min(max_line)
        };
        self.cursor.column = self.cursor.column.min(self.line_width(self.cursor.line));
        self.move_cursor_columns(column_delta);
        self.keep_cursor_visible();
        self.update_keyboard_selection();
    }

    /// Moves the cursor horizontally, overflowing left and right across line
    /// boundaries the way readline-style cursor movement does.
    fn move_cursor_columns(&mut self, column_delta: isize) {
        if column_delta.is_negative() {
            for _ in 0..column_delta.unsigned_abs() {
                self.move_cursor_left_one();
            }
        } else {
            for _ in 0..column_delta as usize {
                self.move_cursor_right_one();
            }
        }
    }

    /// Moves the cursor one cell left, crossing to the previous line when the
    /// cursor is already at the beginning of the current line.
    fn move_cursor_left_one(&mut self) {
        if self.cursor.column > 0 {
            if let Some(line) = self.lines.get(self.cursor.line) {
                if let Some((start, _)) =
                    grapheme_at_column(line, self.cursor.column.saturating_sub(1))
                {
                    self.cursor.column = start;
                } else {
                    self.cursor.column = self.cursor.column.saturating_sub(1);
                }
            } else {
                self.cursor.column = self.cursor.column.saturating_sub(1);
            }
        } else if self.cursor.line > 0 {
            self.cursor.line = self.cursor.line.saturating_sub(1);
            self.cursor.column = self.line_width(self.cursor.line);
        }
    }

    /// Moves the cursor one cell right, crossing to the next line when the
    /// cursor is already at the end of the current line.
    fn move_cursor_right_one(&mut self) {
        let line_width = self.line_width(self.cursor.line);
        if self.cursor.column < line_width {
            if let Some(line) = self.lines.get(self.cursor.line) {
                if let Some((start, width)) = grapheme_at_column(line, self.cursor.column) {
                    self.cursor.column = start.saturating_add(width);
                } else {
                    self.cursor.column = self.cursor.column.saturating_add(1);
                }
            } else {
                self.cursor.column = self.cursor.column.saturating_add(1);
            }
        } else if self.cursor.line.saturating_add(1) < self.lines.len() {
            self.cursor.line = self.cursor.line.saturating_add(1);
            self.cursor.column = 0;
        }
    }

    /// Returns the width of one copy-mode line in terminal-cell columns.
    fn line_width(&self, line: usize) -> usize {
        self.lines
            .get(line)
            .map(|line| char_count(line))
            .unwrap_or_default()
    }

    /// Scrolls the viewport just enough to keep the cursor visible.
    fn keep_cursor_visible(&mut self) {
        if self.cursor.line < self.scroll_top {
            self.scroll_top = self.cursor.line;
        } else {
            let bottom = self.scroll_top.saturating_add(self.viewport_rows);
            if self.cursor.line >= bottom {
                self.scroll_top = self
                    .cursor
                    .line
                    .saturating_sub(self.viewport_rows.saturating_sub(1));
            }
        }
    }

    /// Moves the copy-mode cursor to the beginning of the current line.
    pub fn move_cursor_to_line_start(&mut self) {
        self.cursor.column = 0;
        self.update_keyboard_selection();
    }

    /// Moves the copy-mode cursor to the end of the current line.
    pub fn move_cursor_to_line_end(&mut self) {
        self.cursor.column = self
            .lines
            .get(self.cursor.line)
            .map(|line| char_count(line))
            .unwrap_or_default();
        self.update_keyboard_selection();
    }

    /// Moves the copy-mode cursor to the previous word boundary on the current
    /// line, matching readline-style modified horizontal movement.
    pub fn move_cursor_word_left(&mut self) {
        self.cursor.column = self
            .lines
            .get(self.cursor.line)
            .map(|line| previous_word_column(line, self.cursor.column))
            .unwrap_or_default();
        self.update_keyboard_selection();
    }

    /// Moves the copy-mode cursor to the next word boundary on the current
    /// line, matching readline-style modified horizontal movement.
    pub fn move_cursor_word_right(&mut self) {
        self.cursor.column = self
            .lines
            .get(self.cursor.line)
            .map(|line| next_word_column(line, self.cursor.column))
            .unwrap_or_default();
        self.update_keyboard_selection();
    }

    /// Runs the begin keyboard selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn begin_keyboard_selection(&mut self) {
        self.selection_anchor = Some(self.cursor);
        self.selection = Some((self.cursor, self.cursor));
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
        if query.is_empty() {
            return Err(MezError::invalid_args(
                "copy mode search query must not be empty",
            ));
        }

        let found = match direction {
            SearchDirection::Forward => search_forward(&self.lines, self.scroll_top, query),
            SearchDirection::Backward => search_backward(&self.lines, self.scroll_top, query),
        };

        if let Some((position, width)) = found {
            self.scroll_top = position.line.min(self.lines.len().saturating_sub(1));
            self.selection = Some((
                position,
                CopyPosition {
                    line: position.line,
                    column: position.column.saturating_add(width),
                },
            ));
            Ok(Some(position))
        } else {
            Ok(None)
        }
    }

    /// Runs the select range operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_range(&mut self, start: CopyPosition, end: CopyPosition) -> Result<()> {
        validate_copy_position(&self.lines, start)?;
        validate_copy_position(&self.lines, end)?;
        self.selection = Some((start, end));
        Ok(())
    }

    /// Selects the readline-style word segment surrounding one copy position.
    ///
    /// # Parameters
    /// - `position`: Copy-mode position whose line and column identify the
    ///   clicked terminal cell.
    pub fn select_word_at(&mut self, position: CopyPosition) -> Result<()> {
        let position = self.clamp_position(position);
        let Some(line) = self.lines.get(position.line) else {
            return Err(MezError::invalid_args(
                "copy mode word selection line is invalid",
            ));
        };
        let (start, end) = readline_word_column_range(line, position.column);
        self.select_range(
            CopyPosition {
                line: position.line,
                column: start,
            },
            CopyPosition {
                line: position.line,
                column: end,
            },
        )
    }

    /// Runs the clear selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
    }

    /// Runs the selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn selection(&self) -> Option<(CopyPosition, CopyPosition)> {
        self.selection
    }

    /// Runs the copy selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn copy_selection(&self) -> Result<String> {
        let Some((start, end)) = self.selection else {
            return Ok(String::new());
        };
        let (start, end) = normalize_selection(start, end);
        validate_copy_position(&self.lines, start)?;
        validate_copy_position(&self.lines, end)?;

        if start.line == end.line {
            let lines = vec![self.copy_line_slice(start.line, start.column, end.column)];
            return Ok(normalize_copied_selection_lines(lines).join("\n"));
        }

        let mut copied = Vec::new();
        copied.push(self.copy_line_slice(
            start.line,
            start.column,
            char_count(&self.lines[start.line]),
        ));
        for line in (start.line + 1)..end.line {
            copied.push(self.copy_lines[line].clone());
        }
        copied.push(self.copy_line_slice(end.line, 0, end.column));
        Ok(normalize_copied_selection_lines(copied).join("\n"))
    }

    /// Slices a displayed line unless the selection covers a full transformed
    /// presentation line with a raw-copy override.
    fn copy_line_slice(&self, line: usize, start: usize, end: usize) -> String {
        let Some(display_line) = self.lines.get(line) else {
            return String::new();
        };
        let Some(copy_line) = self.copy_lines.get(line) else {
            return line_slice(display_line, start, end);
        };
        if copy_line == AGENT_COPY_SKIP_LINE {
            return AGENT_COPY_SKIP_LINE.to_string();
        }
        if decode_agent_copy_source_line(copy_line).is_some() {
            return copy_line.clone();
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
        buffers.set(name, self.copy_selection()?)
    }

    /// Runs the update keyboard selection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn update_keyboard_selection(&mut self) {
        if let Some(anchor) = self.selection_anchor {
            self.selection = Some((anchor, self.cursor));
        }
    }
}

/// Finds the grapheme cluster covering a display column and returns its
/// starting display column and total cell width.
fn grapheme_at_column(line: &str, target: usize) -> Option<(usize, usize)> {
    let mut col = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = terminal_grapheme_width(grapheme);
        let next = col.saturating_add(width);
        if target >= col && target < next {
            return Some((col, width));
        }
        col = next;
    }
    None
}

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

/// Converts a display-column position on a line into its corresponding
/// Unicode scalar index so `readline_word_column_range` can operate on it.
fn display_column_to_scalar_index(line: &str, target_column: usize) -> usize {
    let mut col = 0usize;
    let mut scalar = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = terminal_grapheme_width(grapheme);
        let next = col.saturating_add(width);
        if target_column < next {
            return scalar;
        }
        scalar += grapheme.chars().count();
        col = next;
    }
    scalar
}

/// Converts a Unicode scalar index on a line back to its corresponding
/// display column.
fn scalar_index_to_display_column(line: &str, target_scalar: usize) -> usize {
    let mut col = 0usize;
    let mut scalar = 0usize;
    for grapheme in terminal_graphemes(line) {
        if scalar >= target_scalar {
            break;
        }
        scalar += grapheme.chars().count();
        col = col.saturating_add(terminal_grapheme_width(grapheme));
    }
    col
}

/// Returns the current line column reached by moving backward by one
/// word-like segment.
fn previous_word_column(line: &str, column: usize) -> usize {
    let chars = line.chars().collect::<Vec<_>>();
    let scalar = display_column_to_scalar_index(line, column);
    let mut index = scalar.min(chars.len());
    while index > 0 && chars[index.saturating_sub(1)].is_whitespace() {
        index = index.saturating_sub(1);
    }
    let word_start = readline_word_column_range(line, index.saturating_sub(1)).0;
    scalar_index_to_display_column(line, word_start)
}

/// Returns the current line column reached by moving forward by one word-like
/// segment.
fn next_word_column(line: &str, column: usize) -> usize {
    let chars = line.chars().collect::<Vec<_>>();
    let scalar = display_column_to_scalar_index(line, column);
    let mut index = scalar.min(chars.len());
    while index < chars.len() && chars[index].is_whitespace() {
        index = index.saturating_add(1);
    }
    let word_end = readline_word_column_range(line, index).1;
    scalar_index_to_display_column(line, word_end)
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
