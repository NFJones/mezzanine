//! Dependency-neutral copy-mode contracts for multiplexer presentation.
//!
//! This module owns coordinates and state contracts shared by copy-mode input,
//! rendering, and runtime adapters. Product-specific copy-text normalization
//! remains in the Mezzanine composition crate.

use crate::input::{KeyCode, parse_key_chord_bytes};
use crate::readline::readline_word_column_range;
use crate::{MuxError, Result};
use mez_terminal::{
    TerminalScreen, TerminalStyledLine, terminal_emoji_width, terminal_grapheme_width,
    terminal_graphemes, terminal_text_width,
};
use std::ops::{Deref, DerefMut};

/// Copy-text marker for presentation-only continuation rows.
pub const COPY_SKIP_LINE: &str = "\u{1e}mez-copy-skip-line";
/// Prefix carrying one rich-text source-line identity and raw text.
pub const COPY_SOURCE_LINE_PREFIX: &str = "\u{1e}mez-copy-source-line:";
/// Copy-text marker for wrapped rich-text continuation rows.
pub const COPY_WRAP_CONTINUATION: &str = "\u{1e}mez-copy-wrap-continuation";

/// Encodes one rich-text source-line identity with its raw copy text.
pub fn encode_copy_source_line(source_index: usize, copy_line: &str) -> String {
    format!("{COPY_SOURCE_LINE_PREFIX}{source_index}:{copy_line}")
}

/// Identifies one terminal-cell position in a copy-mode buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CopyPosition {
    /// Zero-based logical line in the copy-mode buffer.
    pub line: usize,
    /// Zero-based terminal-cell column within the line.
    pub column: usize,
}

/// Direction used when searching a multiplexer-owned copy buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    /// Search toward later lines, wrapping to the beginning when needed.
    Forward,
    /// Search toward earlier lines, wrapping to the end when needed.
    Backward,
}

/// Keyboard actions supported while copy mode owns attached-client input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyModeKeyAction {
    /// Moves the copy-mode cursor up by one line.
    MoveUp,
    /// Moves the copy-mode cursor up by five lines.
    MoveUpFast,
    /// Moves the copy-mode cursor down by one line.
    MoveDown,
    /// Moves the copy-mode cursor down by five lines.
    MoveDownFast,
    /// Moves the copy-mode cursor left by one cell.
    MoveLeft,
    /// Moves the copy-mode cursor left by one word-like segment.
    MoveWordLeft,
    /// Moves the copy-mode cursor right by one cell.
    MoveRight,
    /// Moves the copy-mode cursor right by one word-like segment.
    MoveWordRight,
    /// Moves the viewport up by one page.
    PageUp,
    /// Moves the viewport down by one page.
    PageDown,
    /// Moves to the top of the copy buffer.
    Top,
    /// Moves to the beginning of the current line.
    LineStart,
    /// Moves to the bottom of the copy buffer.
    Bottom,
    /// Moves to the end of the current line.
    LineEnd,
    /// Starts or completes a keyboard selection.
    BeginSelection,
    /// Consumes an unbound key without forwarding it to the pane.
    Ignore,
    /// Exits copy mode.
    Cancel,
}

/// Result of applying one keyboard action to mux-owned copy state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyModeActionOutcome {
    /// Copy navigation or selection state changed and should be redrawn.
    Updated,
    /// The current selection is ready for product-owned clipboard handling.
    SelectionReady,
    /// The key was consumed without changing copy state.
    Ignored,
    /// Copy mode should close and the underlying pane should be redrawn.
    Exit,
}

impl CopyModeActionOutcome {
    /// Returns whether applying this outcome requires a new client frame.
    pub const fn requires_redraw(self) -> bool {
        !matches!(self, Self::Ignored)
    }
}

