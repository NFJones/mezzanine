//! Display overlay rows, choices, fields, and human-readable content.

use super::product_content::*;
use super::record_adapter::{
    RuntimeDisplayRecord, runtime_display_field_label, runtime_display_field_value,
};
use crate::runtime::render::*;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

/// Display lines and selectable actions derived from command JSON output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeCommandDisplayOverlayContent {
    /// Terminal command that produced these display lines, when present.
    pub(crate) command: Option<String>,
    /// Human-readable lines rendered in the command display overlay.
    pub(crate) lines: Vec<String>,
    /// Visible terminal styles for each rendered display line.
    pub(crate) line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Optional command actions keyed by line index.
    pub(crate) selections: Vec<OverlaySelection>,
}

/// One rendered command-overlay display line with selectable choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeDisplayLine {
    /// Human-readable text shown in the overlay.
    pub(crate) text: String,
    /// Interactive choices rendered inside `text`.
    pub(crate) choices: Vec<RuntimeDisplayChoicePlacement>,
    /// Visible terminal styles applied to `text`.
    pub(crate) style_spans: Vec<TerminalStyleSpan>,
}

/// One selectable choice and its location in a display line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeDisplayChoicePlacement {
    /// Zero-based display column where the choice starts.
    pub(crate) start_column: usize,
    /// Display-cell width of the choice label.
    pub(crate) width: usize,
    /// Human-readable label shown to the user.
    pub(crate) label: String,
    /// Terminal command executed by this choice.
    pub(crate) command: String,
    /// Visual importance of this choice.
    pub(crate) kind: OverlaySelectionKind,
}

/// One parsed executable display choice before it has a line position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeDisplayChoice {
    /// Human-readable label shown to the user.
    pub(crate) label: String,
    /// Terminal command executed by this choice.
    pub(crate) command: String,
    /// Visual importance of this choice.
    pub(crate) kind: OverlaySelectionKind,
}

/// Parses command JSON output into human-readable overlay content.
pub(crate) fn runtime_command_display_overlay_content(
    body: &str,
    ui_theme: &UiTheme,
) -> Result<RuntimeCommandDisplayOverlayContent> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("runtime command response is not valid JSON"))?;
    let outcomes = parsed
        .get("outcomes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| MezError::invalid_state("runtime command response has no outcomes"))?;
    let mut content = RuntimeCommandDisplayOverlayContent {
        command: None,
        lines: Vec::new(),
        line_style_spans: Vec::new(),
        selections: Vec::new(),
    };
    for outcome in outcomes {
        if outcome.get("kind").and_then(serde_json::Value::as_str) != Some("display") {
            continue;
        }
        if content.command.is_none() {
            content.command = outcome
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
        }
        if let Some(body) = outcome.get("body").and_then(serde_json::Value::as_str) {
            let command = outcome
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
            let content_type = outcome
                .get("content_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if agent_output_content_type_is_markdown(content_type)
                || terminal_command_display_body_is_markdown(command.as_deref(), body)
            {
                content.extend_markdown_body(command, body, ui_theme);
            } else {
                content.extend_body(body);
            }
        }
    }
    Ok(content)
}

/// Returns true when a terminal command display body is authored as Markdown.
pub(super) fn terminal_command_display_body_is_markdown(command: Option<&str>, body: &str) -> bool {
    match command {
        Some("help") => body.trim_start().starts_with('#'),
        Some("list-themes") => body.trim_start().starts_with('|'),
        _ => false,
    }
}

/// Verifies `list-themes` bodies authored as Markdown tables take the command
/// overlay Markdown path rather than the legacy plain-text display path.
#[cfg(test)]
#[test]
pub(super) fn list_themes_command_display_detects_markdown_tables() {
    assert!(terminal_command_display_body_is_markdown(
        Some("list-themes"),
        "| active | theme | preview | source | preview colors | action |"
    ));
    assert!(!terminal_command_display_body_is_markdown(
        Some("list-themes"),
        "active     theme                   preview"
    ));
}

/// Verifies Markdown-rendered `list-themes` rows keep clickable theme actions
/// and apply per-block preview colors through the shared overlay renderer.
#[cfg(test)]
#[test]
pub(super) fn list_themes_markdown_overlay_preserves_actions_and_preview_colors() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content(
        Some("list-themes".to_string()),
        "| active | theme | preview | source | preview colors | action |\n| --- | --- | --- | --- | --- | --- |\n| ★ active | kanagawa | █████ | builtin | #111111,#222222,#333333,#444444,#555555 | [`set-theme kanagawa`](mez-agent:set-theme%20kanagawa) |",
        &ui_theme,
    );

    assert!(
        content
            .selections
            .iter()
            .any(|selection| selection.command == "set-theme kanagawa"),
        "{content:?}"
    );
    let line_index = content
        .lines
        .iter()
        .position(|line| line.contains("kanagawa") && line.contains("█████"))
        .unwrap();
    let preview_start = content.lines[line_index].find("█████").unwrap();
    let preview_column = UnicodeWidthStr::width(&content.lines[line_index][..preview_start]);
    let preview_spans = content.line_style_spans[line_index]
        .iter()
        .filter(|span| {
            span.start >= preview_column && span.start < preview_column.saturating_add(5)
        })
        .collect::<Vec<_>>();

    assert_eq!(preview_spans.len(), 5, "{content:?}");
    assert_eq!(
        preview_spans[0].rendition.foreground,
        Some(mez_terminal::TerminalColor::Rgb(0x11, 0x11, 0x11))
    );
    assert_eq!(
        preview_spans[4].rendition.foreground,
        Some(mez_terminal::TerminalColor::Rgb(0x55, 0x55, 0x55))
    );
}

