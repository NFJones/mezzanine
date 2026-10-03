//! Terminal-cell table geometry over the parser's single captured table state.
//!
//! Width allocation, stacked fallback, alignment and inline style coordinates
//! are presentation only. Continuation rows retain the canonical source-copy
//! markers; this owner does not parse Markdown or create a second table capture.

use super::*;

/// A display-cell range in the first cell of a caller-supplied table body row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableFirstCellRange {
    /// Zero-based body row, independent of physical wrapping.
    pub row: usize,
    /// Physical line in the returned table layout.
    pub line: usize,
    /// First terminal cell containing the value, excluding labels and padding.
    pub start: usize,
    /// Number of visible value cells.
    pub width: usize,
}

/// Presentation-only table layout with caller-owned row provenance.
pub struct TableLayout {
    /// Rendered rows using the ordinary Markdown table geometry.
    pub lines: Vec<RichTextLine>,
    /// First-cell fragments; these carry no executable targets.
    pub first_cells: Vec<TableFirstCellRange>,
}

/// Lays out literal cells without reparsing their values as Markdown.
/// Callers retain authority separately and bind body indices to their records.
pub fn render_literal_table(
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    width: usize,
    theme: &RichTextTheme,
) -> TableLayout {
    let mut table = MarkdownTableState::new(
        vec![Alignment::Left; headers.len()],
        Some(width.max(1)),
        theme.inline_code,
        theme.structural,
        theme.table_alternate_row,
    );
    table.header_rows = 1;
    table.rows = std::iter::once(headers)
        .chain(rows)
        .map(|row| {
            row.into_iter()
                .map(|text| MarkdownTableCell {
                    text: sanitized_terminal_line(&text)
                        .replace(['\n', '\r'], " ")
                        .trim()
                        .to_string(),
                    style_spans: Vec::new(),
                })
                .collect()
        })
        .collect();
    let mut first_cells = Vec::new();
    let lines = table.render_lines_with_ranges(&mut first_cells);
    TableLayout { lines, first_cells }
}

impl MarkdownTableState {
    /// Renders the captured table as aligned box-drawing terminal rows.
    pub(super) fn render_lines(self) -> Vec<RichTextLine> {
        self.render_lines_with_ranges(&mut Vec::new())
    }

    /// Uses the same geometry while retaining first-cell body provenance.
    fn render_lines_with_ranges(self, ranges: &mut Vec<TableFirstCellRange>) -> Vec<RichTextLine> {
        let column_count = self.column_count();
        if column_count == 0 {
            return Vec::new();
        }
        if self
            .display_width
            .is_some_and(|width| width < column_count.saturating_mul(4).saturating_add(1))
        {
            return self.render_stacked_lines(column_count, ranges);
        }
        let widths = self.column_widths(column_count);
        let mut lines = Vec::new();
        for (row_index, row) in self.rows.iter().enumerate() {
            let wrapped_cells = self.wrap_row_cells(row, &widths);
            let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for physical_row in 0..row_height {
                let rendered = self.render_wrapped_row(&wrapped_cells, &widths, physical_row);
                if row_index >= self.header_rows
                    && let Some(cell) = wrapped_cells
                        .first()
                        .and_then(|cells| cells.get(physical_row))
                {
                    let width = terminal_text_width(&cell.text);
                    if width > 0 {
                        let padding = widths[0].saturating_sub(width);
                        let left = match self.alignment(0) {
                            Alignment::Right => padding,
                            Alignment::Center => padding / 2,
                            _ => 0,
                        };
                        ranges.push(TableFirstCellRange {
                            row: row_index - self.header_rows,
                            line: lines.len(),
                            start: 2 + left,
                            width,
                        });
                    }
                }
                let mut line = RichTextLine {
                    display: rendered.text.clone(),
                    style_spans: Vec::new(),
                    copy_text: Some(if physical_row == 0 {
                        rendered.text.clone()
                    } else {
                        COPY_SKIP_LINE.to_string()
                    }),
                    kind: if physical_row == 0 {
                        RichTextLineKind::MarkdownTableRow
                    } else {
                        RichTextLineKind::MarkdownTableContinuation
                    },
                };
                self.apply_row_style(&mut line, row_index);
                for span in rendered.style_spans {
                    push_or_extend_style_span(&mut line.style_spans, span);
                }
                lines.push(line);
            }
            if row_index + 1 == self.header_rows {
                lines.push(RichTextLine {
                    display: self.render_separator(&widths),
                    style_spans: vec![TerminalStyleSpan {
                        start: 0,
                        length: terminal_text_width(self.render_separator(&widths).as_str()),
                        rendition: GraphicRendition {
                            foreground: Some(self.border_foreground),
                            background: None,
                            dim: true,
                            ..GraphicRendition::default()
                        },
                    }],
                    copy_text: None,
                    kind: RichTextLineKind::MarkdownTableSeparator,
                });
            }
        }
        lines
    }

