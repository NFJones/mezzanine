//! Projection of the live terminal grid into physical styled rows.
//!
//! This module interprets screen cells and rendition grids together. It owns
//! row-level visibility and style-span projection, but does not mutate parser,
//! cursor, or terminal mode state.

use super::*;

impl TerminalScreen {
    /// Returns the currently visible physical rows with their style metadata.
    pub(super) fn current_visible_rows(&self) -> Vec<PhysicalStyledLine> {
        let last_visible_row = self
            .last_significant_row()
            .map(|row| row.max(self.cursor.row))
            .unwrap_or(self.cursor.row)
            .min(self.cells.len().saturating_sub(1));
        (0..=last_visible_row)
            .map(|row| PhysicalStyledLine {
                line: styled_line_from_row_with_copy_text(
                    &self.cells[row],
                    &self.renditions[row],
                    self.line_copy_texts.get(row).cloned().flatten(),
                ),
                wraps_to_next: self.line_wraps.get(row).copied().unwrap_or(false),
            })
            .collect()
    }

    /// Returns normal-screen physical rows with addresses for copy metadata.
    pub(super) fn normal_physical_line_targets(&self) -> Vec<NormalPhysicalLineTarget> {
        let mut targets = self
            .history
            .styled_lines_with_wraps()
            .enumerate()
            .map(|(index, (line, wraps_to_next))| NormalPhysicalLineTarget {
                index: NormalPhysicalLineIndex::History(index),
                text: line.text,
                wraps_to_next,
            })
            .collect::<Vec<_>>();
        if !self.alternate.active() {
            targets.extend(self.visible_styled_lines().into_iter().enumerate().map(
                |(index, line)| NormalPhysicalLineTarget {
                    index: NormalPhysicalLineIndex::Visible(index),
                    text: line.text,
                    wraps_to_next: self.line_wraps.get(index).copied().unwrap_or(false),
                },
            ));
        }
        targets
    }

    /// Updates the raw-copy text associated with one normal physical row.
    pub(super) fn assign_normal_physical_copy_text(
        &mut self,
        index: NormalPhysicalLineIndex,
        copy_text: Option<String>,
    ) {
        match index {
            NormalPhysicalLineIndex::History(row) => {
                self.history.set_copy_text(row, copy_text);
            }
            NormalPhysicalLineIndex::Visible(row) => {
                if let Some(slot) = self.line_copy_texts.get_mut(row) {
                    *slot = copy_text;
                }
            }
        }
    }

    /// Clears raw-copy metadata for a visible row after terminal mutation.
    pub(super) fn clear_line_copy_text(&mut self, row: usize) {
        if let Some(copy_text) = self.line_copy_texts.get_mut(row) {
            *copy_text = None;
        }
    }

    /// Returns the leading cell column for the grapheme occupying `column`.
    pub(super) fn leading_column_for_cell(&self, row: usize, column: usize) -> Option<usize> {
        let row_cells = self.cells.get(row)?;
        if row_cells.is_empty() {
            return None;
        }
        let mut leading_column = column.min(row_cells.len().saturating_sub(1));
        while leading_column > 0
            && row_cells
                .get(leading_column)
                .is_some_and(|cell| cell.continuation)
        {
            leading_column = leading_column.saturating_sub(1);
        }
        Some(leading_column)
    }

    /// Clears the complete grapheme footprint touching one display column.
    pub(super) fn clear_cell_footprint(
        &mut self,
        row: usize,
        column: usize,
        rendition: GraphicRendition,
    ) {
        let Some(leading_column) = self.leading_column_for_cell(row, column) else {
            return;
        };
        let width = self.cells[row][leading_column].width().max(1);
        let end = leading_column
            .saturating_add(width)
            .min(self.cells[row].len());
        for clear_column in leading_column..end {
            self.cells[row][clear_column] = TerminalScreenCell::blank();
            self.renditions[row][clear_column] = rendition;
        }
    }

