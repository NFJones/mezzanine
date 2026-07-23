//! Product prompt, shell-result, command-link, and status content projection.

use super::display_content::{
    RuntimeCommandDisplayOverlayContent, runtime_command_overlay_available_width,
    runtime_human_readable_display_lines, wrap_runtime_command_display_overlay_content,
};
use super::record_adapter::runtime_theme_preview_style_spans;
use crate::runtime::render::*;
use unicode_width::UnicodeWidthStr;

/// Render placement for an open pane agent status selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneAgentStatusSelectorLayout {
    /// Zero-based column where selector rows begin.
    pub(crate) column: u16,
    /// Width in terminal cells reserved for selector rows.
    pub(crate) width: u16,
    /// Visible selector items with their rendered rows.
    pub(crate) visible_items: Vec<PaneAgentStatusSelectorLayoutItem>,
}

/// Render placement for one visible selector item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneAgentStatusSelectorLayoutItem {
    /// Index into the selector item list.
    pub(crate) item_index: usize,
    /// Zero-based terminal row where this item is drawn.
    pub(crate) row: u16,
}

/// Maximum number of model/reasoning picker rows shown at once.
pub(crate) const PANE_AGENT_STATUS_SELECTOR_MAX_ROWS: usize = 30;
/// Returns a compact MCP server state label for command completion details.
pub(crate) fn agent_shell_mcp_display_state_name(
    enabled: bool,
    status: McpServerStatus,
) -> &'static str {
    if !enabled {
        return "disabled";
    }
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Runs the default runtime agent prompt input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn default_runtime_agent_prompt_input() -> RuntimeAgentPromptInput {
    RuntimeAgentPromptInput {
        prompt: ReadlinePrompt::new(ReadlinePromptKind::Agent),
        decoder: ReadlineInputDecoder::new(),
        display_lines: Vec::new(),
        pending_ctrl_c_exit_at_unix_ms: None,
    }
}

/// Runs the runtime primary prompt input operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_primary_prompt_input(
    kind: ReadlinePromptKind,
    prefill: &str,
) -> RuntimePrimaryPromptInput {
    let mut prompt = ReadlinePrompt::new(kind);
    prompt.buffer.set_line(prefill);
    RuntimePrimaryPromptInput {
        prompt,
        decoder: ReadlineInputDecoder::new(),
    }
}

/// Runs the runtime agent shell display lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
/// Carries typed agent-shell display output decoded from JSON responses.
///
/// Markdown output is kept as one raw body so it can flow through the same
/// renderer and copy-preservation path as model-authored markdown `say`
/// actions. Plain output remains line-oriented because legacy command display
/// bodies are key/value text rather than presentation markup.
pub(crate) enum RuntimeAgentShellDisplayOutput {
    /// No user-facing display should be rendered for this command response.
    Suppressed,
    /// One-line command feedback rendered through the transient status bar.
    TransientStatus(Vec<String>),
    /// Preformatted display lines for plain text and diagnostic responses.
    Lines(Vec<String>),
    /// Display content rendered through the command overlay pager.
    Overlay(RuntimeCommandDisplayOverlayContent),
}