    /// Renders structurally overwide tables as width-bounded header/value rows.
    ///
    /// Box-drawing tables require at least one content cell plus borders and
    /// padding for every column. Below that structural width, preserving the
    /// normal table would lose right-edge columns to terminal clipping.
    fn render_stacked_lines(
        &self,
        column_count: usize,
        ranges: &mut Vec<TableFirstCellRange>,
    ) -> Vec<RichTextLine> {
        let width = self.display_width.unwrap_or(1).max(1);
        let headers = self.rows.first().cloned().unwrap_or_default();
        let body_start = self.header_rows.min(self.rows.len());
        let rows = if body_start == 0 {
            self.rows.as_slice()
        } else {
            &self.rows[body_start..]
        };
        let mut lines = Vec::new();
        for (body_index, row) in rows.iter().enumerate() {
            for column in 0..column_count {
                let header = headers
                    .get(column)
                    .filter(|header| !header.text.is_empty())
                    .cloned()
                    .unwrap_or_else(|| MarkdownTableCell {
                        text: format!("Column {}", column.saturating_add(1)),
                        style_spans: Vec::new(),
                    });
                let value = row.get(column).cloned().unwrap_or_default();
                let value_start = terminal_text_width(&header.text).saturating_add(2);
                let value_end = value_start.saturating_add(terminal_text_width(&value.text));
                let combined = Self::stacked_cell(header, value);
                let fragments = Self::wrap_cell_with_ranges(&combined, width);
                for (fragment_index, (fragment, source_start)) in fragments.into_iter().enumerate()
                {
                    let start = value_start.max(source_start);
                    let end = value_end
                        .min(source_start.saturating_add(terminal_text_width(&fragment.text)));
                    if column == 0 && start < end {
                        ranges.push(TableFirstCellRange {
                            row: body_index,
                            line: lines.len(),
                            start: start - source_start,
                            width: end - start,
                        });
                    }
                    let mut line = RichTextLine {
                        copy_text: Some(if fragment_index == 0 {
                            fragment.text.clone()
                        } else {
                            COPY_SKIP_LINE.to_string()
                        }),
                        display: fragment.text,
                        style_spans: Vec::new(),
                        kind: if fragment_index == 0 {
                            RichTextLineKind::MarkdownTableRow
                        } else {
                            RichTextLineKind::MarkdownTableContinuation
                        },
                    };
                    self.apply_row_style(&mut line, body_index.saturating_add(self.header_rows));
                    for span in fragment.style_spans {
                        push_or_extend_style_span(&mut line.style_spans, span);
                    }
                    lines.push(line);
                }
            }
            if body_index.saturating_add(1) < rows.len() {
                lines.push(markdown_blank_line());
            }
        }
        lines
    }

    /// Returns the number of table columns.
    fn column_count(&self) -> usize {
        self.alignments
            .len()
            .max(self.rows.iter().map(Vec::len).max().unwrap_or_default())
    }