/// Classifies one complete attached-client key sequence for copy mode.
pub fn classify_copy_mode_key_action(input: &[u8]) -> Option<CopyModeKeyAction> {
    if input == b"\x1b" {
        return Some(CopyModeKeyAction::Cancel);
    }
    if input == b"\x03" {
        return Some(CopyModeKeyAction::Ignore);
    }
    let (chord, consumed) = parse_key_chord_bytes(input)?;
    if consumed != input.len() {
        return None;
    }
    match chord.code {
        KeyCode::Up if chord.modifiers.ctrl => Some(CopyModeKeyAction::MoveUpFast),
        KeyCode::Up => Some(CopyModeKeyAction::MoveUp),
        KeyCode::Down if chord.modifiers.ctrl => Some(CopyModeKeyAction::MoveDownFast),
        KeyCode::Down => Some(CopyModeKeyAction::MoveDown),
        KeyCode::Left if chord.modifiers.ctrl || chord.modifiers.alt => {
            Some(CopyModeKeyAction::MoveWordLeft)
        }
        KeyCode::Left => Some(CopyModeKeyAction::MoveLeft),
        KeyCode::Right if chord.modifiers.ctrl || chord.modifiers.alt => {
            Some(CopyModeKeyAction::MoveWordRight)
        }
        KeyCode::Right => Some(CopyModeKeyAction::MoveRight),
        KeyCode::PageUp => Some(CopyModeKeyAction::PageUp),
        KeyCode::PageDown => Some(CopyModeKeyAction::PageDown),
        KeyCode::Home if chord.modifiers.ctrl => Some(CopyModeKeyAction::Top),
        KeyCode::Home => Some(CopyModeKeyAction::LineStart),
        KeyCode::End if chord.modifiers.ctrl => Some(CopyModeKeyAction::Bottom),
        KeyCode::End => Some(CopyModeKeyAction::LineEnd),
        KeyCode::Char(' ') => Some(CopyModeKeyAction::BeginSelection),
        _ => None,
    }
}

/// Owns dependency-neutral copy-mode navigation and selection state.
///
/// Display styling and product-specific clipboard normalization remain in the
/// composition crate; this type only tracks a line buffer, viewport, cursor,
/// search results, and selection transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyBuffer {
    lines: Vec<String>,
    viewport_rows: usize,
    scroll_top: usize,
    cursor: CopyPosition,
    selection: Option<(CopyPosition, CopyPosition)>,
    selection_anchor: Option<CopyPosition>,
}

impl CopyBuffer {
    /// Creates navigation state for a copy buffer.
    pub fn new(
        lines: Vec<String>,
        viewport_rows: usize,
        scroll_top: usize,
        cursor: CopyPosition,
    ) -> Result<Self> {
        if viewport_rows == 0 {
            return Err(MuxError::invalid_args(
                "copy mode viewport must have at least one row",
            ));
        }
        let mut buffer = Self {
            lines,
            viewport_rows,
            scroll_top,
            cursor,
            selection: None,
            selection_anchor: None,
        };
        buffer.scroll_top = buffer
            .scroll_top
            .min(buffer.lines.len().saturating_sub(buffer.viewport_rows));
        buffer.cursor = buffer.clamp_position(buffer.cursor);
        buffer.keep_cursor_visible();
        Ok(buffer)
    }

    /// Returns all display lines in the copy buffer.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Returns the zero-based first visible line.
    pub fn scroll_top(&self) -> usize {
        self.scroll_top
    }

    /// Returns the number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Returns the one-based line boundary at the bottom of the viewport.
    pub fn visible_end_line(&self) -> usize {
        self.scroll_top
            .saturating_add(self.viewport_rows)
            .min(self.lines.len())
    }