    /// Repairs continuation sentinels after column insertion or deletion.
    pub(super) fn repair_row_continuations(&mut self, row: usize) {
        let Some(row_cells) = self.cells.get(row) else {
            return;
        };
        let columns = row_cells.len();
        let mut column = 0usize;
        while column < columns {
            if self.cells[row][column].continuation {
                self.cells[row][column] = TerminalScreenCell::blank();
                column = column.saturating_add(1);
                continue;
            }
            let width = self.cells[row][column].width();
            if width <= 1 {
                column = column.saturating_add(1);
                continue;
            }
            if column.saturating_add(width) > columns {
                for clear_column in column..columns {
                    self.cells[row][clear_column] = TerminalScreenCell::blank();
                }
                break;
            }
            let rendition = self.renditions[row][column];
            for offset in 1..width {
                self.cells[row][column.saturating_add(offset)] = TerminalScreenCell::continuation();
                self.renditions[row][column.saturating_add(offset)] = rendition;
            }
            column = column.saturating_add(width);
        }
    }

    /// Extends the previous leading cell when a scalar completes its grapheme.
    pub(super) fn try_extend_previous_grapheme(&mut self, ch: char) -> bool {
        let Some(row_cells) = self.cells.get(self.cursor.row) else {
            return false;
        };
        if row_cells.is_empty() {
            return false;
        }
        let start_column = if self.wrap_pending {
            self.cursor.column.min(row_cells.len().saturating_sub(1))
        } else if let Some(column) = self.cursor.column.checked_sub(1) {
            column.min(row_cells.len().saturating_sub(1))
        } else {
            return false;
        };
        let Some(leading_column) = self.leading_column_for_cell(self.cursor.row, start_column)
        else {
            return false;
        };
        if self.cells[self.cursor.row][leading_column].is_blank() {
            return false;
        }
        let mut candidate = self.cells[self.cursor.row][leading_column].text.clone();
        candidate.push(ch);
        let mut graphemes = terminal_graphemes(&candidate);
        if graphemes.next() != Some(candidate.as_str()) || graphemes.next().is_some() {
            return false;
        }
        let old_width = self.cells[self.cursor.row][leading_column].width().max(1);
        let new_width = terminal_grapheme_width(&candidate);
        if new_width == 0 || leading_column.saturating_add(new_width) > row_cells.len() {
            return false;
        }

        self.clear_line_copy_text(self.cursor.row);
        let rendition = self.renditions[self.cursor.row][leading_column];
        for column in
            leading_column.saturating_add(old_width)..leading_column.saturating_add(new_width)
        {
            self.clear_cell_footprint(self.cursor.row, column, rendition);
        }
        self.cells[self.cursor.row][leading_column] = TerminalScreenCell::text(&candidate);
        for offset in 1..new_width {
            self.cells[self.cursor.row][leading_column.saturating_add(offset)] =
                TerminalScreenCell::continuation();
            self.renditions[self.cursor.row][leading_column.saturating_add(offset)] = rendition;
        }
        for offset in new_width..old_width {
            let column = leading_column.saturating_add(offset);
            if column < self.cells[self.cursor.row].len() {
                self.cells[self.cursor.row][column] = TerminalScreenCell::blank();
                self.renditions[self.cursor.row][column] = rendition;
            }
        }
        let next_column = leading_column.saturating_add(new_width);
        if next_column > self.max_column() {
            self.cursor.column = leading_column;
            self.wrap_pending = true;
        } else {
            self.cursor.column = next_column;
            self.wrap_pending = false;
        }
        true
    }

    /// Runs the last significant row operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn last_significant_row(&self) -> Option<usize> {
        self.cells
            .iter()
            .zip(self.renditions.iter())
            .rposition(|(cells, renditions)| {
                cells
                    .iter()
                    .zip(renditions.iter())
                    .any(|(cell, rendition)| {
                        cell.is_written()
                            || !cell.is_blank()
                            || *rendition != GraphicRendition::default()
                    })
            })
    }
}
