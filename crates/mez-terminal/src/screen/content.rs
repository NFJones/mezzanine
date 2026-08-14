//! Public content and history projections for a terminal screen.
//!
//! This module exposes visible lines, styled lines, history, and grid size from
//! canonical screen state. It does not parse input or perform cell edits.

use super::*;

impl TerminalScreen {
    /// Returns the current terminal grid dimensions.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Runs the visible lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn visible_lines(&self) -> Vec<String> {
        self.cells.iter().map(|row| trim_screen_row(row)).collect()
    }

    /// Returns inline-scalar and owned-grapheme cell counts for focused tests.
    #[cfg(test)]
    pub(crate) fn visible_cell_storage_counts(&self) -> (usize, usize) {
        self.cells.iter().flatten().fold(
            (0usize, 0usize),
            |(inline_scalars, owned_graphemes), cell| {
                (
                    inline_scalars.saturating_add(usize::from(cell.is_inline_scalar())),
                    owned_graphemes.saturating_add(usize::from(cell.is_owned_grapheme())),
                )
            },
        )
    }

    /// Returns visible lines with non-default SGR style spans preserved.
    pub fn visible_styled_lines(&self) -> Vec<TerminalStyledLine> {
        self.cells
            .iter()
            .zip(self.renditions.iter())
            .enumerate()
            .map(|(row, (cells, renditions))| {
                styled_line_from_row_with_copy_text(
                    cells,
                    renditions,
                    self.line_copy_texts.get(row).cloned().flatten(),
                    self.emoji_width,
                )
            })
            .collect()
    }

    /// Runs the normal content lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn normal_content_lines(&self) -> Vec<String> {
        let mut lines = self
            .history
            .lines()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !self.alternate.active() {
            lines.extend(self.visible_lines());
        }
        lines
    }

    /// Returns normal-screen history and visible rows with SGR style spans.
    pub fn normal_styled_content_lines(&self) -> Vec<TerminalStyledLine> {
        let mut lines = self.history.styled_lines().collect::<Vec<_>>();
        if !self.alternate.active() {
            lines.extend(self.visible_styled_lines());
        }
        lines
    }

    /// Assigns raw-copy text to the most recent normal-screen logical lines.
    ///
    /// Presentation renderers use this after feeding transformed display text
    /// into the terminal screen. Copy mode can then recover the source text
    /// even when the visible line has been styled or simplified for display.
    pub fn set_recent_normal_copy_texts(
        &mut self,
        copy_texts: &[String],
        continuation_copy_text: &str,
    ) {
        if self.set_recent_normal_copy_texts_inner(copy_texts, continuation_copy_text) > 0 {
            self.mark_render_changed();
        }
    }

    /// Assigns recent copy text and returns inspected physical-row count for tests.
    #[cfg(test)]
    pub(crate) fn set_recent_normal_copy_texts_with_inspection_count(
        &mut self,
        copy_texts: &[String],
        continuation_copy_text: &str,
    ) -> usize {
        self.set_recent_normal_copy_texts_inner(copy_texts, continuation_copy_text)
    }

    /// Performs a bounded reverse walk over newest normal-screen logical lines.
    fn set_recent_normal_copy_texts_inner(
        &mut self,
        copy_texts: &[String],
        continuation_copy_text: &str,
    ) -> usize {
        if copy_texts.is_empty() || self.alternate.active() {
            return 0;
        }
        let mut inspected_rows = 0usize;
        let mut target_end = self.normal_physical_line_count();
        while target_end > 0 {
            let target = target_end.saturating_sub(1);
            inspected_rows = inspected_rows.saturating_add(1);
            if self.normal_physical_line_wraps_to_next(target)
                || !self.normal_physical_line_is_blank(target)
            {
                break;
            }
            target_end = target;
        }

        for copy_text in copy_texts.iter().rev() {
            if target_end == 0 {
                break;
            }
            let mut start = target_end.saturating_sub(1);
            while start > 0 {
                inspected_rows = inspected_rows.saturating_add(1);
                if !self.normal_physical_line_wraps_to_next(start.saturating_sub(1)) {
                    break;
                }
                start = start.saturating_sub(1);
            }
            if let Some(index) = self.normal_physical_line_index(start) {
                self.assign_normal_physical_copy_text(index, Some(copy_text.clone()));
            }
            for offset in start.saturating_add(1)..target_end {
                if let Some(index) = self.normal_physical_line_index(offset) {
                    self.assign_normal_physical_copy_text(
                        index,
                        Some(continuation_copy_text.to_string()),
                    );
                }
            }
            target_end = start;
        }
        inspected_rows
    }

    /// Runs the history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn history(&self) -> &HistoryBuffer {
        &self.history
    }

    /// Runs the history limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn history_limit(&self) -> usize {
        self.history.limit()
    }

    /// Returns the configured history rotation batch size.
    pub fn history_rotate_lines(&self) -> usize {
        self.history.rotate_lines()
    }

    /// Runs the set history limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_history_limit(
        &mut self,
        limit: usize,
    ) -> std::result::Result<(), TerminalScreenConfigError> {
        self.history.set_limit(limit).map_err(Into::into)
    }

    /// Updates the history rotation batch size.
    pub fn set_history_rotate_lines(
        &mut self,
        rotate_lines: usize,
    ) -> std::result::Result<(), TerminalScreenConfigError> {
        self.history
            .set_rotate_lines(rotate_lines)
            .map_err(Into::into)
    }

    /// Runs the clear history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.normal_viewport_detached_from_history = false;
        self.mark_render_changed();
    }

    /// Scrolls the used normal-screen viewport into history and blanks it.
    ///
    /// Pane-local UI clears, such as entering agent mode or handling `Ctrl+L`,
    /// should remove existing text from the live viewport without erasing it
    /// from copyable pane logs. Alternate-screen contents are intentionally not
    /// recorded to normal history.
    pub fn clear_visible_into_history(&mut self) {
        self.mark_render_changed();
        if self.alternate.active() {
            self.clear_screen();
            return;
        }
        if let Some(last_row) = self.last_significant_row() {
            for row in 0..=last_row {
                self.history.push_styled_line_with_wrap(
                    styled_line_from_row_with_copy_text(
                        &self.cells[row],
                        &self.renditions[row],
                        self.line_copy_texts.get(row).cloned().flatten(),
                        self.emoji_width,
                    ),
                    self.line_wraps.get(row).copied().unwrap_or(false),
                );
            }
        }
        self.clear_screen();
        self.normal_viewport_detached_from_history = true;
    }

    /// Returns whether a UI clear detached the normal viewport from scrollback.
    ///
    /// A detached viewport must remain blank through later resizes until new
    /// terminal output arrives, while its prior rows remain available in history.
    pub fn normal_viewport_detached_from_history(&self) -> bool {
        self.normal_viewport_detached_from_history
    }

    /// Restores plain normal-screen history and styled visible rows.
    ///
    /// Snapshot resume uses this path to rebuild a non-live pane's rendered
    /// terminal contents without replaying the original PTY byte stream when
    /// the persisted history source has no style metadata.
    pub fn restore_normal_styled_content(
        &mut self,
        history_lines: &[String],
        visible_lines: &[TerminalStyledLine],
    ) {
        let history_lines = history_lines
            .iter()
            .map(|line| TerminalStyledLine::plain(line.clone()))
            .collect::<Vec<_>>();
        self.restore_normal_styled_history_content(&history_lines, visible_lines);
    }

    /// Restores styled normal-screen history and visible rows.
    pub fn restore_normal_styled_history_content(
        &mut self,
        history_lines: &[TerminalStyledLine],
        visible_lines: &[TerminalStyledLine],
    ) {
        self.mark_render_changed();
        self.history.clear();
        for line in history_lines {
            self.history.push_styled_line(line.clone());
        }

        self.alternate = AlternateScreenState::new();
        self.cells = blank_cells(self.size);
        self.renditions = blank_renditions(self.size, GraphicRendition::default());
        self.line_wraps = vec![false; usize::from(self.size.rows)];
        self.line_copy_texts = vec![None; usize::from(self.size.rows)];
        self.cursor = Cursor { row: 0, column: 0 };
        self.cursor_visible = true;
        self.wrap_pending = false;
        self.saved_cursor = None;
        self.parser_state = ParserState::Ground;
        self.csi_buffer.clear();
        self.osc_buffer.clear();
        self.osc_buffer_truncated = false;
        self.osc_events.clear();
        self.bracketed_paste_enabled = false;
        self.normal_mouse_tracking_enabled = false;
        self.button_event_mouse_tracking_enabled = false;
        self.any_event_mouse_tracking_enabled = false;
        self.sgr_mouse_enabled = false;
        self.application_cursor_enabled = false;
        self.origin_mode_enabled = false;
        self.application_keypad_enabled = false;
        self.focus_events_enabled = false;
        self.g0_charset = TerminalCharset::Ascii;
        self.g1_charset = TerminalCharset::Ascii;
        self.shift_out = false;
        self.saved_dec_private_modes.clear();
        self.scroll_region = None;
        self.normal_viewport_detached_from_history = false;

        let rows = usize::from(self.size.rows);
        let start = visible_lines.len().saturating_sub(rows);
        for (row_index, line) in visible_lines.iter().skip(start).take(rows).enumerate() {
            write_styled_line_to_row(
                line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
                self.emoji_width,
            );
            self.line_copy_texts[row_index] = line.copy_text.clone();
        }
    }
}