/// Runs the runtime agent shell display output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_agent_shell_display_output(
    body: &str,
    ui_theme: &UiTheme,
    terminal_width: usize,
    _configured_wrap_width: usize,
) -> Result<RuntimeAgentShellDisplayOutput> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("agent shell response is not valid JSON"))?;
    let kind = parsed.get("kind").and_then(serde_json::Value::as_str);
    let command = parsed
        .get("command")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    if kind == Some("mutated") {
        if command
            .as_deref()
            .is_some_and(runtime_agent_shell_suppressed_mutation_command_name)
        {
            return Ok(RuntimeAgentShellDisplayOutput::Suppressed);
        }
        if let Some(body) = parsed.get("body").and_then(serde_json::Value::as_str) {
            let mut lines = runtime_human_readable_display_lines(body);
            lines.truncate(200);
            return Ok(RuntimeAgentShellDisplayOutput::TransientStatus(lines));
        }
        return Ok(RuntimeAgentShellDisplayOutput::Suppressed);
    }
    let mut lines = Vec::new();
    if let Some(body) = parsed.get("body").and_then(serde_json::Value::as_str) {
        let content_type = parsed
            .get("content_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if agent_output_content_type_is_markdown(content_type) {
            if body.starts_with("agent command error:") {
                lines.extend(runtime_human_readable_display_lines(body));
                lines.truncate(200);
                return Ok(RuntimeAgentShellDisplayOutput::Lines(lines));
            }
            let display_width = terminal_width.max(1);
            let content = runtime_agent_shell_markdown_overlay_content_for_layout(
                command.clone(),
                body,
                ui_theme,
                terminal_width,
                display_width,
            );
            if runtime_command_display_should_open_overlay(&content) {
                return Ok(RuntimeAgentShellDisplayOutput::Overlay(content));
            }
            if command
                .as_deref()
                .is_some_and(runtime_agent_shell_transient_display_command_name)
            {
                let mut lines = runtime_human_readable_display_lines(body);
                lines.truncate(200);
                return Ok(RuntimeAgentShellDisplayOutput::TransientStatus(lines));
            }
            lines.extend(runtime_human_readable_display_lines(body));
            lines.truncate(200);
            return Ok(RuntimeAgentShellDisplayOutput::Lines(lines));
        } else {
            lines.extend(runtime_human_readable_display_lines(body));
            if command
                .as_deref()
                .is_some_and(runtime_agent_shell_transient_display_command_name)
            {
                lines.truncate(200);
                return Ok(RuntimeAgentShellDisplayOutput::TransientStatus(lines));
            }
        }
    }
    lines.truncate(200);
    Ok(RuntimeAgentShellDisplayOutput::Lines(lines))
}

/// Returns true for slash-command mutations whose success is already visible.
pub(super) fn runtime_agent_shell_suppressed_mutation_command_name(command: &str) -> bool {
    matches!(command, "clear" | "new" | "prompt")
}

/// Returns true for slash-command displays that should not enter pane logs.
pub(super) fn runtime_agent_shell_transient_display_command_name(command: &str) -> bool {
    matches!(
        command,
        "approval"
            | "directive"
            | "latency"
            | "log-level"
            | "memory"
            | "personality"
            | "routing"
            | "thinking"
    )
}

/// Verifies `/show` Markdown tables are laid out to the pager width before
/// generic physical-row wrapping can damage their structure.
#[cfg(test)]
#[test]
fn show_markdown_overlay_uses_width_aware_table_layout() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let content = runtime_agent_shell_markdown_overlay_content_for_width(
        Some("show-issues".to_string()),
        "| Field | Value |\n| --- | --- |\n| description | alpha beta gamma delta |",
        &ui_theme,
        Some(24),
    );

    assert!(content.lines.len() > 3, "{content:?}");
    assert!(
        content
            .lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 24),
        "{content:?}"
    );
}

/// Verifies an initial record-list response uses the full overlay width rather
/// than the narrower prose cap that applies only after a detail view opens.
#[cfg(test)]
#[test]
fn initial_show_record_list_uses_full_overlay_width() {
    let output = runtime_agent_shell_display_output(
        r##"{"kind":"display","command":"show-issues","content_type":"text/markdown; charset=utf-8","body":"# Issues\n\n- very long issue title that should remain on the initial full-width record list"}"##,
        &mez_mux::theme::deepforest_ui_theme(),
        80,
        32,
    )
    .expect("valid display response should render");

    let RuntimeAgentShellDisplayOutput::Overlay(content) = output else {
        panic!("expected a display overlay");
    };
    assert!(
        content
            .lines
            .iter()
            .any(|line| UnicodeWidthStr::width(line.as_str()) > 32),
        "{content:?}"
    );
    assert!(
        content
            .lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 80),
        "{content:?}"
    );
}

