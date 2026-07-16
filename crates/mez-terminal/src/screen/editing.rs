//! Cursor-directed terminal grid editing and printable text insertion.
//!
//! This module owns character insertion, erase and delete operations, scrolling,
//! line wrapping, and grapheme-safe cell mutation. Parser dispatch and public
//! content projection remain in sibling modules.

use super::*;

impl TerminalScreen {
    /// Runs the print operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn print(&mut self, ch: char) {
        if self.try_extend_previous_grapheme(ch) {
            return;
        }
        let translated = match self.active_charset() {
            TerminalCharset::Ascii => ch,
            TerminalCharset::DecSpecialGraphics => dec_special_graphics_char(ch).unwrap_or(ch),
        };
        let text = translated.to_string();
        let width = terminal_grapheme_width(&text);
        if width == 0 {
            return;
        }
        if self.wrap_pending {
            self.wrap_to_next_line();
        }
        if self.autowrap_enabled
            && self.cursor.column.saturating_add(width).saturating_sub(1) > self.max_column()
        {
            self.wrap_to_next_line();
        }
        self.clear_line_copy_text(self.cursor.row);
        let row = self.cursor.row;
        let column = self.cursor.column;
        if !self.autowrap_enabled && column.saturating_add(width) > self.cells[row].len() {
            for target_column in column..self.cells[row].len() {
                self.clear_cell_footprint(row, target_column, self.graphic_rendition);
            }
            self.cursor.column = self.max_column();
            self.wrap_pending = false;
            return;
        }
        for target_column in column..column.saturating_add(width).min(self.cells[row].len()) {
            self.clear_cell_footprint(row, target_column, self.graphic_rendition);
        }
        self.cells[row][column] = TerminalScreenCell::text(&text);
        self.renditions[row][column] = self.graphic_rendition;
        for offset in 1..width {
            let column = column.saturating_add(offset);
            if column <= self.max_column() {
                self.cells[row][column] = TerminalScreenCell::continuation();
                self.renditions[row][column] = self.graphic_rendition;
            }
        }
        let next_column = self.cursor.column.saturating_add(width);
        if next_column > self.max_column() {
            if !self.autowrap_enabled {
                self.cursor.column = self.max_column();
                self.wrap_pending = false;
                return;
            }
            self.wrap_pending = true;
        } else {
            self.cursor.column = next_column;
        }
    }

    /// Runs the newline operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn newline(&mut self) {
        self.next_line();
    }

    /// Runs the index operation for this subsystem.
    ///
    /// The function keeps VT-style vertical movement separate from carriage
    /// return so LF and IND can preserve the current column while NEL and the
    /// legacy `newline` helper can still move to the first column.
    pub(super) fn index(&mut self) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row == bottom {
            self.scroll_region_up_from(top, bottom, 1);
        } else {
            self.cursor.row = self.cursor.row.saturating_add(1).min(bottom);
        }
    }

    /// Runs the next-line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn next_line(&mut self) {
        self.wrap_pending = false;
        self.cursor.column = 0;
        self.index();
    }

    /// Runs the wrap to next line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn wrap_to_next_line(&mut self) {
        let continuation_prefix = self.current_wrap_continuation_prefix();
        if let Some(wraps) = self.line_wraps.get_mut(self.cursor.row) {
            *wraps = true;
        }
        self.newline();
        self.wrap_pending = false;
        if let Some(prefix) = continuation_prefix {
            self.write_wrap_continuation_prefix(&prefix);
        }
    }

    /// Runs the reverse index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn reverse_index(&mut self) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row == top {
            self.scroll_region_down_from(top, bottom, 1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
    }

    /// Runs the active scroll region operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn active_scroll_region(&self) -> (usize, usize) {
        self.scroll_region.unwrap_or((0, self.max_row()))
    }

    /// Runs the scroll region up operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_up(&mut self, count: usize) {
        let (top, bottom) = self.active_scroll_region();
        self.scroll_region_up_from(top, bottom, count);
    }

    /// Runs the scroll region down operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_down(&mut self, count: usize) {
        let (top, bottom) = self.active_scroll_region();
        self.scroll_region_down_from(top, bottom, count);
    }

    /// Runs the scroll region up from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_up_from(&mut self, top: usize, bottom: usize, count: usize) {
        if top > bottom || bottom > self.max_row() {
            return;
        }
        let count = count.min(bottom.saturating_sub(top).saturating_add(1));
        for _ in 0..count {
            if top == 0
                && bottom == self.max_row()
                && self.alternate.should_record_scroll_off_to_history()
            {
                self.normal_viewport_detached_from_history = false;
                self.history.push_styled_line_with_wrap(
                    styled_line_from_row_with_copy_text(
                        &self.cells[0],
                        &self.renditions[0],
                        self.line_copy_texts.first().cloned().flatten(),
                    ),
                    self.line_wraps.first().copied().unwrap_or(false),
                );
            }
            self.cells.remove(top);
            self.renditions.remove(top);
            self.line_wraps.remove(top);
            self.line_copy_texts.remove(top);
            self.cells.insert(bottom, blank_row(self.size.columns));
            self.renditions.insert(
                bottom,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(bottom, false);
            self.line_copy_texts.insert(bottom, None);
        }
    }

    /// Runs the scroll region down from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn scroll_region_down_from(&mut self, top: usize, bottom: usize, count: usize) {
        if top > bottom || bottom > self.max_row() {
            return;
        }
        let count = count.min(bottom.saturating_sub(top).saturating_add(1));
        for _ in 0..count {
            self.cells.remove(bottom);
            self.renditions.remove(bottom);
            self.line_wraps.remove(bottom);
            self.line_copy_texts.remove(bottom);
            self.cells.insert(top, blank_row(self.size.columns));
            self.renditions.insert(
                top,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(top, false);
            self.line_copy_texts.insert(top, None);
        }
    }

    /// Runs the insert blank chars operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn insert_blank_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let row = self.cursor.row;
        let column = self.cursor.column;
        let width = usize::from(self.size.columns);
        let count = count.min(width.saturating_sub(column));
        self.clear_line_copy_text(row);
        if count > 0
            && self.cells[row]
                .get(column)
                .is_some_and(|cell| cell.continuation)
        {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
        }
        for _ in 0..count {
            self.cells[row].insert(column, TerminalScreenCell::blank());
            self.renditions[row].insert(column, self.graphic_rendition);
            self.cells[row].truncate(width);
            self.renditions[row].truncate(width);
        }
        self.repair_row_continuations(row);
    }

    /// Runs the delete chars operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn delete_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let row = self.cursor.row;
        let column = self.cursor.column;
        let width = usize::from(self.size.columns);
        let count = count.min(width.saturating_sub(column));
        self.clear_line_copy_text(row);
        if count > 0
            && self.cells[row]
                .get(column)
                .is_some_and(|cell| cell.continuation)
        {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
        }
        for _ in 0..count {
            self.cells[row].remove(column);
            self.renditions[row].remove(column);
            self.cells[row].push(TerminalScreenCell::blank());
            self.renditions[row].push(self.graphic_rendition);
        }
        self.repair_row_continuations(row);
    }

    /// Runs the erase chars operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let row = self.cursor.row;
        let start = self.cursor.column;
        let end = start
            .saturating_add(count.saturating_sub(1))
            .min(self.max_column());
        for column in start..=end {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
            self.cells[row][column] = TerminalScreenCell::blank();
            self.renditions[row][column] = self.graphic_rendition;
        }
        self.clear_line_copy_text(row);
    }

    /// Runs the insert lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn insert_lines(&mut self, count: usize) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }
        let count = count.min(bottom.saturating_sub(self.cursor.row).saturating_add(1));
        for _ in 0..count {
            self.cells
                .insert(self.cursor.row, blank_row(self.size.columns));
            self.renditions.insert(
                self.cursor.row,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(self.cursor.row, false);
            self.line_copy_texts.insert(self.cursor.row, None);
            self.cells.remove(bottom.saturating_add(1));
            self.renditions.remove(bottom.saturating_add(1));
            self.line_wraps.remove(bottom.saturating_add(1));
            self.line_copy_texts.remove(bottom.saturating_add(1));
        }
    }

    /// Runs the delete lines operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn delete_lines(&mut self, count: usize) {
        self.wrap_pending = false;
        let (top, bottom) = self.active_scroll_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }
        let count = count.min(bottom.saturating_sub(self.cursor.row).saturating_add(1));
        for _ in 0..count {
            self.cells.remove(self.cursor.row);
            self.renditions.remove(self.cursor.row);
            self.line_wraps.remove(self.cursor.row);
            self.line_copy_texts.remove(self.cursor.row);
            self.cells.insert(bottom, blank_row(self.size.columns));
            self.renditions.insert(
                bottom,
                blank_rendition_row(self.size.columns, self.graphic_rendition),
            );
            self.line_wraps.insert(bottom, false);
            self.line_copy_texts.insert(bottom, None);
        }
    }

    /// Runs the set scroll region operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_scroll_region(&mut self, params: &str) {
        if params.is_empty() {
            self.scroll_region = None;
            self.cursor = Cursor { row: 0, column: 0 };
            self.wrap_pending = false;
            return;
        }
        let mut parts = params.split(';');
        let top = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let bottom = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or_else(|| usize::from(self.size.rows))
            .saturating_sub(1)
            .min(self.max_row());
        if top < bottom {
            self.scroll_region = Some((top, bottom));
            self.cursor = Cursor {
                row: if self.origin_mode_enabled { top } else { 0 },
                column: 0,
            };
            self.wrap_pending = false;
        }
    }

    /// Runs the move cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor(&mut self, params: &str) {
        self.wrap_pending = false;
        let mut parts = params.split(';');
        let row = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or(1);
        let column = parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<usize>().ok())
            .unwrap_or(1);
        self.cursor.row = if self.origin_mode_enabled {
            let (top, bottom) = self.active_scroll_region();
            top.saturating_add(row.saturating_sub(1)).min(bottom)
        } else {
            row.saturating_sub(1).min(self.max_row())
        };
        self.cursor.column = column.saturating_sub(1).min(self.max_column());
    }

    /// Runs the move cursor column operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_column(&mut self, params: &str) {
        self.wrap_pending = false;
        let column = first_csi_param(params).max(1);
        self.cursor.column = column.saturating_sub(1).min(self.max_column());
    }

    /// Runs the move cursor row operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_row(&mut self, params: &str) {
        self.wrap_pending = false;
        let row = first_csi_param(params).max(1);
        self.cursor.row = if self.origin_mode_enabled {
            let (top, bottom) = self.active_scroll_region();
            top.saturating_add(row.saturating_sub(1)).min(bottom)
        } else {
            row.saturating_sub(1).min(self.max_row())
        };
    }

    /// Runs the move cursor next line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_next_line(&mut self, params: &str) {
        self.move_cursor_relative(params, 1, 0);
        self.cursor.column = 0;
    }

    /// Runs the move cursor previous line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_previous_line(&mut self, params: &str) {
        self.move_cursor_relative(params, -1, 0);
        self.cursor.column = 0;
    }

    /// Runs the move cursor relative operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn move_cursor_relative(
        &mut self,
        params: &str,
        row_direction: isize,
        column_direction: isize,
    ) {
        self.wrap_pending = false;
        let amount = csi_count(params);
        let (min_row, max_row) = if self.origin_mode_enabled {
            self.active_scroll_region()
        } else {
            (0, self.max_row())
        };
        if row_direction < 0 {
            self.cursor.row = self.cursor.row.saturating_sub(amount).max(min_row);
        } else if row_direction > 0 {
            self.cursor.row = self.cursor.row.saturating_add(amount).min(max_row);
        }

        if column_direction < 0 {
            self.cursor.column = self.cursor.column.saturating_sub(amount);
        } else if column_direction > 0 {
            self.cursor.column = self
                .cursor
                .column
                .saturating_add(amount)
                .min(self.max_column());
        }
    }

    /// Runs the clear screen operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn clear_screen(&mut self) {
        self.cells = blank_cells(self.size);
        self.renditions = blank_renditions(self.size, self.graphic_rendition);
        self.line_wraps = vec![false; usize::from(self.size.rows)];
        self.line_copy_texts = vec![None; usize::from(self.size.rows)];
        self.cursor = Cursor { row: 0, column: 0 };
        self.wrap_pending = false;
    }

    /// Runs the erase display operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_display(&mut self, params: &str) {
        self.wrap_pending = false;
        match first_csi_param(params) {
            0 => {
                self.erase_line_range(self.cursor.row, self.cursor.column, self.max_column());
                for row in self.cursor.row.saturating_add(1)..=self.max_row() {
                    self.erase_line_range(row, 0, self.max_column());
                }
            }
            1 => {
                for row in 0..self.cursor.row {
                    self.erase_line_range(row, 0, self.max_column());
                }
                self.erase_line_range(self.cursor.row, 0, self.cursor.column);
            }
            2 => {
                for row in 0..=self.max_row() {
                    self.erase_line_range(row, 0, self.max_column());
                }
                if !self.alternate.active() {
                    self.normal_viewport_detached_from_history = true;
                }
            }
            3 if !self.alternate.active() => {
                self.history.clear();
                self.normal_viewport_detached_from_history = false;
            }
            _ => {}
        }
    }

    /// Runs the erase line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_line(&mut self, params: &str) {
        self.wrap_pending = false;
        match first_csi_param(params) {
            0 => self.erase_line_range(self.cursor.row, self.cursor.column, self.max_column()),
            1 => self.erase_line_range(self.cursor.row, 0, self.cursor.column),
            2 => self.erase_line_range(self.cursor.row, 0, self.max_column()),
            _ => {}
        }
    }

    /// Runs the erase line range operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn erase_line_range(&mut self, row: usize, start_column: usize, end_column: usize) {
        let end_column = end_column.min(self.max_column());
        for column in start_column.min(end_column)..=end_column {
            self.clear_cell_footprint(row, column, self.graphic_rendition);
            self.cells[row][column] = TerminalScreenCell::blank();
            self.renditions[row][column] = self.graphic_rendition;
        }
        self.clear_line_copy_text(row);
        if end_column == self.max_column()
            && let Some(wraps) = self.line_wraps.get_mut(row)
        {
            *wraps = false;
        }
    }

    /// Runs the save cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn save_cursor(&mut self) {
        self.saved_cursor = Some(self.cursor);
    }

    /// Runs the restore cursor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn restore_cursor(&mut self) {
        if let Some(cursor) = self.saved_cursor {
            self.cursor = Cursor {
                row: cursor.row.min(self.max_row()),
                column: cursor.column.min(self.max_column()),
            };
            self.wrap_pending = false;
        }
    }

    /// Runs the apply sgr operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_sgr(&mut self, params: &str) {
        let values = sgr_params(params);
        let mut index = 0;
        while index < values.len() {
            match values[index] {
                0 => self.graphic_rendition = GraphicRendition::default(),
                1 => self.graphic_rendition.bold = true,
                2 => self.graphic_rendition.dim = true,
                3 => self.graphic_rendition.italic = true,
                4 => self.graphic_rendition.underline = true,
                7 => self.graphic_rendition.inverse = true,
                8 => self.graphic_rendition.hidden = true,
                9 => self.graphic_rendition.strikethrough = true,
                21 => {
                    self.graphic_rendition.underline = true;
                    self.graphic_rendition.double_underline = true;
                }
                22 => {
                    self.graphic_rendition.bold = false;
                    self.graphic_rendition.dim = false;
                }
                23 => self.graphic_rendition.italic = false,
                24 => {
                    self.graphic_rendition.underline = false;
                    self.graphic_rendition.double_underline = false;
                }
                27 => self.graphic_rendition.inverse = false,
                28 => self.graphic_rendition.hidden = false,
                29 => self.graphic_rendition.strikethrough = false,
                30..=37 => {
                    self.graphic_rendition.foreground =
                        Some(TerminalColor::Indexed((values[index] - 30) as u8));
                }
                39 => self.graphic_rendition.foreground = None,
                40..=47 => {
                    self.graphic_rendition.background =
                        Some(TerminalColor::Indexed((values[index] - 40) as u8));
                }
                49 => self.graphic_rendition.background = None,
                90..=97 => {
                    self.graphic_rendition.foreground =
                        Some(TerminalColor::Indexed((values[index] - 90 + 8) as u8));
                }
                100..=107 => {
                    self.graphic_rendition.background =
                        Some(TerminalColor::Indexed((values[index] - 100 + 8) as u8));
                }
                38 | 48 => {
                    if let Some((color, consumed)) = parse_extended_sgr_color(&values[index + 1..])
                    {
                        if values[index] == 38 {
                            self.graphic_rendition.foreground = Some(color);
                        } else {
                            self.graphic_rendition.background = Some(color);
                        }
                        index += consumed;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    /// Runs the max row operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn max_row(&self) -> usize {
        usize::from(self.size.rows.saturating_sub(1))
    }

    /// Runs the max column operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn max_column(&self) -> usize {
        usize::from(self.size.columns.saturating_sub(1))
    }
}
