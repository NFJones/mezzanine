//! Pane-agent selector and record-browser layout projection.

use super::display_content::RuntimeCommandDisplayOverlayContent;
use super::product_content::*;
use crate::runtime::render::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn runtime_pane_agent_status_selector_layout(
    selector: &RuntimePaneAgentStatusSelector,
    size: Size,
) -> PaneAgentStatusSelectorLayout {
    let item_width = selector
        .items
        .iter()
        .map(|item| UnicodeWidthStr::width(item.as_str()))
        .max()
        .unwrap_or(0)
        .saturating_add(4);
    let width = usize::from(selector.anchor_width)
        .max(item_width)
        .max(8)
        .min(usize::from(size.columns).max(1));
    let width_u16 = u16::try_from(width).unwrap_or(size.columns.max(1));
    let max_column = size.columns.saturating_sub(width_u16);
    let column = selector.anchor_column.min(max_column);
    let pane_relative_limit = usize::from(size.rows)
        .saturating_mul(3)
        .saturating_div(4)
        .max(1);
    let visible_count = selector
        .items
        .len()
        .min(PANE_AGENT_STATUS_SELECTOR_MAX_ROWS)
        .min(pane_relative_limit)
        .min(usize::from(size.rows).saturating_sub(1).max(1));
    let rows_below = size
        .rows
        .saturating_sub(selector.anchor_row.saturating_add(1));
    let start_row = if rows_below >= u16::try_from(visible_count).unwrap_or(u16::MAX) {
        selector.anchor_row.saturating_add(1)
    } else {
        selector
            .anchor_row
            .saturating_sub(u16::try_from(visible_count).unwrap_or(u16::MAX))
    };
    let max_first_index = selector.items.len().saturating_sub(visible_count);
    let first_index = selector.scroll_offset.min(max_first_index);
    let visible_items = (0..visible_count)
        .filter_map(|offset| {
            Some(PaneAgentStatusSelectorLayoutItem {
                item_index: first_index.saturating_add(offset),
                row: start_row.checked_add(u16::try_from(offset).ok()?)?,
            })
        })
        .collect();
    PaneAgentStatusSelectorLayout {
        column,
        width: width_u16,
        visible_items,
    }
}

/// Builds one padded selector row clipped to the available terminal width.
pub(crate) fn runtime_selector_line(marker: &str, value: &str, width: usize) -> String {
    let mut line = format!("{marker} {value}");
    let mut fitted = String::new();
    let mut used = 0usize;
    for ch in line.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if used.saturating_add(ch_width) > width {
            break;
        }
        fitted.push(ch);
        used = used.saturating_add(ch_width);
    }
    line = fitted;
    while UnicodeWidthStr::width(line.as_str()) < width {
        line.push(' ');
    }
    line
}

pub(super) fn record_browser_prompt_text(
    prompt: &mez_mux::record_browser::RecordBrowserPrompt,
) -> String {
    match prompt {
        mez_mux::record_browser::RecordBrowserPrompt::Filter { input, .. }
        | mez_mux::record_browser::RecordBrowserPrompt::Save { input } => input.clone(),
        mez_mux::record_browser::RecordBrowserPrompt::KindSelector { .. } => String::new(),
    }
}

pub(super) fn render_record_browser_overlay(
    overlay: &mut RuntimeDisplayOverlay,
    ui_theme: &mez_mux::theme::UiTheme,
    terminal_width: usize,
    prose_width: usize,
) -> bool {
    render_record_browser_overlay_matching(overlay, ui_theme, terminal_width, prose_width, None)
}

