//! Pane-agent selector and record-browser layout projection.

use super::action_registry::{
    OverlayActionRegistry, OverlayActionTarget, RuntimeOverlayAction,
    overlay_record_browser_open_target, overlay_record_browser_prompt_select_target,
};
use super::display_content::{
    RuntimeCommandDisplayOverlayContent, runtime_command_overlay_available_width,
    wrap_runtime_command_display_overlay_content,
};
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
    registry: &mut OverlayActionRegistry,
    ui_theme: &mez_mux::theme::UiTheme,
    terminal_width: usize,
    prose_width: usize,
) -> bool {
    render_record_browser_overlay_matching(
        overlay,
        registry,
        ui_theme,
        terminal_width,
        prose_width,
        None,
    )
}

/// Rebuilds a record-browser overlay, optionally restricting its list rows to
/// an in-page pager search while retaining the generic match highlight state.
pub(super) fn render_record_browser_overlay_matching(
    overlay: &mut RuntimeDisplayOverlay,
    registry: &mut OverlayActionRegistry,
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
    // Selector gutter columns are reserved only when this render registers
    // selectable ranges, so a browser without an openable row keeps the full
    // body width while a selectable list keeps its rows inside the body width.
    let registers_row_actions = prompt_selection.is_some()
        || (!record_browser.browser.is_detail_view()
            && record_browser.browser.records().iter().any(|record| {
                record_browser_open_target(record).is_some() || record_browser.command == "resume"
            }));
    let terminal_width = if registers_row_actions {
        runtime_command_overlay_available_width(terminal_width, true)
    } else {
        terminal_width
    };
    let content_width = if record_browser.browser.is_detail_view() {
        prose_width
    } else {
        terminal_width
    };
    let mut content = if let Some(layout) = record_browser.browser.render_list_layout(
        search_query.unwrap_or_default(),
        terminal_width,
        &crate::runtime::render::presentation::agent_rich_text_theme(ui_theme),
    ) {
        record_browser_layout_content(
            &record_browser.command,
            &record_browser.browser,
            layout,
            ui_theme,
            terminal_width,
        )
    } else {
        runtime_agent_shell_markdown_overlay_content_for_layout(
            Some(record_browser.command.clone()),
            &page.markdown,
            ui_theme,
            terminal_width,
            content_width,
        )
    };
    if let Some(prompt_selection) = prompt_selection {
        content.actions = record_browser_prompt_actions(&content.lines, prompt_selection);
    }
    registry.begin_generation();
    let selections = registry.register_all(std::mem::take(&mut content.actions));
    overlay.lines = content.lines;
    overlay.line_style_spans = content.line_style_spans;
    overlay.line_copy_texts = content.line_copy_texts;
    overlay.selections = selections;
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
            overlay.scroll_offset = search_match
                .line_index
                .saturating_sub(mez_mux::overlay::overlay_fixed_prefix_rows(overlay));
        }
    }
    true
}

/// Binds literal first-cell geometry to product-owned targets before wrapping.
/// Display labels and metadata never supply record identity or command authority.
fn record_browser_layout_content(
    command: &str,
    browser: &mez_mux::record_browser::RecordBrowser,
    layout: mez_mux::record_browser::RecordBrowserLayout,
    ui_theme: &mez_mux::theme::UiTheme,
    width: usize,
) -> RuntimeCommandDisplayOverlayContent {
    let mut content = RuntimeCommandDisplayOverlayContent {
        command: Some(command.to_string()),
        live_source: None,
        lines: Vec::new(),
        line_style_spans: Vec::new(),
        line_kinds: Vec::new(),
        line_copy_texts: Vec::new(),
        actions: Vec::new(),
    };
    for line in layout.lines {
        content.lines.push(line.display);
        content.line_style_spans.push(line.style_spans);
        content.line_kinds.push(line.kind);
        content.line_copy_texts.push(line.copy_text);
    }
    for range in layout.record_ranges {
        let Some(record) = browser.records().get(range.row) else {
            continue;
        };
        let Some(target) = (command == "list-personalities")
            .then(|| OverlayActionTarget::RecordBrowserSelect {
                record_id: record.id.clone(),
            })
            .or_else(|| record_browser_open_target(record))
            .or_else(|| {
                (command == "resume").then(|| OverlayActionTarget::RecordBrowserSelect {
                    record_id: record.id.clone(),
                })
            })
        else {
            continue;
        };
        if range.width == 0 {
            continue;
        }
        content.actions.push(RuntimeOverlayAction {
            logical_id: range.row,
            line_index: range.line,
            start_column: range.start,
            width: range.width,
            target,
            kind: OverlaySelectionKind::Primary,
        });
        if let Some(spans) = content.line_style_spans.get_mut(range.line) {
            push_or_extend_style_span(
                spans,
                TerminalStyleSpan {
                    start: range.start,
                    length: range.width,
                    rendition: overlay_link_rendition(ui_theme),
                },
            );
        }
    }
    wrap_runtime_command_display_overlay_content(content, width, width)
}

/// Returns the validated open target for one record-browser record.
fn record_browser_open_target(
    record: &mez_mux::record_browser::RecordBrowserRecord,
) -> Option<OverlayActionTarget> {
    record
        .open_command
        .as_deref()
        .and_then(overlay_record_browser_open_target)
}