/// Verifies rendered `list-themes` overlay headers reserve the same selector
/// gutter width as selectable body rows.
#[cfg(test)]
#[test]
pub(super) fn list_themes_rendered_overlay_lines_align_headers_with_selectable_rows() {
    let overlay = RuntimeDisplayOverlay {
        lines: vec![
            "| active | theme |".to_string(),
            "| --- | --- |".to_string(),
            "| ★ active | kanagawa |".to_string(),
        ],
        line_style_spans: vec![Vec::new(); 3],
        scroll_offset: 0,
        selections: vec![OverlaySelection {
            line_index: 2,
            start_column: 13,
            width: 8,
            command: "set-theme kanagawa".to_string(),
            kind: OverlaySelectionKind::Primary,
        }],
        active_selection_index: Some(0),
        dismiss_on_any_input: false,
        search_input: None,
        search_query: None,
        search_match: None,
        search_status: None,
        mouse_selection: None,
        record_browser: None,
    };

    let rendered = overlay_render_lines(&overlay);

    assert_eq!(
        rendered[0],
        format!("{DISPLAY_OVERLAY_INACTIVE_SELECTOR}| active | theme |")
    );
    assert_eq!(
        rendered[1],
        format!("{DISPLAY_OVERLAY_INACTIVE_SELECTOR}| --- | --- |")
    );
    assert_eq!(
        rendered[2],
        format!("{DISPLAY_OVERLAY_ACTIVE_SELECTOR}| ★ active | kanagawa |")
    );
}

/// Returns whether a terminal command response needs the modal display overlay.
pub(crate) fn runtime_command_display_should_open_overlay(
    content: &RuntimeCommandDisplayOverlayContent,
) -> bool {
    if content.lines.is_empty() {
        return false;
    }
    if !content.selections.is_empty() {
        return true;
    }
    if content.lines.len() <= 1 {
        return false;
    }
    !content
        .command
        .as_deref()
        .is_some_and(runtime_immediate_terminal_command_name)
}

/// Returns the transient status line for short terminal command feedback.
pub(crate) fn runtime_command_display_transient_status_line(
    content: &RuntimeCommandDisplayOverlayContent,
) -> Option<String> {
    if !content.selections.is_empty() {
        return None;
    }
    if !content
        .command
        .as_deref()
        .is_some_and(runtime_transient_terminal_command_name)
    {
        return None;
    }
    content
        .lines
        .iter()
        .find(|line| !line.trim().is_empty())
        .cloned()
}

/// Returns true for terminal commands whose one-line feedback is transient.
pub(crate) fn runtime_transient_terminal_command_name(command: &str) -> bool {
    matches!(
        command,
        "auth-status"
            | "agent-shell"
            | "bind-key"
            | "copy-selection"
            | "create-buffer"
            | "mark-pane-ready"
            | "next-layout"
            | "paste-buffer"
            | "paste-clipboard"
            | "pipe-pane"
            | "rebalance-window"
            | "refresh-provider-info"
            | "resize-pane"
            | "select-layout"
            | "send-prefix"
            | "set-option"
            | "set-theme"
            | "source-file"
            | "synchronize-panes"
            | "unbind-key"
            | "zoom-pane"
    )
}