/// Rebuilds a record-browser overlay, optionally restricting its list rows to
/// an in-page pager search while retaining the generic match highlight state.
pub(super) fn render_record_browser_overlay_matching(
    overlay: &mut RuntimeDisplayOverlay,
    ui_theme: &mez_mux::theme::UiTheme,
    terminal_width: usize,
    prose_width: usize,
    search_query: Option<&str>,
) -> bool {
    let Some(record_browser) = overlay.record_browser.as_ref() else {
        return false;
    };
    let page = record_browser
        .browser
        .render_page_matching(search_query.unwrap_or_default());
    let prompt_selection = record_browser.browser.prompt_selection();
    let list_active_index =
        (!record_browser.browser.is_detail_view()).then(|| record_browser.browser.active_index());
    let content_width = if record_browser.browser.is_detail_view() {
        prose_width
    } else {
        terminal_width
    };
    let mut content = runtime_agent_shell_markdown_overlay_content_for_layout(
        Some(record_browser.command.clone()),
        &page.markdown,
        ui_theme,
        terminal_width,
        content_width,
    );
    if let Some(prompt_selection) = prompt_selection {
        content.selections = content
            .lines
            .iter()
            .enumerate()
            .skip(prompt_selection.start_line)
            .take(prompt_selection.option_count)
            .map(|(line_index, line)| OverlaySelection {
                logical_id: line_index,
                line_index,
                start_column: 0,
                width: UnicodeWidthStr::width(line.as_str()),
                command: String::new(),
                kind: OverlaySelectionKind::Primary,
            })
            .collect();
    } else {
        restore_record_browser_table_link_selections(&mut content, &record_browser.browser);
    }
    let content = content;
    overlay.lines = content.lines;
    overlay.line_style_spans = content.line_style_spans;
    overlay.line_copy_texts = content.line_copy_texts;
    overlay.selections = content.selections;
    overlay.active_selection_index = if overlay.selections.is_empty() {
        None
    } else {
        prompt_selection
            .map(|selection| {
                selection
                    .active_index
                    .min(overlay.selections.len().saturating_sub(1))
            })
            .or_else(|| {
                list_active_index.and_then(|logical_id| {
                    overlay
                        .selections
                        .iter()
                        .position(|selection| selection.logical_id == logical_id)
                })
            })
            .or(Some(0))
    };
    if record_browser.browser.prompt().is_some() {
        overlay.scroll_offset = 0;
    }
    overlay.search_input = None;
    overlay.search_query = None;
    overlay.search_match = None;
    overlay.search_status = None;
    if let Some(query) = search_query.filter(|query| !query.is_empty()) {
        overlay.search_query = Some(query.to_string());
        overlay.search_match = mez_mux::overlay::overlay_next_search_match(overlay, query, 0);
        overlay.search_status = overlay
            .search_match
            .is_none()
            .then(|| format!("pattern not found: {query}"));
        if let Some(search_match) = overlay.search_match {
            overlay.scroll_offset = search_match.line_index;
        }
    }
    true
}

/// Restores logical record links when a narrow Markdown table splits an ID
/// across physical continuation rows that no longer retain a complete link
/// label on any one rendered line.
fn restore_record_browser_table_link_selections(
    content: &mut RuntimeCommandDisplayOverlayContent,
    browser: &mez_mux::record_browser::RecordBrowser,
) {
    for (logical_id, record) in browser.records().iter().enumerate() {
        let Some(command) = record.open_command.as_deref() else {
            continue;
        };
        for selection in content
            .selections
            .iter_mut()
            .filter(|selection| selection.command == command)
        {
            selection.logical_id = logical_id;
        }
        let mut matched_id = String::new();
        let mut fragments = Vec::new();
        for (line_index, line) in content.lines.iter().enumerate() {
            let Some((start_column, visible)) = markdown_table_first_cell_visible_text(line) else {
                continue;
            };
            let candidate = format!("{matched_id}{visible}");
            if record.id.starts_with(&candidate) {
                matched_id = candidate;
                fragments.push((line_index, start_column, UnicodeWidthStr::width(visible)));
            } else if record.id.starts_with(visible) {
                matched_id = visible.to_string();
                fragments.clear();
                fragments.push((line_index, start_column, UnicodeWidthStr::width(visible)));
            } else {
                matched_id.clear();
                fragments.clear();
            }
            if matched_id == record.id {
                for (line_index, start_column, width) in fragments.drain(..) {
                    if content.selections.iter().any(|selection| {
                        selection.line_index == line_index
                            && selection.start_column == start_column
                            && selection.width == width
                            && selection.command == command
                    }) {
                        continue;
                    }
                    content.selections.push(OverlaySelection {
                        logical_id,
                        line_index,
                        start_column,
                        width,
                        command: command.to_string(),
                        kind: OverlaySelectionKind::Primary,
                    });
                }
                break;
            }
        }
    }
}