/// Verifies `/show-*` Markdown is converted to physical pager rows before the
/// modal compositor sees it.
///
/// Prose, quotes, lists, links, and unbreakable tokens must honor the configured
/// cap after the selector gutter is reserved. Tables deliberately keep the
/// wider terminal body width, and rich-text source metadata must survive the
/// visual wrapping so later copy behavior does not expose presentation rows.
#[cfg(test)]
#[test]
fn show_markdown_overlay_wraps_prose_but_preserves_table_width_and_copy_source() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    let markdown = "# Detail\n\nA long prose sentence with enough words to wrap repeatedly.\n\n> quoted words also need to wrap cleanly\n\n- listed words also need to wrap cleanly\n\n[open issue](mez-agent:%2Fshow-issues%20issue-1)\n\n| Field | Value |\n| --- | --- |\n| description | alpha beta gamma delta epsilon |\n\naveryveryverylongunbreakabletoken";
    let content = runtime_agent_shell_markdown_overlay_content_for_layout(
        Some("show-issues".to_string()),
        markdown,
        &ui_theme,
        32,
        12,
    );

    let table_rows = content
        .lines
        .iter()
        .zip(&content.line_kinds)
        .filter(|(_, kind)| matches!(kind, RichTextLineKind::MarkdownTableRow))
        .map(|(line, _)| UnicodeWidthStr::width(line.as_str()))
        .collect::<Vec<_>>();
    assert!(table_rows.iter().any(|width| *width > 12), "{content:?}");
    assert!(table_rows.iter().all(|width| *width <= 30), "{content:?}");
    assert!(
        content
            .lines
            .iter()
            .zip(&content.line_kinds)
            .filter(|(_, kind)| {
                !matches!(
                    kind,
                    RichTextLineKind::MarkdownTableRow
                        | RichTextLineKind::MarkdownTableContinuation
                        | RichTextLineKind::MarkdownTableSeparator
                )
            })
            .all(|(line, _)| UnicodeWidthStr::width(line.as_str()) <= 12),
        "{content:?}"
    );
    assert!(
        content
            .selections
            .iter()
            .any(|selection| selection.command == "/show-issues issue-1"),
        "{content:?}"
    );
    assert!(
        content.line_copy_texts.iter().flatten().any(|copy_text| {
            copy_text == "A long prose sentence with enough words to wrap repeatedly."
        }),
        "{content:?}"
    );
}

/// Renders slash-command markdown display output into the command overlay
/// pager while preserving clickable `mez-agent:` links.
#[cfg(test)]
pub(crate) fn runtime_agent_shell_markdown_overlay_content(
    command: Option<String>,
    markdown: &str,
    ui_theme: &UiTheme,
) -> RuntimeCommandDisplayOverlayContent {
    runtime_agent_shell_markdown_overlay_content_for_width(command, markdown, ui_theme, None)
}