/// Returns true for terminal commands whose success is already observable.
pub(crate) fn runtime_immediate_terminal_command_name(command: &str) -> bool {
    matches!(
        command,
        "send-prefix"
            | "agent-shell"
            | "copy-selection"
            | "paste-clipboard"
            | "paste-buffer"
            | "create-buffer"
            | "bind-key"
            | "unbind-key"
            | "mark-pane-ready"
            | "set-theme"
            | "set-option"
            | "source-file"
            | "pipe-pane"
            | "mcp"
            | "mcp-status"
            | "refresh-client"
            | "refresh"
    )
}

/// Converts compact command-display field records into readable overlay lines.
///
/// Runtime command results keep their JSON bodies stable for control clients
/// and automation. This presentation helper only affects text shown in the TUI
/// command overlay or pane-local agent shell output.
pub(crate) fn runtime_human_readable_display_lines(body: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in body.lines() {
        lines.extend(
            runtime_human_readable_display_line_with_choices(line)
                .into_iter()
                .map(|line| line.text),
        );
    }
    lines
}

/// Converts one compact display line and returns selector choices.
pub(crate) fn runtime_human_readable_display_line_with_choices(
    line: &str,
) -> Vec<RuntimeDisplayLine> {
    let record = if line.split_whitespace().count() > 1 {
        RuntimeDisplayRecord::parse_space_delimited(line)
            .or_else(|| RuntimeDisplayRecord::parse_colon_delimited(line))
    } else {
        RuntimeDisplayRecord::parse_colon_delimited(line)
            .or_else(|| RuntimeDisplayRecord::parse_space_delimited(line))
    };
    if let Some(record) = record {
        if let Some(text) = runtime_custom_human_readable_display_line(&record) {
            vec![RuntimeDisplayLine {
                text,
                choices: Vec::new(),
                style_spans: Vec::new(),
            }]
        } else {
            vec![record.into_display_line()]
        }
    } else {
        vec![RuntimeDisplayLine {
            text: line.to_string(),
            choices: Vec::new(),
            style_spans: Vec::new(),
        }]
    }
}

/// Formats high-volume runtime status records as terse sentences.
pub(crate) fn runtime_custom_human_readable_display_line(
    record: &RuntimeDisplayRecord,
) -> Option<String> {
    if record.field_value("source") == Some("runtime-agent-say") {
        return runtime_agent_say_copy_sentence(record);
    }
    if record.field_value("forked").is_some() && record.field_value("conversation_id").is_some() {
        return runtime_agent_fork_sentence(record);
    }
    match record.field_value("source")? {
        "runtime-routing" => runtime_routing_sentence(record),
        "runtime-policy" => runtime_policy_sentence(record),
        _ => None,
    }
}

/// Formats `/copy` rows for retained say text as concise runtime status text.
pub(crate) fn runtime_agent_say_copy_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
    match record.field_value("say")? {
        "written" => Some(format!(
            "copied {} bytes from {} to {}.",
            record.field_value("bytes").unwrap_or("0"),
            record.field_value("turn").unwrap_or("unknown turn"),
            runtime_copy_destination_display(record)
        )),
        "not-written" => Some(format!(
            "agent say text was not copied: {}.",
            runtime_display_field_value(record.field_value("reason").unwrap_or("unavailable"))
        )),
        _ => None,
    }
}

/// Formats the target destination carried by a `/copy` status row.
pub(crate) fn runtime_copy_destination_display(record: &RuntimeDisplayRecord) -> String {
    match record.field_value("destination").unwrap_or("pane") {
        "buffer" => format!(
            "buffer {}",
            record.field_value("buffer").unwrap_or("agent-output")
        ),
        "clipboard" => "clipboard".to_string(),
        "pane" => "the pane".to_string(),
        destination => runtime_display_field_value(destination),
    }
}