/// Returns the start column and trimmed text of the first rendered table cell.
fn markdown_table_first_cell_visible_text(line: &str) -> Option<(usize, &str)> {
    let mut dividers = line.match_indices('│').map(|(index, _)| index);
    let first = dividers.next()?;
    let second = dividers.next()?;
    let cell_start = first.saturating_add('│'.len_utf8());
    let cell = line.get(cell_start..second)?;
    let trimmed_start = cell.trim_start();
    let leading_bytes = cell.len().saturating_sub(trimmed_start.len());
    let visible = trimmed_start.trim_end();
    (!visible.is_empty()).then(|| {
        (
            UnicodeWidthStr::width(&line[..cell_start.saturating_add(leading_bytes)]),
            visible,
        )
    })
}

/// Appends a muted Save-path completion suffix without changing editable input.
pub(super) fn append_record_browser_save_completion_shadow(
    overlay: &mut RuntimeDisplayOverlay,
    input: &str,
    suffix: &str,
) {
    let prompt_prefix = "Save to: ";
    let prompt_line = format!("{prompt_prefix}{input}");
    let Some(line_index) = overlay.lines.iter().position(|line| line == &prompt_line) else {
        return;
    };
    let start = UnicodeWidthStr::width(prompt_line.as_str());
    overlay.lines[line_index].push_str(suffix);
    let rendition = GraphicRendition {
        dim: true,
        ..GraphicRendition::default()
    };
    overlay.line_style_spans[line_index].push(TerminalStyleSpan {
        start,
        length: UnicodeWidthStr::width(suffix),
        rendition,
    });
}

pub(super) fn record_browser_command_name(command: &str) -> Option<String> {
    let trimmed = command.trim_start();
    let body = trimmed.strip_prefix('/')?;
    let name = body.split_whitespace().next()?;
    matches!(
        name,
        "list-personalities" | "show-approvals" | "show-context" | "show-issues" | "show-memories"
    )
    .then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_mux::render::RichTextLineKind;

    /// Verifies restoring a record table with one retained wrapped-link fragment
    /// adds only the missing continuation fragment and gives both one logical ID.
    #[test]
    fn restores_missing_wrapped_record_link_fragment_after_partial_retention() {
        let command = "/show-issues issue-42";
        let browser = mez_mux::record_browser::RecordBrowser::new(
            "Issues",
            vec![mez_mux::record_browser::RecordBrowserRecord {
                id: "issue-42".to_string(),
                open_command: Some(command.to_string()),
                title: "Wrapped issue".to_string(),
                metadata: Vec::new(),
                markdown: String::new(),
            }],
            Vec::new(),
        )
        .unwrap();
        let mut content = RuntimeCommandDisplayOverlayContent {
            command: Some("show-issues".to_string()),
            live_source: None,
            lines: vec![
                "│ issue- │ Wrapped issue │".to_string(),
                "│ 42 │              │".to_string(),
            ],
            line_style_spans: vec![Vec::new(), Vec::new()],
            line_kinds: vec![RichTextLineKind::Normal, RichTextLineKind::Normal],
            line_copy_texts: vec![None, None],
            selections: vec![OverlaySelection {
                logical_id: 99,
                line_index: 0,
                start_column: 2,
                width: 6,
                command: command.to_string(),
                kind: OverlaySelectionKind::Primary,
            }],
        };

        restore_record_browser_table_link_selections(&mut content, &browser);

        assert_eq!(content.selections.len(), 2);
        assert_eq!(
            content
                .selections
                .iter()
                .map(|selection| (
                    selection.logical_id,
                    selection.line_index,
                    selection.start_column,
                    selection.width,
                    selection.command.as_str(),
                ))
                .collect::<Vec<_>>(),
            vec![(0, 0, 2, 6, command), (0, 1, 2, 2, command)]
        );
    }
}