/// Renders slash-command Markdown with an optional table-layout width.
pub(crate) fn runtime_agent_shell_markdown_overlay_content_for_width(
    command: Option<String>,
    markdown: &str,
    ui_theme: &UiTheme,
    table_display_width: Option<usize>,
) -> RuntimeCommandDisplayOverlayContent {
    let mut content = RuntimeCommandDisplayOverlayContent {
        command,
        lines: Vec::new(),
        line_style_spans: Vec::new(),
        line_kinds: Vec::new(),
        line_copy_texts: Vec::new(),
        selections: Vec::new(),
    };
    for rendered in
        render_command_markdown_body_lines_for_width(markdown, ui_theme, table_display_width)
    {
        let RichTextLine {
            display,
            mut style_spans,
            copy_text,
            kind,
        } = rendered;
        let line_index = content.lines.len();
        for (start_column, width, command) in agent_command_links_in_line(&display) {
            let logical_id = content.selections.len();
            content.selections.push(OverlaySelection {
                logical_id,
                line_index,
                start_column,
                width,
                command,
                kind: OverlaySelectionKind::Primary,
            });
        }
        if let Some(copy_text) = copy_text.as_deref() {
            for (start_column, width, command) in
                agent_command_hidden_link_ranges_for_rendered_line(copy_text, &display)
            {
                let duplicate = content.selections.iter().any(|selection| {
                    selection.line_index == line_index
                        && selection.start_column == start_column
                        && selection.width == width
                        && selection.command == command
                });
                if !duplicate {
                    let logical_id = content.selections.len();
                    content.selections.push(OverlaySelection {
                        logical_id,
                        line_index,
                        start_column,
                        width,
                        command,
                        kind: OverlaySelectionKind::Primary,
                    });
                }
                push_or_extend_style_span(
                    &mut style_spans,
                    TerminalStyleSpan {
                        start: start_column,
                        length: width,
                        rendition: overlay_link_rendition(ui_theme),
                    },
                );
            }
        }
        style_spans.extend(runtime_list_themes_markdown_preview_style_spans(
            content.command.as_deref(),
            copy_text.as_deref(),
            &display,
        ));
        content.line_style_spans.push(style_spans);
        content.line_kinds.push(kind);
        content.line_copy_texts.push(copy_text);
        content.lines.push(display);
    }
    content
}

/// Renders and physically wraps command Markdown for one modal overlay body.
///
/// Prose honors the configured wrap cap after selector chrome is reserved,
/// while tables retain every remaining terminal column. The retained rich-text
/// source metadata keeps wrapping presentation-only for copy and save paths.
pub(crate) fn runtime_agent_shell_markdown_overlay_content_for_layout(
    command: Option<String>,
    markdown: &str,
    ui_theme: &UiTheme,
    terminal_width: usize,
    prose_width: usize,
) -> RuntimeCommandDisplayOverlayContent {
    let initial = runtime_agent_shell_markdown_overlay_content_for_width(
        command.clone(),
        markdown,
        ui_theme,
        Some(terminal_width.max(1)),
    );
    let available_width =
        runtime_command_overlay_available_width(terminal_width, !initial.selections.is_empty());
    let mut content = if available_width == terminal_width.max(1) {
        initial
    } else {
        runtime_agent_shell_markdown_overlay_content_for_width(
            command,
            markdown,
            ui_theme,
            Some(available_width),
        )
    };
    let prose_width = prose_width.min(available_width).max(1);
    content = wrap_runtime_command_display_overlay_content(content, prose_width, available_width);
    content
}

/// Returns preview swatch styling for Markdown-rendered `list-themes` rows.
pub(super) fn runtime_list_themes_markdown_preview_style_spans(
    command: Option<&str>,
    source_line: Option<&str>,
    display: &str,
) -> Vec<TerminalStyleSpan> {
    if !matches!(command, Some("list-themes")) {
        return Vec::new();
    }
    let Some(source_line) = source_line else {
        return Vec::new();
    };
    let cells = runtime_markdown_table_cells(source_line);
    if cells.len() < 5 {
        return Vec::new();
    }
    let preview = cells[2];
    let preview_colors = cells[4];
    let Some(preview_start) = display.find(preview) else {
        return Vec::new();
    };
    runtime_theme_preview_style_spans(
        UnicodeWidthStr::width(&display[..preview_start]),
        preview,
        Some(preview_colors),
    )
}

/// Splits one Markdown table line into trimmed cell contents.
pub(super) fn runtime_markdown_table_cells(line: &str) -> Vec<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return Vec::new();
    }
    trimmed
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

/// Runs the runtime agent shell visibility operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_agent_shell_visibility(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|parsed| {
            parsed
                .get("visibility")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
}