/// Formats `/fork` rows as a readable sentence rather than raw key/value data.
pub(crate) fn runtime_agent_fork_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
    let pane = record.field_value("pane")?;
    let source_pane = record.field_value("source_pane").unwrap_or("unknown pane");
    let conversation_id = record.field_value("conversation_id")?;
    let entries = record.field_value("entries").unwrap_or("0");
    match record.field_value("forked")? {
        "true" => Some(format!(
            "forked {entries} transcript entries from {source_pane} into {pane}; conversation {conversation_id}."
        )),
        "false" => Some(format!(
            "conversation {conversation_id} was not forked: {}.",
            runtime_display_field_value(record.field_value("reason").unwrap_or("unavailable"))
        )),
        _ => None,
    }
}

/// Formats pane-local routing status and mutation rows.
pub(crate) fn runtime_routing_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
    let pane = record.field_value("pane")?;
    let enabled = runtime_enabled_phrase(record.field_value("enabled")?);
    let default = runtime_enabled_phrase(record.field_value("default")?);
    if let Some(changed) = record.field_value("changed") {
        let change = if changed == "true" {
            "changed"
        } else {
            "unchanged"
        };
        return Some(format!(
            "routing is {enabled} for pane {pane}; default is {default}; {change}."
        ));
    }
    if let Some(override_present) = record.field_value("override_present") {
        let override_text = if override_present == "true" {
            "pane override is present"
        } else {
            "no pane override"
        };
        return Some(format!(
            "routing is {enabled} for pane {pane}; default is {default}; {override_text}."
        ));
    }
    None
}

/// Formats permission and approval-policy rows as human-readable statements.
pub(crate) fn runtime_policy_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
    if let (Some(field), Some(current), Some(requested)) = (
        record.field_value("field"),
        record.field_value("current"),
        record.field_value("requested"),
    ) {
        let label = runtime_display_field_label(field).to_ascii_lowercase();
        let changed = record.field_value("changed").unwrap_or("false") == "true";
        let authority = record
            .field_value("authority_change")
            .filter(|value| *value != "none")
            .map(|value| format!("; authority {value}"))
            .unwrap_or_default();
        let approval = record
            .field_value("approved_by")
            .filter(|value| *value != "none")
            .map(|value| format!(" approved by {}", runtime_display_field_value(value)))
            .unwrap_or_default();
        if changed {
            return Some(format!(
                "{label} changed from {} to {}{authority}{approval}.",
                runtime_display_field_value(current),
                runtime_display_field_value(requested)
            ));
        }
        return Some(format!(
            "{label} remains {} after requested {}{authority}{approval}.",
            runtime_display_field_value(current),
            runtime_display_field_value(requested)
        ));
    }
    if let (Some(policy), Some(preset)) = (
        record.field_value("approval_policy"),
        record.field_value("preset"),
    ) {
        let bypass = runtime_enabled_phrase(record.field_value("bypass").unwrap_or("false"));
        let rules = record.field_value("rules").unwrap_or("0");
        return Some(format!(
            "permissions use preset {preset}; approval policy is {}; bypass is {bypass}; {rules} command rules.",
            runtime_display_field_value(policy)
        ));
    }
    None
}

/// Returns `enabled` or `disabled` for compact boolean display values.
pub(crate) fn runtime_enabled_phrase(value: &str) -> &'static str {
    if value == "true" {
        "enabled"
    } else {
        "disabled"
    }
}

impl RuntimeCommandDisplayOverlayContent {
    /// Appends one markdown display body to this overlay content.
    fn extend_markdown_body(&mut self, command: Option<String>, body: &str, ui_theme: &UiTheme) {
        let mut markdown_content =
            runtime_agent_shell_markdown_overlay_content(command, body, ui_theme);
        let line_offset = self.lines.len();
        self.lines.append(&mut markdown_content.lines);
        self.line_style_spans
            .append(&mut markdown_content.line_style_spans);
        self.selections.extend(
            markdown_content
                .selections
                .into_iter()
                .map(|mut selection| {
                    selection.line_index += line_offset;
                    selection
                }),
        );
    }

    /// Appends one raw display body to this overlay content.
    fn extend_body(&mut self, body: &str) {
        for line in body.lines() {
            for display_line in runtime_human_readable_display_line_with_choices(line) {
                let line_index = self.lines.len();
                self.lines.push(display_line.text);
                self.line_style_spans.push(display_line.style_spans);
                for choice in display_line.choices {
                    self.selections.push(OverlaySelection {
                        line_index,
                        start_column: choice.start_column,
                        width: choice.width,
                        command: choice.command,
                        kind: choice.kind,
                    });
                }
            }
        }
    }
}
