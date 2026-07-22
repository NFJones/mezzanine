//! Terminal-screen construction, reset, resize, and buffer lifecycle.
//!
//! This module establishes screen invariants and owns transitions that replace
//! or reshape whole buffers, including alternate-screen entry and restoration.
//! Incremental parsing and cell edits remain in sibling modules.

use super::*;

impl TerminalScreen {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        size: Size,
        history_limit: usize,
    ) -> std::result::Result<Self, TerminalScreenConfigError> {
        Self::new_with_history_config(size, history_limit, DEFAULT_HISTORY_ROTATE_LINES)
    }

    /// Builds a terminal screen with explicit history limit and rotation
    /// settings so runtime configuration can control bounded-history eviction.
    pub fn new_with_history_config(
        size: Size,
        history_limit: usize,
        history_rotate_lines: usize,
    ) -> std::result::Result<Self, TerminalScreenConfigError> {
        Ok(Self {
            size,
            cells: blank_cells(size),
            renditions: blank_renditions(size, GraphicRendition::default()),
            line_wraps: vec![false; usize::from(size.rows)],
            line_copy_texts: vec![None; usize::from(size.rows)],
            wrap_continuation_prefix: None,
            cursor: Cursor { row: 0, column: 0 },
            cursor_visible: true,
            autowrap_enabled: true,
            wrap_pending: false,
            saved_cursor: None,
            parser_state: ParserState::Ground,
            csi_buffer: String::new(),
            csi_buffer_truncated: false,
            osc_buffer: String::new(),
            osc_buffer_truncated: false,
            osc_events: Vec::new(),
            terminal_response_bytes: Vec::new(),
            title: None,
            graphic_rendition: GraphicRendition::default(),
            bracketed_paste_enabled: false,
            normal_mouse_tracking_enabled: false,
            button_event_mouse_tracking_enabled: false,
            any_event_mouse_tracking_enabled: false,
            sgr_mouse_enabled: false,
            application_cursor_enabled: false,
            origin_mode_enabled: false,
            line_feed_newline_enabled: false,
            application_keypad_enabled: false,
            focus_events_enabled: false,
            saved_dec_private_modes: BTreeMap::new(),
            scroll_region: None,
            alternate: AlternateScreenState::new(),
            alternate_screen_generation: 0,
            history: HistoryBuffer::new_with_rotation(history_limit, history_rotate_lines)?,
            normal_viewport_detached_from_history: false,
            activity_events: 0,
            bell_events: 0,
            g0_charset: TerminalCharset::Ascii,
            g1_charset: TerminalCharset::Ascii,
            shift_out: false,
            utf8_tail: Vec::new(),
        })
    }

    /// Configures a styled prefix to repeat on soft-wrapped continuation rows.
    ///
    /// An empty prefix disables the policy. Callers must write the same prefix
    /// with a non-default rendition at the start of the logical line for the
    /// screen to recognize it.
    pub fn set_wrap_continuation_prefix(&mut self, prefix: impl Into<String>) {
        let prefix = prefix.into();
        self.wrap_continuation_prefix = (!prefix.is_empty()).then_some(prefix);
    }

    /// Runs the feed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn feed(&mut self, input: &[u8]) {
        if !input.is_empty() {
            self.activity_events = self.activity_events.saturating_add(1);
        }
        let mut bytes = Vec::with_capacity(self.utf8_tail.len().saturating_add(input.len()));
        if !self.utf8_tail.is_empty() {
            bytes.extend_from_slice(&self.utf8_tail);
            self.utf8_tail.clear();
        }
        bytes.extend_from_slice(input);

        let mut offset = 0;
        while offset < bytes.len() {
            match std::str::from_utf8(&bytes[offset..]) {
                Ok(text) => {
                    for ch in text.chars() {
                        self.feed_char(ch);
                    }
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        let text = std::str::from_utf8(&bytes[offset..offset + valid_up_to])
                            .expect("valid UTF-8 prefix must decode");
                        for ch in text.chars() {
                            self.feed_char(ch);
                        }
                        offset += valid_up_to;
                    }

                    match error.error_len() {
                        Some(error_len) => {
                            let invalid_end = offset + error_len;
                            let text = String::from_utf8_lossy(&bytes[offset..invalid_end]);
                            for ch in text.chars() {
                                self.feed_char(ch);
                            }
                            offset = invalid_end;
                        }
                        None => {
                            self.utf8_tail.extend_from_slice(&bytes[offset..]);
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Runs the resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize(&mut self, size: Size) {
        if self.size == size {
            return;
        }

        // DECSTBM margins describe coordinates in the current grid. Reset
        // them before choosing a resize strategy so stale bounds cannot drive
        // either reflow selection or later cursor movement outside the grid.
        self.scroll_region = None;

        if self.alternate.active() {
            self.resize_alternate_screen(size);
            return;
        }

        if self.normal_viewport_detached_from_history {
            self.resize_detached_normal_screen(size);
            return;
        }
        if self.normal_screen_viewport_is_cleared() {
            self.resize_cleared_normal_screen(size);
            return;
        }
        if self.size.columns == size.columns {
            self.resize_normal_screen_rows_only(size);
            return;
        }
        self.resize_normal_screen_reflowing(size);
    }

    /// Resizes the live alternate screen without recording application content
    /// in normal-screen history.
    ///
    /// Stable-width changes preserve physical cells, while width changes reflow
    /// the visible alternate grid using the same cursor and wrap invariants as
    /// the normal screen. Alternate content never enters scrollback.
    pub(super) fn resize_alternate_screen(&mut self, size: Size) {
        if self.size.columns == size.columns {
            self.resize_grid_preserving_cells(size);
        } else {
            self.resize_detached_normal_screen(size);
        }
    }
    /// Returns whether the live normal-screen viewport is intentionally blank.
    ///
    /// Pane-local clears such as `Ctrl+L` move visible rows into scrollback and
    /// reset the cursor to the origin. Subsequent resizes must preserve that
    /// cleared viewport instead of pulling scrollback back into view.
    pub(super) fn normal_screen_viewport_is_cleared(&self) -> bool {
        self.last_significant_row().is_none() && self.cursor.row == 0 && self.cursor.column == 0
    }
    /// Resizes an intentionally cleared normal-screen viewport.
    ///
    /// The resize keeps scrollback untouched and preserves the blank live pane
    /// presentation expected after pane-local clear operations.
    pub(super) fn resize_cleared_normal_screen(&mut self, size: Size) {
        self.size = size;
        self.clear_screen();
        let max_row = self.max_row();
        let max_column = self.max_column();
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }

    /// Resizes a normal-screen viewport that has been detached from scrollback.
    ///
    /// Shell clears such as `Ctrl+L` erase the live viewport while preserving
    /// scrollback. Until new output scrolls the pane again, row-only resizes
    /// must preserve the exact live viewport position, and width changes must
    /// reflow only the live rows without pulling adjacent history rows back
    /// into the visible grid.
    pub(super) fn resize_detached_normal_screen(&mut self, size: Size) {
        if self.size.columns == size.columns {
            self.resize_grid_preserving_cells(size);
            return;
        }
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        let preserve_bottom = new_rows < old_rows
            && (self.cursor.row >= new_rows || self.last_significant_row() >= Some(new_rows));
        let preserve_delayed_wrap = self.wrap_pending
            && self
                .leading_column_for_cell(self.cursor.row, self.cursor.column)
                .is_some_and(|column| {
                    self.cells[self.cursor.row][column].width() <= usize::from(size.columns)
                });
        let source_rows = self.current_visible_rows();
        let cursor = cursor_logical_position(&source_rows, self.cursor.row, self.cursor.column);
        let logical_lines =
            merge_wrapped_physical_lines(&source_rows, self.wrap_continuation_prefix.as_deref());
        let physical_rows = reflow_logical_lines(
            &logical_lines,
            usize::from(size.columns),
            self.wrap_continuation_prefix.as_deref(),
        );
        let visible_start = if preserve_bottom || physical_rows.len() > new_rows {
            physical_rows.len().saturating_sub(new_rows)
        } else {
            0
        };

        self.size = size;
        self.cells = blank_cells(size);
        self.renditions = blank_renditions(size, GraphicRendition::default());
        self.line_wraps = vec![false; new_rows];
        self.line_copy_texts = vec![None; new_rows];
        for (row_index, row) in physical_rows
            .iter()
            .skip(visible_start)
            .take(new_rows)
            .enumerate()
        {
            write_styled_line_to_row(
                &row.line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
            );
            self.line_wraps[row_index] = row.wraps_to_next;
            self.line_copy_texts[row_index] = row.line.copy_text.clone();
        }

        let max_row = self.max_row();
        let max_column = self.max_column();
        if let Some((logical_line, logical_column)) = cursor {
            let (absolute_row, column) = physical_position_for_logical_cursor(
                &logical_lines,
                logical_line,
                logical_column.saturating_add(usize::from(preserve_delayed_wrap)),
                usize::from(size.columns),
                self.wrap_continuation_prefix.as_deref(),
            );
            self.cursor.row = absolute_row.saturating_sub(visible_start).min(max_row);
            self.cursor.column = column.min(max_column);
        } else {
            self.cursor.row = self.cursor.row.min(max_row);
            self.cursor.column = self.cursor.column.min(max_column);
        }
        self.wrap_pending =
            preserve_delayed_wrap && self.autowrap_enabled && self.cursor.column == max_column;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }

    /// Runs the resize grid preserving cells operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn resize_grid_preserving_cells(&mut self, size: Size) {
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        let preserve_bottom = new_rows < old_rows
            && (self.cursor.row >= new_rows || self.last_significant_row() >= Some(new_rows));
        let row_offset = if preserve_bottom {
            old_rows.saturating_sub(new_rows)
        } else {
            0
        };
        let mut cells = blank_cells(size);
        let mut renditions = blank_renditions(size, GraphicRendition::default());
        let mut line_wraps = vec![false; usize::from(size.rows)];
        let mut line_copy_texts = vec![None; usize::from(size.rows)];
        let rows = old_rows.saturating_sub(row_offset).min(cells.len());
        let columns = self
            .cells
            .first()
            .map(Vec::len)
            .unwrap_or_default()
            .min(cells.first().map(Vec::len).unwrap_or_default());
        for (row_index, row) in cells.iter_mut().enumerate().take(rows) {
            let source_row = row_index.saturating_add(row_offset);
            row[..columns].clone_from_slice(&self.cells[source_row][..columns]);
            renditions[row_index][..columns]
                .copy_from_slice(&self.renditions[source_row][..columns]);
            line_wraps[row_index] = self.line_wraps.get(source_row).copied().unwrap_or(false);
            line_copy_texts[row_index] = self.line_copy_texts.get(source_row).cloned().flatten();
        }

        // Commit content-bearing dropped rows to history so shrink content is preserved.
        if new_rows < old_rows && self.alternate.should_record_to_history() {
            let dropped_rows = if preserve_bottom {
                0..row_offset
            } else {
                new_rows..old_rows
            };
            for row in dropped_rows {
                let copy_text = self.line_copy_texts.get(row).cloned().flatten();
                if copy_text.is_some()
                    || self.cells[row]
                        .iter()
                        .any(|cell| cell.is_written() || !cell.is_blank())
                {
                    self.history.push_styled_line_with_wrap(
                        styled_line_from_row_with_copy_text(
                            &self.cells[row],
                            &self.renditions[row],
                            copy_text,
                        ),
                        self.line_wraps.get(row).copied().unwrap_or(false),
                    );
                }
            }
        }

        self.size = size;
        self.cells = cells;
        self.renditions = renditions;
        self.line_wraps = line_wraps;
        self.line_copy_texts = line_copy_texts;
        let max_row = self.max_row();
        let max_column = self.max_column();
        self.cursor.row = self.cursor.row.saturating_sub(row_offset).min(max_row);
        self.cursor.column = self.cursor.column.min(max_column);
        self.wrap_pending =
            self.wrap_pending && self.autowrap_enabled && self.cursor.column == max_column;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.saturating_sub(row_offset).min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }
    /// Resizes a normal screen when only the row count changes.
    ///
    /// With a stable column count the physical wrap boundaries do not change.
    /// Pane growth must keep the currently rendered viewport stationary, while
    /// pane shrink may bottom-anchor only when the visible tail would otherwise
    /// be truncated.
    pub(super) fn resize_normal_screen_rows_only(&mut self, size: Size) {
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        if new_rows > old_rows {
            self.resize_grid_preserving_cells(size);
            return;
        }
        let live_bottom = self
            .last_significant_row()
            .map(|row| row.max(self.cursor.row))
            .unwrap_or(self.cursor.row);
        if new_rows < old_rows && live_bottom < new_rows {
            self.resize_grid_preserving_cells(size);
            return;
        }
        let preserve_bottom = new_rows < old_rows && live_bottom >= new_rows;
        if new_rows == old_rows {
            self.size = size;
            return;
        }

        let mut visible_rows = self.current_visible_rows();
        let visible_len = visible_rows.len();
        let visible_start = if preserve_bottom || visible_len > new_rows {
            visible_len.saturating_sub(new_rows)
        } else {
            0
        };
        let moved_to_history = visible_start;
        let retained_visible = visible_rows.len().saturating_sub(visible_start);
        let pulled_from_history = new_rows.saturating_sub(retained_visible);
        let history_append_rows = visible_rows.drain(..moved_to_history).collect::<Vec<_>>();

        let mut next_visible_rows = Vec::with_capacity(new_rows);
        let mut restored_history_rows = Vec::with_capacity(pulled_from_history);
        for _ in 0..pulled_from_history {
            let Some((line, wraps_to_next)) = self.history.pop_styled_line() else {
                break;
            };
            restored_history_rows.push(PhysicalStyledLine {
                line,
                wraps_to_next,
            });
        }
        restored_history_rows.reverse();
        let restored_history_row_count = restored_history_rows.len();
        next_visible_rows.extend(restored_history_rows);
        next_visible_rows.extend(visible_rows);

        self.size = size;
        for row in &history_append_rows {
            self.history
                .push_styled_line_with_wrap(row.line.clone(), row.wraps_to_next);
        }
        self.cells = blank_cells(size);
        self.renditions = blank_renditions(size, GraphicRendition::default());
        self.line_wraps = vec![false; new_rows];
        self.line_copy_texts = vec![None; new_rows];
        for (row_index, row) in next_visible_rows.iter().take(new_rows).enumerate() {
            write_styled_line_to_row(
                &row.line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
            );
            self.line_wraps[row_index] = row.wraps_to_next;
            self.line_copy_texts[row_index] = row.line.copy_text.clone();
        }

        let max_row = self.max_row();
        let max_column = self.max_column();
        self.cursor.row = self
            .cursor
            .row
            .saturating_add(restored_history_row_count)
            .saturating_sub(moved_to_history)
            .min(max_row);
        self.cursor.column = self.cursor.column.min(max_column);
        self.wrap_pending =
            self.wrap_pending && self.autowrap_enabled && self.cursor.column == max_column;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor
                .row
                .saturating_add(restored_history_row_count)
                .saturating_sub(moved_to_history)
                .min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }

    /// Reflows live normal-screen rows after a width-changing resize.
    ///
    /// Resize latency must not scale with the configured scrollback limit, and
    /// resizing must not pull retained scrollback into the live viewport. Only
    /// rows that were visible before the resize participate in synchronous
    /// reflow; older history remains stored in its existing physical row form.
    pub(super) fn resize_normal_screen_reflowing(&mut self, size: Size) {
        let old_rows = self.cells.len();
        let new_rows = usize::from(size.rows);
        let preserve_bottom = new_rows < old_rows
            && (self.cursor.row >= new_rows || self.last_significant_row() >= Some(new_rows));
        let preserve_delayed_wrap = self.wrap_pending
            && self
                .leading_column_for_cell(self.cursor.row, self.cursor.column)
                .is_some_and(|column| {
                    self.cells[self.cursor.row][column].width() <= usize::from(size.columns)
                });
        let source_rows = self.current_visible_rows();
        let cursor = cursor_logical_position(&source_rows, self.cursor.row, self.cursor.column);
        let logical_lines =
            merge_wrapped_physical_lines(&source_rows, self.wrap_continuation_prefix.as_deref());
        let physical_rows = reflow_logical_lines(
            &logical_lines,
            usize::from(size.columns),
            self.wrap_continuation_prefix.as_deref(),
        );
        let visible_start = if preserve_bottom || physical_rows.len() > new_rows {
            physical_rows.len().saturating_sub(new_rows)
        } else {
            0
        };

        self.size = size;
        for row in physical_rows.iter().take(visible_start) {
            self.history
                .push_styled_line_with_wrap(row.line.clone(), row.wraps_to_next);
        }
        self.cells = blank_cells(size);
        self.renditions = blank_renditions(size, GraphicRendition::default());
        self.line_wraps = vec![false; new_rows];
        self.line_copy_texts = vec![None; new_rows];
        for (row_index, row) in physical_rows
            .iter()
            .skip(visible_start)
            .take(new_rows)
            .enumerate()
        {
            write_styled_line_to_row(
                &row.line,
                &mut self.cells[row_index],
                &mut self.renditions[row_index],
            );
            self.line_wraps[row_index] = row.wraps_to_next;
            self.line_copy_texts[row_index] = row.line.copy_text.clone();
        }

        let max_row = self.max_row();
        let max_column = self.max_column();
        if let Some((logical_line, logical_column)) = cursor {
            let (absolute_row, column) = physical_position_for_logical_cursor(
                &logical_lines,
                logical_line,
                logical_column.saturating_add(usize::from(preserve_delayed_wrap)),
                usize::from(size.columns),
                self.wrap_continuation_prefix.as_deref(),
            );
            self.cursor.row = absolute_row.saturating_sub(visible_start).min(max_row);
            self.cursor.column = column.min(max_column);
        } else {
            self.cursor.row = self.cursor.row.min(max_row);
            self.cursor.column = self.cursor.column.min(max_column);
        }
        self.wrap_pending =
            preserve_delayed_wrap && self.autowrap_enabled && self.cursor.column == max_column;
        if let Some(cursor) = self.saved_cursor.as_mut() {
            cursor.row = cursor.row.min(max_row);
            cursor.column = cursor.column.min(max_column);
        }
    }
}