    /// Returns whether the viewport is at the live bottom of the buffer.
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_top >= self.lines.len().saturating_sub(self.viewport_rows)
    }

    /// Returns the display lines visible in the viewport.
    pub fn visible_lines(&self) -> &[String] {
        &self.lines[self.scroll_top..self.visible_end_line()]
    }

    /// Updates the viewport height and clamps dependent state.
    pub fn resize_viewport_rows(&mut self, viewport_rows: usize) -> Result<()> {
        if viewport_rows == 0 {
            return Err(MuxError::invalid_args(
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

    /// Scrolls the viewport by a signed line count.
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

    /// Scrolls one page toward the start of the buffer.
    pub fn page_up(&mut self) {
        if self.scroll_top < self.viewport_rows {
            self.scroll_to_top();
        } else {
            self.scroll_by(-(self.viewport_rows as isize));
        }
    }

    /// Scrolls one page toward the end of the buffer.
    pub fn page_down(&mut self) {
        let max_top = self.lines.len().saturating_sub(self.viewport_rows);
        if max_top.saturating_sub(self.scroll_top) < self.viewport_rows {
            self.scroll_to_bottom();
        } else {
            self.scroll_by(self.viewport_rows as isize);
        }
    }

    /// Moves the viewport and cursor to the beginning of the buffer.
    pub fn scroll_to_top(&mut self) {
        self.scroll_top = 0;
        self.cursor = CopyPosition { line: 0, column: 0 };
        self.update_keyboard_selection();
    }

    /// Moves the viewport and cursor to the end of the buffer.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_top = self.lines.len().saturating_sub(self.viewport_rows);
        self.cursor.line = self.lines.len().saturating_sub(1);
        self.cursor.column = self.line_width(self.cursor.line);
        self.update_keyboard_selection();
    }

    /// Returns the keyboard cursor position.
    pub fn cursor(&self) -> CopyPosition {
        self.cursor
    }

    /// Clamps one position to the available copy buffer.
    pub fn clamp_position(&self, position: CopyPosition) -> CopyPosition {
        let line = position.line.min(self.lines.len().saturating_sub(1));
        CopyPosition {
            line,
            column: position.column.min(self.line_width(line)),
        }
    }

    /// Moves the cursor by signed line and display-column deltas.
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

    /// Moves the cursor to the beginning of its line.
    pub fn move_cursor_to_line_start(&mut self) {
        self.cursor.column = 0;
        self.update_keyboard_selection();
    }

    /// Moves the cursor to the end of its line.
    pub fn move_cursor_to_line_end(&mut self) {
        self.cursor.column = self.line_width(self.cursor.line);
        self.update_keyboard_selection();
    }

    /// Moves the cursor to the previous readline-style word boundary.
    pub fn move_cursor_word_left(&mut self) {
        self.cursor.column = self
            .lines
            .get(self.cursor.line)
            .map(|line| previous_word_column(line, self.cursor.column))
            .unwrap_or_default();
        self.update_keyboard_selection();
    }

    /// Moves the cursor to the next readline-style word boundary.
    pub fn move_cursor_word_right(&mut self) {
        self.cursor.column = self
            .lines
            .get(self.cursor.line)
            .map(|line| next_word_column(line, self.cursor.column))
            .unwrap_or_default();
        self.update_keyboard_selection();
    }

    /// Begins a keyboard selection at the current cursor.
    pub fn begin_keyboard_selection(&mut self) {
        self.selection_anchor = Some(self.cursor);
        self.selection = Some((self.cursor, self.cursor));
    }

    /// Applies one classified copy-mode key action to this buffer.
    ///
    /// Clipboard text normalization and copy-mode lifecycle ownership remain
    /// with the product adapter. `SelectionReady` deliberately leaves the
    /// selection intact so that adapter can copy styled or source-aware text
    /// before clearing it.
    pub fn apply_key_action(&mut self, action: CopyModeKeyAction) -> CopyModeActionOutcome {
        match action {
            CopyModeKeyAction::MoveUp => self.move_cursor_by(-1, 0),
            CopyModeKeyAction::MoveUpFast => self.move_cursor_by(-5, 0),
            CopyModeKeyAction::MoveDown => self.move_cursor_by(1, 0),
            CopyModeKeyAction::MoveDownFast => self.move_cursor_by(5, 0),
            CopyModeKeyAction::MoveLeft => self.move_cursor_by(0, -1),
            CopyModeKeyAction::MoveWordLeft => self.move_cursor_word_left(),
            CopyModeKeyAction::MoveRight => self.move_cursor_by(0, 1),
            CopyModeKeyAction::MoveWordRight => self.move_cursor_word_right(),
            CopyModeKeyAction::PageUp => self.page_up(),
            CopyModeKeyAction::PageDown => self.page_down(),
            CopyModeKeyAction::Top => self.scroll_to_top(),
            CopyModeKeyAction::LineStart => self.move_cursor_to_line_start(),
            CopyModeKeyAction::Bottom => self.scroll_to_bottom(),
            CopyModeKeyAction::LineEnd => self.move_cursor_to_line_end(),
            CopyModeKeyAction::BeginSelection if self.selection().is_some() => {
                return CopyModeActionOutcome::SelectionReady;
            }
            CopyModeKeyAction::BeginSelection => self.begin_keyboard_selection(),
            CopyModeKeyAction::Ignore => return CopyModeActionOutcome::Ignored,
            CopyModeKeyAction::Cancel => return CopyModeActionOutcome::Exit,
        }
        CopyModeActionOutcome::Updated
    }

    /// Searches the buffer and selects the matching text.
    pub fn search(
        &mut self,
        query: &str,
        direction: SearchDirection,
    ) -> Result<Option<CopyPosition>> {
        if query.is_empty() {
            return Err(MuxError::invalid_args(
                "copy mode search query must not be empty",
            ));
        }
        if let Some((position, width)) =
            search_lines(&self.lines, self.scroll_top, query, direction)
        {
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

    /// Sets an explicit selection range.
    pub fn select_range(&mut self, start: CopyPosition, end: CopyPosition) -> Result<()> {
        validate_position(&self.lines, start)?;
        validate_position(&self.lines, end)?;
        self.selection = Some((start, end));
        Ok(())
    }

    /// Selects the readline-style word surrounding one position.
    pub fn select_word_at(&mut self, position: CopyPosition) -> Result<()> {
        let position = self.clamp_position(position);
        let Some(line) = self.lines.get(position.line) else {
            return Err(MuxError::invalid_args(
                "copy mode word selection line is invalid",
            ));
        };
        let scalar = display_column_to_scalar_index(line, position.column);
        let (start, end) = readline_word_column_range(line, scalar);
        self.select_range(
            CopyPosition {
                line: position.line,
                column: scalar_index_to_display_column(line, start),
            },
            CopyPosition {
                line: position.line,
                column: scalar_index_to_display_column(line, end),
            },
        )
    }

    /// Clears the current selection and keyboard anchor.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
    }

    /// Returns the current selection range.
    pub fn selection(&self) -> Option<(CopyPosition, CopyPosition)> {
        self.selection
    }

    fn line_width(&self, line: usize) -> usize {
        self.lines
            .get(line)
            .map(|line| terminal_text_width(line, terminal_emoji_width()))
            .unwrap_or_default()
    }

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

    fn move_cursor_left_one(&mut self) {
        if self.cursor.column > 0 {
            self.cursor.column = self
                .lines
                .get(self.cursor.line)
                .and_then(|line| grapheme_at_column(line, self.cursor.column.saturating_sub(1)))
                .map(|(start, _)| start)
                .unwrap_or_else(|| self.cursor.column.saturating_sub(1));
        } else if self.cursor.line > 0 {
            self.cursor.line = self.cursor.line.saturating_sub(1);
            self.cursor.column = self.line_width(self.cursor.line);
        }
    }

    fn move_cursor_right_one(&mut self) {
        let line_width = self.line_width(self.cursor.line);
        if self.cursor.column < line_width {
            self.cursor.column = self
                .lines
                .get(self.cursor.line)
                .and_then(|line| grapheme_at_column(line, self.cursor.column))
                .map(|(start, width)| start.saturating_add(width))
                .unwrap_or_else(|| self.cursor.column.saturating_add(1));
        } else if self.cursor.line.saturating_add(1) < self.lines.len() {
            self.cursor.line = self.cursor.line.saturating_add(1);
            self.cursor.column = 0;
        }
    }

    fn keep_cursor_visible(&mut self) {
        if self.cursor.line < self.scroll_top {
            self.scroll_top = self.cursor.line;
        } else if self.cursor.line >= self.scroll_top.saturating_add(self.viewport_rows) {
            self.scroll_top = self
                .cursor
                .line
                .saturating_sub(self.viewport_rows.saturating_sub(1));
        }
    }

    fn update_keyboard_selection(&mut self) {
        if let Some(anchor) = self.selection_anchor {
            self.selection = Some((anchor, self.cursor));
        }
    }
}

/// Mux-owned styled copy-mode state built from one terminal surface.
///
/// This type keeps terminal-derived display rows, source-aware copy text, and
/// navigation state aligned. Product adapters may interpret the copy-text
/// metadata, but they do not own viewport, cursor, search, or selection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledCopyMode {
    buffer: CopyBuffer,
    copy_lines: Vec<String>,
    styled_lines: Vec<TerminalStyledLine>,
    alternate_screen_was_active: bool,
}

impl StyledCopyMode {
    /// Builds copy state from normal-screen history and live terminal rows.
    pub fn from_screen(screen: &TerminalScreen, viewport_rows: usize) -> Result<Self> {
        let styled_lines = screen.normal_styled_content_lines();
        let (scroll_top, cursor) = initial_copy_mode_view(screen, &styled_lines, viewport_rows);
        Self::new(
            styled_lines,
            viewport_rows,
            scroll_top,
            cursor,
            screen.alternate_screen_active(),
        )
    }

    /// Builds copy state from only the currently visible terminal rows.
    pub fn from_visible_screen(screen: &TerminalScreen, viewport_rows: usize) -> Result<Self> {
        Self::new(
            screen.visible_styled_lines(),
            viewport_rows,
            0,
            CopyPosition { line: 0, column: 0 },
            screen.alternate_screen_active(),
        )
    }

    /// Builds styled copy state from explicit terminal rows.
    pub fn new(
        styled_lines: Vec<TerminalStyledLine>,
        viewport_rows: usize,
        scroll_top: usize,
        cursor: CopyPosition,
        alternate_screen_was_active: bool,
    ) -> Result<Self> {
        let lines = styled_lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let copy_lines = styled_lines
            .iter()
            .map(|line| line.copy_text.clone().unwrap_or_else(|| line.text.clone()))
            .collect::<Vec<_>>();
        Ok(Self {
            buffer: CopyBuffer::new(lines, viewport_rows, scroll_top, cursor)?,
            copy_lines,
            styled_lines,
            alternate_screen_was_active,
        })
    }

    /// Returns the underlying navigation and selection state.
    pub fn buffer(&self) -> &CopyBuffer {
        &self.buffer
    }

    /// Returns mutable navigation and selection state.
    pub fn buffer_mut(&mut self) -> &mut CopyBuffer {
        &mut self.buffer
    }

    /// Returns copy-text metadata parallel to the display rows.
    pub fn copy_lines(&self) -> &[String] {
        &self.copy_lines
    }

    /// Returns all styled terminal rows in the copy buffer.
    pub fn styled_lines(&self) -> &[TerminalStyledLine] {
        &self.styled_lines
    }

    /// Returns styled rows currently visible in the copy viewport.
    pub fn visible_styled_lines(&self) -> &[TerminalStyledLine] {
        &self.styled_lines[self.buffer.scroll_top()..self.buffer.visible_end_line()]
    }

    /// Returns whether the terminal used its alternate screen at entry.
    pub fn alternate_screen_was_active(&self) -> bool {
        self.alternate_screen_was_active
    }
}

impl Deref for StyledCopyMode {
    type Target = CopyBuffer;

    fn deref(&self) -> &Self::Target {
        &self.buffer
    }
}

impl DerefMut for StyledCopyMode {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffer
    }
}

/// Computes the initial viewport and keyboard cursor from terminal state.
fn initial_copy_mode_view(
    screen: &TerminalScreen,
    styled_lines: &[TerminalStyledLine],
    viewport_rows: usize,
) -> (usize, CopyPosition) {
    let mut scroll_top = styled_lines.len().saturating_sub(viewport_rows);
    if styled_lines.is_empty() || screen.alternate_screen_active() {
        return (
            scroll_top,
            CopyPosition {
                line: scroll_top,
                column: 0,
            },
        );
    }

    let live_rows = usize::from(screen.size().rows).max(1);
    let live_base_line = styled_lines.len().saturating_sub(live_rows);
    let terminal_cursor = screen.cursor_state();
    let cursor_line = live_base_line
        .saturating_add(terminal_cursor.row.min(live_rows.saturating_sub(1)))
        .min(styled_lines.len().saturating_sub(1));
    let cursor_column = styled_lines
        .get(cursor_line)
        .map(|line| {
            terminal_cursor
                .column
                .min(terminal_text_width(&line.text, terminal_emoji_width()))
        })
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

/// Finds the grapheme cluster covering a display column and returns its
/// starting display column and total cell width.
fn grapheme_at_column(line: &str, target: usize) -> Option<(usize, usize)> {
    let mut col = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = terminal_grapheme_width(grapheme, terminal_emoji_width());
        let next = col.saturating_add(width);
        if target >= col && target < next {
            return Some((col, width));
        }
        col = next;
    }
    None
}

/// Converts a display-column position into its corresponding Unicode scalar
/// index so readline word-boundary behavior can operate on it.
fn display_column_to_scalar_index(line: &str, target_column: usize) -> usize {
    let mut col = 0usize;
    let mut scalar = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = terminal_grapheme_width(grapheme, terminal_emoji_width());
        let next = col.saturating_add(width);
        if target_column < next {
            return scalar;
        }
        scalar += grapheme.chars().count();
        col = next;
    }
    scalar
}

