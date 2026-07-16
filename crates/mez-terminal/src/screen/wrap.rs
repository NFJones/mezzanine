//! Continuation-prefix policy for wrapped logical terminal lines.
//!
//! This module identifies and reapplies configured prefixes while preserving
//! terminal display widths and style cells. General wrapping and grid editing
//! remain owned by the editing module.

use super::*;

impl TerminalScreen {
    /// Returns the configured styled prefix for the current wrapped logical
    /// line when the first physical row matches the configured policy.
    pub(super) fn current_wrap_continuation_prefix(&self) -> Option<Vec<StyledPrefixCell>> {
        let prefix = self.wrap_continuation_prefix.as_deref()?;
        if usize::from(self.size.columns) <= terminal_text_width(prefix) {
            return None;
        }
        let mut row = self.cursor.row.min(self.cells.len().saturating_sub(1));
        while row > 0 && self.line_wraps.get(row.saturating_sub(1)).copied() == Some(true) {
            row = row.saturating_sub(1);
        }
        self.wrap_continuation_prefix_from_row(row, prefix)
    }

    /// Reads the configured styled continuation prefix from one visible row.
    pub(super) fn wrap_continuation_prefix_from_row(
        &self,
        row: usize,
        configured_prefix: &str,
    ) -> Option<Vec<StyledPrefixCell>> {
        let cells = self.cells.get(row)?;
        let renditions = self.renditions.get(row)?;
        let mut column = 0usize;
        let mut prefix = Vec::new();
        for expected in configured_prefix.chars() {
            let width = terminal_char_width(expected);
            let cell = cells.get(column)?;
            if width == 0 || cell.continuation || cell.text != expected.to_string() {
                return None;
            }
            prefix.push(StyledPrefixCell {
                ch: expected,
                width,
                rendition: renditions.get(column).copied().unwrap_or_default(),
            });
            column = column.saturating_add(width);
        }
        styled_prefix_is_non_default(&prefix).then_some(prefix)
    }

    /// Writes a display-only continuation prefix at the cursor after a soft
    /// wrap without changing the current SGR state for the wrapped content.
    pub(super) fn write_wrap_continuation_prefix(&mut self, prefix: &[StyledPrefixCell]) {
        if prefix_width(prefix) >= usize::from(self.size.columns) {
            return;
        }
        for cell in prefix {
            if self
                .cursor
                .column
                .saturating_add(cell.width)
                .saturating_sub(1)
                > self.max_column()
            {
                return;
            }
            self.clear_line_copy_text(self.cursor.row);
            let text = cell.ch.to_string();
            self.cells[self.cursor.row][self.cursor.column] = TerminalScreenCell::text(&text);
            self.renditions[self.cursor.row][self.cursor.column] = cell.rendition;
            for offset in 1..cell.width {
                let column = self.cursor.column.saturating_add(offset);
                self.cells[self.cursor.row][column] = TerminalScreenCell::continuation();
                self.renditions[self.cursor.row][column] = cell.rendition;
            }
            self.cursor.column = self.cursor.column.saturating_add(cell.width);
        }
    }
}