    /// Computes display widths for each column.
    fn column_widths(&self, column_count: usize) -> Vec<usize> {
        let natural_widths = (0..column_count)
            .map(|column| {
                self.rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| terminal_text_width(cell.text.as_str()))
                    .max()
                    .unwrap_or(0)
                    .max(3)
            })
            .collect::<Vec<_>>();
        let Some(display_width) = self.display_width else {
            return natural_widths;
        };
        if Self::table_total_width(&natural_widths) <= display_width {
            return natural_widths;
        }
        Self::bounded_column_widths(&natural_widths, display_width)
    }

    /// Returns total display width for a rendered table with these content widths.
    fn table_total_width(widths: &[usize]) -> usize {
        widths
            .iter()
            .sum::<usize>()
            .saturating_add(widths.len().saturating_mul(3))
            .saturating_add(1)
    }

    /// Allocates bounded content widths after box and padding overhead.
    fn bounded_column_widths(natural_widths: &[usize], display_width: usize) -> Vec<usize> {
        let column_count = natural_widths.len();
        if column_count == 0 {
            return Vec::new();
        }
        let structural_width = column_count.saturating_mul(3).saturating_add(1);
        let available = display_width
            .saturating_sub(structural_width)
            .max(column_count);
        let minimum_width: usize = if available >= column_count.saturating_mul(3) {
            3
        } else {
            1
        };
        let mut widths = vec![minimum_width; column_count];
        let mut remaining = available.saturating_sub(minimum_width.saturating_mul(column_count));
        while remaining > 0 {
            let mut advanced = false;
            for (width, natural_width) in widths.iter_mut().zip(natural_widths.iter()) {
                if remaining == 0 {
                    break;
                }
                if *width < *natural_width {
                    *width = width.saturating_add(1);
                    remaining = remaining.saturating_sub(1);
                    advanced = true;
                }
            }
            if !advanced {
                break;
            }
        }
        widths
    }

    /// Wraps every cell in one markdown source row to its allocated content width.
    fn wrap_row_cells(
        &self,
        row: &[MarkdownTableCell],
        widths: &[usize],
    ) -> Vec<Vec<MarkdownTableCell>> {
        widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let cell = row.get(column).cloned().unwrap_or_default();
                Self::wrap_cell(&cell, *width)
            })
            .collect()
    }

    /// Wraps one cell into physical table-row fragments.
    fn wrap_cell(cell: &MarkdownTableCell, width: usize) -> Vec<MarkdownTableCell> {
        Self::wrap_cell_with_ranges(cell, width)
            .into_iter()
            .map(|(cell, _)| cell)
            .collect()
    }

    /// Retains original display-cell offsets through hard and word wrapping.
    fn wrap_cell_with_ranges(
        cell: &MarkdownTableCell,
        width: usize,
    ) -> Vec<(MarkdownTableCell, usize)> {
        let width = width.max(1);
        let mut remaining = cell.text.as_str();
        let mut source_start = 0usize;
        if remaining.is_empty() {
            return vec![(MarkdownTableCell::default(), 0)];
        }
        let mut lines = Vec::new();
        while !remaining.is_empty() {
            let (segment, consumed) = Self::take_cell_segment(remaining, width);
            let segment_width = terminal_text_width(segment.as_str());
            lines.push((
                MarkdownTableCell {
                    text: segment,
                    style_spans: style_spans_for_rich_text_segment(
                        &cell.style_spans,
                        source_start,
                        source_start.saturating_add(segment_width),
                        0,
                    ),
                },
                source_start,
            ));
            source_start = source_start.saturating_add(terminal_text_width(&remaining[..consumed]));
            remaining = &remaining[consumed..];
            let trimmed = remaining.trim_start();
            let trimmed_bytes = remaining.len().saturating_sub(trimmed.len());
            source_start =
                source_start.saturating_add(terminal_text_width(&remaining[..trimmed_bytes]));
            remaining = trimmed;
        }
        lines
    }

    /// Joins one header and value while retaining both cells' inline styles.
    fn stacked_cell(header: MarkdownTableCell, value: MarkdownTableCell) -> MarkdownTableCell {
        let value_start = terminal_text_width(header.text.as_str()).saturating_add(2);
        let mut style_spans = header.style_spans;
        style_spans.extend(value.style_spans.into_iter().map(|mut span| {
            span.start = span.start.saturating_add(value_start);
            span
        }));
        MarkdownTableCell {
            text: format!("{}: {}", header.text, value.text),
            style_spans,
        }
    }

    /// Takes one table-cell segment, hard-splitting only when needed for layout.
    fn take_cell_segment(text: &str, width: usize) -> (String, usize) {
        if terminal_text_width(text) <= width {
            return (text.to_string(), text.len());
        }
        if let Some((_, grapheme)) = UnicodeSegmentation::grapheme_indices(text, true).next()
            && terminal_grapheme_width(grapheme) > width
        {
            return ("…".to_string(), grapheme.len());
        }
        let mut used_width = 0usize;
        let mut boundary_consumed = 0usize;
        let mut last_space_break: Option<(usize, usize)> = None;
        for (index, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
            let grapheme_width = terminal_grapheme_width(grapheme);
            if used_width > 0 && used_width.saturating_add(grapheme_width) > width {
                break;
            }
            let next_consumed = index.saturating_add(grapheme.len());
            if grapheme.chars().all(char::is_whitespace) && used_width > 0 {
                last_space_break = Some((index, next_consumed));
            }
            boundary_consumed = next_consumed;
            used_width = used_width.saturating_add(grapheme_width);
            if used_width >= width {
                break;
            }
        }
        if let Some((space_start, consumed_through_space)) = last_space_break {
            let segment = text[..space_start].trim_end().to_string();
            if !segment.is_empty() {
                return (segment, consumed_through_space);
            }
        }
        (text[..boundary_consumed].to_string(), boundary_consumed)
    }

    /// Renders one physical table row from already wrapped cells.
    fn render_wrapped_row(
        &self,
        cells: &[Vec<MarkdownTableCell>],
        widths: &[usize],
        row_index: usize,
    ) -> MarkdownTableCell {
        let mut rendered = MarkdownTableCell {
            text: "│".to_string(),
            style_spans: Vec::new(),
        };
        for (column, width) in widths.iter().enumerate() {
            let cell = cells
                .get(column)
                .and_then(|lines| lines.get(row_index))
                .cloned()
                .unwrap_or_default();
            let cell = self.render_cell(cell, *width, self.alignment(column));
            let offset = terminal_text_width(rendered.text.as_str());
            rendered
                .style_spans
                .extend(cell.style_spans.into_iter().map(|mut span| {
                    span.start = span.start.saturating_add(offset);
                    span
                }));
            rendered.text.push_str(&cell.text);
            rendered.text.push('│');
        }
        rendered
    }

    /// Applies header or alternating-row table styling to one physical row.
    fn apply_row_style(&self, line: &mut RichTextLine, row_index: usize) {
        let length = terminal_text_width(line.display.as_str());
        if length == 0 {
            return;
        }
        let rendition = if row_index < self.header_rows {
            GraphicRendition {
                foreground: Some(self.header_foreground),
                background: None,
                bold: true,
                ..GraphicRendition::default()
            }
        } else if row_index.saturating_sub(self.header_rows).is_multiple_of(2) {
            GraphicRendition {
                foreground: Some(self.alternate_row_foreground),
                background: None,
                ..GraphicRendition::default()
            }
        } else {
            return;
        };
        line.style_spans.push(TerminalStyleSpan {
            start: 0,
            length,
            rendition,
        });
        self.apply_border_style(line);
    }

    /// Applies subdued foreground styling to visible box-drawing table borders.
    fn apply_border_style(&self, line: &mut RichTextLine) {
        for (start, grapheme) in UnicodeSegmentation::grapheme_indices(line.display.as_str(), true)
        {
            if matches!(grapheme, "│" | "├" | "┤" | "┼" | "─") {
                push_or_extend_style_span(
                    &mut line.style_spans,
                    TerminalStyleSpan {
                        start: terminal_text_width(&line.display[..start]),
                        length: terminal_grapheme_width(grapheme),
                        rendition: GraphicRendition {
                            foreground: Some(self.border_foreground),
                            background: None,
                            dim: true,
                            ..GraphicRendition::default()
                        },
                    },
                );
            }
        }
    }

    /// Renders one box-drawing table separator row.
    fn render_separator(&self, widths: &[usize]) -> String {
        let cells = widths
            .iter()
            .map(|width| "─".repeat(width.saturating_add(2)))
            .collect::<Vec<_>>();
        format!("├{}┤", cells.join("┼"))
    }

    /// Renders one padded table cell.
    fn render_cell(
        &self,
        mut cell: MarkdownTableCell,
        width: usize,
        alignment: Alignment,
    ) -> MarkdownTableCell {
        let cell_width = terminal_text_width(cell.text.as_str());
        let padding = width.saturating_sub(cell_width);
        let (left, right) = match alignment {
            Alignment::Right => (padding, 0),
            Alignment::Center => (padding / 2, padding.saturating_sub(padding / 2)),
            Alignment::None | Alignment::Left => (0, padding),
        };
        let content_start = left.saturating_add(1);
        for span in &mut cell.style_spans {
            span.start = span.start.saturating_add(content_start);
        }
        cell.text = format!(" {}{}{} ", " ".repeat(left), cell.text, " ".repeat(right));
        cell
    }

    /// Returns the alignment for a column.
    fn alignment(&self, column: usize) -> Alignment {
        self.alignments
            .get(column)
            .copied()
            .unwrap_or(Alignment::None)
    }
}