/// Converts a Unicode scalar index back to its display column.
fn scalar_index_to_display_column(line: &str, target_scalar: usize) -> usize {
    let mut col = 0usize;
    let mut scalar = 0usize;
    for grapheme in terminal_graphemes(line) {
        if scalar >= target_scalar {
            break;
        }
        scalar += grapheme.chars().count();
        col = col.saturating_add(terminal_grapheme_width(grapheme, terminal_emoji_width()));
    }
    col
}

/// Returns the display column reached by moving backward one word segment.
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

/// Returns the display column reached by moving forward one word segment.
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

/// Searches copy-buffer lines in the requested direction, wrapping once.
///
/// Returns the terminal-cell position and display width of the first match.
pub fn search_lines(
    lines: &[String],
    start_line: usize,
    query: &str,
    direction: SearchDirection,
) -> Option<(CopyPosition, usize)> {
    if lines.is_empty() {
        return None;
    }

    let start = start_line.min(lines.len().saturating_sub(1));
    let indices: Box<dyn Iterator<Item = usize>> = match direction {
        SearchDirection::Forward => Box::new((start..lines.len()).chain(0..start)),
        SearchDirection::Backward => {
            Box::new((0..=start).rev().chain(((start + 1)..lines.len()).rev()))
        }
    };

    for line_index in indices {
        let line = &lines[line_index];
        let Some(byte_index) = (match direction {
            SearchDirection::Forward => line.find(query),
            SearchDirection::Backward => line.rfind(query),
        }) else {
            continue;
        };
        return Some((
            CopyPosition {
                line: line_index,
                column: terminal_text_width(&line[..byte_index], terminal_emoji_width()),
            },
            terminal_text_width(query, terminal_emoji_width()),
        ));
    }
    None
}