/// Registers one typed action per visible record-browser prompt option row.
fn record_browser_prompt_actions(
    lines: &[String],
    prompt_selection: mez_mux::record_browser::RecordBrowserPromptSelection,
) -> Vec<RuntimeOverlayAction> {
    lines
        .iter()
        .enumerate()
        .skip(prompt_selection.start_line)
        .take(prompt_selection.option_count)
        .enumerate()
        .map(|(option_index, (line_index, line))| RuntimeOverlayAction {
            logical_id: line_index,
            line_index,
            start_column: 0,
            width: UnicodeWidthStr::width(line.as_str()),
            target: overlay_record_browser_prompt_select_target(option_index),
            kind: OverlaySelectionKind::Primary,
        })
        .collect()
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

    /// Verifies a wrapped record-browser table row registers one open action
    /// whose physical fragments share a single logical id.
    #[test]
    fn registers_open_action_for_wrapped_record_id_fragments() {
        let browser = mez_mux::record_browser::RecordBrowser::new(
            "Issues",
            vec![mez_mux::record_browser::RecordBrowserRecord {
                id: "issue-42".to_string(),
                open_command: Some("/show-issues issue-42".to_string()),
                title: "Wrapped issue".to_string(),
                metadata: Vec::new(),
                markdown: String::new(),
            }],
            Vec::new(),
        )
        .unwrap();
        let layout = mez_mux::record_browser::RecordBrowserLayout {
            lines: ["│ issue- │ Wrapped issue │", "│ 42 │              │"]
                .into_iter()
                .map(|display| RichTextLine {
                    display: display.to_string(),
                    style_spans: Vec::new(),
                    copy_text: None,
                    kind: RichTextLineKind::MarkdownTableRow,
                })
                .collect(),
            record_ranges: vec![
                mez_mux::render::TableFirstCellRange {
                    row: 0,
                    line: 0,
                    start: 2,
                    width: 6,
                },
                mez_mux::render::TableFirstCellRange {
                    row: 0,
                    line: 1,
                    start: 2,
                    width: 2,
                },
            ],
        };
        let content = record_browser_layout_content(
            "show-issues",
            &browser,
            layout,
            &mez_mux::theme::deepforest_ui_theme(),
            80,
        );

        assert_eq!(content.actions.len(), 2, "{content:?}");
        let expected = OverlayActionTarget::RecordBrowserOpen {
            command_name: "show-issues".to_string(),
            record_id: "issue-42".to_string(),
        };
        assert_eq!(
            content
                .actions
                .iter()
                .map(|action| (
                    action.logical_id,
                    action.line_index,
                    action.start_column,
                    action.width,
                    action.target.clone(),
                ))
                .collect::<Vec<_>>(),
            vec![(0, 0, 2, 6, expected.clone()), (0, 1, 2, 2, expected),]
        );
    }

    /// Verifies wrapped record-browser UUID fragments retain the link rendition
    /// registered for their row and share the active selection background
    /// across zebra rows.
    #[test]
    fn wrapped_record_browser_links_keep_final_composed_rendition() {
        let ids = [
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
        ];
        let mut browser = mez_mux::record_browser::RecordBrowser::new(
            "Issues",
            ids.iter()
                .enumerate()
                .map(|(index, id)| mez_mux::record_browser::RecordBrowserRecord {
                    id: (*id).to_string(),
                    open_command: Some(format!("/show-issues {id}")),
                    title: format!("Issue {}", index.saturating_add(1)),
                    metadata: vec![("title".to_string(), format!("Issue {}", index + 1))],
                    markdown: String::new(),
                })
                .collect(),
            Vec::new(),
        )
        .unwrap();
        browser.set_table_columns_with_labels(vec![("Title".to_string(), "title".to_string())]);
        let mut overlay = RuntimeDisplayOverlay {
            lines: Vec::new(),
            line_style_spans: Vec::new(),
            line_copy_texts: Vec::new(),
            scroll_offset: 0,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            selections: Vec::new(),
            active_selection_index: None,
            dismiss_on_any_input: false,
            live_source: None,
            record_browser: Some(
                crate::runtime::service_state::RuntimeRecordBrowserOverlayState {
                    pane_id: "%1".to_string(),
                    command: "show-issues".to_string(),
                    source: None,
                    browser,
                    stack: Vec::new(),
                },
            ),
        };
        let ui_theme = mez_mux::theme::deepforest_ui_theme();
        let mut registry = OverlayActionRegistry::default();

        assert!(render_record_browser_overlay(
            &mut overlay,
            &mut registry,
            &ui_theme,
            24,
            24
        ));
        assert!(overlay.selections.len() >= 4, "{overlay:?}");
        for selection in &overlay.selections {
            let spans =
                overlay_rendered_line_style_spans(&overlay, selection.line_index, 24, &ui_theme);
            let start = overlay_rendered_selection_start(&overlay, selection);
            for column in start..start.saturating_add(selection.width) {
                let rendition = rendered_line_rendition_at(&spans, column);
                assert!(rendition.bold, "column {column} lost bold: {spans:?}");
                assert!(
                    rendition.underline,
                    "column {column} lost underline: {spans:?}"
                );
                assert_eq!(
                    rendition.foreground,
                    Some(ui_theme.colors.agent_transcript_command.foreground),
                    "column {column} lost link foreground: {spans:?}"
                );
                if selection.logical_id == 0 {
                    assert_eq!(
                        rendition.background,
                        Some(ui_theme.colors.agent_model.background),
                        "column {column} lost active background: {spans:?}"
                    );
                }
            }
        }
    }
}
