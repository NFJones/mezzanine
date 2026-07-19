//! Pane-agent selector and record-browser layout projection.

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
    let Some(record_browser) = overlay.record_browser.as_ref() else {
        return false;
    };
    let page = record_browser.browser.render_page();
    let prompt_selection = record_browser.browser.prompt_selection();
    let mut content = runtime_agent_shell_markdown_overlay_content_for_layout(
        Some(record_browser.command.clone()),
        &page.markdown,
        ui_theme,
        terminal_width,
        prose_width,
    );
    if let Some(prompt_selection) = prompt_selection {
        content.selections = content
            .lines
            .iter()
            .enumerate()
            .skip(prompt_selection.start_line)
            .take(prompt_selection.option_count)
            .map(|(line_index, line)| OverlaySelection {
                line_index,
                start_column: 0,
                width: UnicodeWidthStr::width(line.as_str()),
                command: String::new(),
                kind: OverlaySelectionKind::Primary,
            })
            .collect();
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
            .or(Some(0))
    };
    overlay.search_input = None;
    overlay.search_match = None;
    true
}

pub(super) fn record_browser_command_name(command: &str) -> Option<String> {
    let trimmed = command.trim_start();
    let body = trimmed.strip_prefix('/')?;
    let name = body.split_whitespace().next()?;
    matches!(name, "show-context" | "show-issues" | "show-memories").then(|| name.to_string())
}