/// Orders a copy-mode selection from its earlier position to its later one.
pub fn normalize_selection(start: CopyPosition, end: CopyPosition) -> (CopyPosition, CopyPosition) {
    if start <= end {
        (start, end)
    } else {
        (end, start)
    }
}

/// Validates that a copy position references an existing buffer line.
pub fn validate_position(lines: &[String], position: CopyPosition) -> Result<()> {
    if lines.get(position.line).is_none() {
        return Err(MuxError::invalid_args(
            "copy mode selection line is out of range",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies copy mode classifies navigation, selection, cancellation, and
    /// unbound input without forwarding owned keystrokes to a pane process.
    #[test]
    fn copy_mode_key_actions_are_classified_by_mux() {
        assert_eq!(
            classify_copy_mode_key_action(b"\x1b[5~"),
            Some(CopyModeKeyAction::PageUp)
        );
        assert_eq!(
            classify_copy_mode_key_action(b" "),
            Some(CopyModeKeyAction::BeginSelection)
        );
        assert_eq!(
            classify_copy_mode_key_action(b"\x1b"),
            Some(CopyModeKeyAction::Cancel)
        );
        assert_eq!(
            classify_copy_mode_key_action(b"\x03"),
            Some(CopyModeKeyAction::Ignore)
        );
        assert_eq!(classify_copy_mode_key_action(b"q"), None);
    }

    /// Verifies styled copy state keeps terminal display rows, source-aware
    /// copy text, viewport state, and navigation in one mux-owned structure.
    #[test]
    fn styled_copy_mode_owns_terminal_derived_copy_state() {
        let mut source_line = TerminalStyledLine::plain("rendered source");
        source_line.copy_text = Some("# raw source".to_string());
        let styled_lines = vec![TerminalStyledLine::plain("first"), source_line];

        let mut copy = StyledCopyMode::new(
            styled_lines.clone(),
            1,
            1,
            CopyPosition { line: 1, column: 0 },
            true,
        )
        .unwrap();

        assert_eq!(copy.styled_lines(), styled_lines);
        assert_eq!(copy.copy_lines(), ["first", "# raw source"]);
        assert_eq!(copy.visible_styled_lines(), &styled_lines[1..]);
        assert!(copy.alternate_screen_was_active());
        copy.move_cursor_to_line_end();
        assert_eq!(copy.cursor().column, "rendered source".len());
    }

    /// Verifies styled copy state preserves mux-owned invalid viewport
    /// validation instead of reintroducing product error handling.
    #[test]
    fn styled_copy_mode_rejects_empty_viewport() {
        let error = StyledCopyMode::new(
            vec![TerminalStyledLine::plain("line")],
            0,
            0,
            CopyPosition { line: 0, column: 0 },
            false,
        )
        .unwrap_err();

        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
    }

    /// Verifies the mux-owned copy buffer keeps Unicode cursor movement,
    /// viewport scrolling, and keyboard selections synchronized.
    #[test]
    fn copy_buffer_owns_navigation_and_selection_state() {
        let mut buffer = CopyBuffer::new(
            vec![
                "first".to_string(),
                "中 target".to_string(),
                "last".to_string(),
            ],
            2,
            1,
            CopyPosition { line: 1, column: 0 },
        )
        .unwrap();

        buffer.begin_keyboard_selection();
        buffer.move_cursor_by(0, 1);
        assert_eq!(buffer.cursor(), CopyPosition { line: 1, column: 2 });
        assert_eq!(
            buffer.selection(),
            Some((
                CopyPosition { line: 1, column: 0 },
                CopyPosition { line: 1, column: 2 }
            ))
        );

        buffer.page_down();
        assert!(buffer.is_at_bottom());
        assert_eq!(
            buffer.visible_lines(),
            &["中 target".to_string(), "last".to_string()]
        );
    }

    /// Verifies invalid viewport sizes and empty search queries retain
    /// mux-owned invalid-argument errors at the copy-state boundary.
    #[test]
    fn copy_buffer_rejects_invalid_navigation_inputs() {
        let error = CopyBuffer::new(
            vec!["line".to_string()],
            0,
            0,
            CopyPosition { line: 0, column: 0 },
        )
        .unwrap_err();
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);

        let mut buffer = CopyBuffer::new(
            vec!["line".to_string()],
            1,
            0,
            CopyPosition { line: 0, column: 0 },
        )
        .unwrap();
        let error = buffer.search("", SearchDirection::Forward).unwrap_err();
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert_eq!(error.message(), "copy mode search query must not be empty");
    }

    /// Verifies forward and backward searches wrap around the copy buffer while
    /// reporting terminal-cell columns rather than UTF-8 byte offsets.
    #[test]
    fn copy_search_wraps_and_reports_terminal_columns() {
        let lines = vec!["first target".to_string(), "中 target".to_string()];

        assert_eq!(
            search_lines(&lines, 1, "target", SearchDirection::Forward),
            Some((CopyPosition { line: 1, column: 3 }, 6))
        );
        assert_eq!(
            search_lines(&lines, 1, "first", SearchDirection::Forward),
            Some((CopyPosition { line: 0, column: 0 }, 5))
        );
        assert_eq!(
            search_lines(&lines, 0, "target", SearchDirection::Backward),
            Some((CopyPosition { line: 0, column: 6 }, 6))
        );
        assert_eq!(
            search_lines(&lines, 0, "中", SearchDirection::Backward),
            Some((CopyPosition { line: 1, column: 0 }, 2))
        );
    }

    /// Verifies selection normalization preserves already ordered ranges and
    /// reverses ranges created by backward mouse drags.
    #[test]
    fn copy_selection_normalizes_position_order() {
        let earlier = CopyPosition { line: 1, column: 2 };
        let later = CopyPosition { line: 3, column: 4 };

        assert_eq!(normalize_selection(earlier, later), (earlier, later));
        assert_eq!(normalize_selection(later, earlier), (earlier, later));
    }

    /// Verifies copy-position validation accepts existing lines and reports a
    /// mux-owned invalid-argument error for positions outside the buffer.
    #[test]
    fn copy_position_validation_rejects_missing_lines() {
        let lines = vec!["only line".to_string()];

        assert!(
            validate_position(
                &lines,
                CopyPosition {
                    line: 0,
                    column: 99
                }
            )
            .is_ok()
        );
        let error = validate_position(&lines, CopyPosition { line: 1, column: 0 }).unwrap_err();
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert_eq!(error.message(), "copy mode selection line is out of range");
    }
}