/// Formats a recoverable runtime error for the transient status overlay.
pub(crate) fn runtime_primary_error_status_text(line: &str) -> String {
    let normalized = line
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    if normalized.starts_with("mez error:") || normalized.starts_with("error:") {
        normalized
    } else {
        format!("mez error: {normalized}")
    }
}

/// Formats a successful command acknowledgement for the transient status overlay.
pub(crate) fn runtime_primary_notice_status_text(line: &str) -> String {
    let normalized = line
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    if normalized.starts_with("mez:") {
        normalized
    } else {
        format!("mez: {normalized}")
    }
}

/// Returns the agent command link at one rendered line column.
pub(crate) fn agent_command_link_at_line_column(line: &str, column: usize) -> Option<String> {
    agent_command_links_in_line(line)
        .into_iter()
        .find(|(start_column, width, _command)| {
            column >= *start_column && column < start_column.saturating_add(*width)
        })
        .map(|(_, _, command)| command)
}

/// Returns visible agent command link ranges in one rendered line.
pub(crate) fn agent_command_links_in_line(line: &str) -> Vec<(usize, usize, String)> {
    let scheme = "mez-agent:";
    let mut search_start = 0;
    let mut links = Vec::new();
    while let Some(relative_start) = line[search_start..].find(scheme) {
        let scheme_start = search_start.saturating_add(relative_start);
        let encoded_start = scheme_start.saturating_add(scheme.len());
        let encoded_end = line[encoded_start..]
            .find(|ch: char| ch == ')' || ch.is_whitespace())
            .map(|end| encoded_start.saturating_add(end))
            .unwrap_or(line.len());
        let Some(command) = percent_decode_agent_command(&line[encoded_start..encoded_end]) else {
            search_start = encoded_end;
            continue;
        };
        let destination_start_column = UnicodeWidthStr::width(&line[..scheme_start]);
        let destination_end_column = UnicodeWidthStr::width(&line[..encoded_end]);
        let label_clicked = command
            .strip_prefix("/resume ")
            .and_then(|session_id| {
                line[..scheme_start]
                    .rfind(session_id)
                    .map(|label_start| (label_start, session_id))
            })
            .map(|(label_start, session_id)| {
                let start_column = UnicodeWidthStr::width(&line[..label_start]);
                let width = UnicodeWidthStr::width(session_id);
                (start_column, width)
            });
        if let Some((start_column, width)) = label_clicked {
            links.push((start_column, width, command));
        } else {
            links.push((
                destination_start_column,
                destination_end_column.saturating_sub(destination_start_column),
                command.clone(),
            ));
        }
        search_start = encoded_end;
    }
    links
}

/// Returns source-aligned hidden `mez-agent:` link ranges for one rendered row.
pub(crate) fn agent_command_hidden_link_ranges_for_rendered_line(
    source_line: &str,
    display: &str,
) -> Vec<(usize, usize, String)> {
    mez_mux::render::markdown_link_display_ranges(
        source_line,
        display,
        agent_command_link_destination,
    )
}

/// Decodes one `mez-agent:` markdown destination into an executable command.
pub(crate) fn agent_command_link_destination(destination: &str) -> Option<String> {
    let encoded = destination.strip_prefix("mez-agent:")?;
    let command = percent_decode_agent_command(encoded)?;
    (!command.is_empty()).then_some(command)
}

/// Percent-decodes a markdown command link destination.
pub(crate) fn percent_decode_agent_command(encoded: &str) -> Option<String> {
    let mut output = Vec::with_capacity(encoded.len());
    let bytes = encoded.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index.saturating_add(1))?)?;
            let low = hex_value(*bytes.get(index.saturating_add(2))?)?;
            output.push(high.saturating_mul(16).saturating_add(low));
            index = index.saturating_add(3);
        } else {
            output.push(bytes[index]);
            index = index.saturating_add(1);
        }
    }
    String::from_utf8(output).ok()
}

/// Decodes one ASCII hexadecimal digit.
pub(crate) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
