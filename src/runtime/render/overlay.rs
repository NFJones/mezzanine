//! Runtime command overlay and pane-agent selector helpers.
//!
//! This module owns command-display overlay parsing, selectable command/link
//! rendering, overlay scrolling/style composition, and pane-agent status selector
//! placement. Keeping these pure presentation helpers outside the runtime render
//! facade makes overlay behavior easier to maintain without mixing it with pane
//! input dispatch and frame composition.

use super::*;
use crate::terminal::parse_hex_color;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Render placement for an open pane agent status selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneAgentStatusSelectorLayout {
    /// Zero-based column where selector rows begin.
    pub(super) column: u16,
    /// Width in terminal cells reserved for selector rows.
    pub(super) width: u16,
    /// Visible selector items with their rendered rows.
    pub(super) visible_items: Vec<PaneAgentStatusSelectorLayoutItem>,
}

/// Render placement for one visible selector item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaneAgentStatusSelectorLayoutItem {
    /// Index into the selector item list.
    pub(super) item_index: usize,
    /// Zero-based terminal row where this item is drawn.
    pub(super) row: u16,
}

/// Maximum number of model/reasoning picker rows shown at once.
pub(super) const PANE_AGENT_STATUS_SELECTOR_MAX_ROWS: usize = 30;
/// Selector marker shown in front of the active command-output overlay row.
const DISPLAY_OVERLAY_ACTIVE_SELECTOR: &str = "> ";
/// Placeholder marker shown in front of inactive selectable overlay rows.
const DISPLAY_OVERLAY_INACTIVE_SELECTOR: &str = "  ";
/// Returns a compact MCP server state label for command completion details.
pub(super) fn agent_shell_mcp_display_state_name(
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
pub(super) fn default_runtime_agent_prompt_input() -> RuntimeAgentPromptInput {
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
pub(super) fn runtime_primary_prompt_input(
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
pub(super) enum RuntimeAgentShellDisplayOutput {
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
pub(super) fn runtime_agent_shell_display_output(
    body: &str,
    ui_theme: &UiTheme,
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
            let content =
                runtime_agent_shell_markdown_overlay_content(command.clone(), body, ui_theme);
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
fn runtime_agent_shell_suppressed_mutation_command_name(command: &str) -> bool {
    matches!(command, "clear" | "new" | "prompt")
}

/// Returns true for slash-command displays that should not enter pane logs.
fn runtime_agent_shell_transient_display_command_name(command: &str) -> bool {
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

/// Renders slash-command markdown display output into the command overlay
/// pager while preserving clickable `mez-agent:` links.
pub(super) fn runtime_agent_shell_markdown_overlay_content(
    command: Option<String>,
    markdown: &str,
    ui_theme: &UiTheme,
) -> RuntimeCommandDisplayOverlayContent {
    let mut content = RuntimeCommandDisplayOverlayContent {
        command,
        lines: Vec::new(),
        line_style_spans: Vec::new(),
        selections: Vec::new(),
    };
    for rendered in render_command_markdown_body_lines(markdown, ui_theme) {
        let AgentRenderedLine {
            display,
            mut style_spans,
            copy_text,
            ..
        } = rendered;
        let line_index = content.lines.len();
        for (start_column, width, command) in agent_command_links_in_line(&display) {
            content.selections.push(RuntimeDisplayOverlaySelection {
                line_index,
                start_column,
                width,
                command,
                kind: RuntimeDisplayOverlaySelectionKind::Primary,
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
                    content.selections.push(RuntimeDisplayOverlaySelection {
                        line_index,
                        start_column,
                        width,
                        command,
                        kind: RuntimeDisplayOverlaySelectionKind::Primary,
                    });
                }
                push_or_extend_style_span(
                    &mut style_spans,
                    TerminalStyleSpan {
                        start: start_column,
                        length: width,
                        rendition: runtime_display_overlay_link_rendition(ui_theme),
                    },
                );
            }
        }
        content.line_style_spans.push(style_spans);
        content.lines.push(display);
    }
    content
}

/// Runs the runtime agent shell visibility operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_shell_visibility(body: &str) -> Option<String> {
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
pub(super) fn runtime_primary_error_status_text(line: &str) -> String {
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
pub(super) fn runtime_primary_notice_status_text(line: &str) -> String {
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
pub(super) fn agent_command_link_at_line_column(line: &str, column: usize) -> Option<String> {
    agent_command_links_in_line(line)
        .into_iter()
        .find(|(start_column, width, _command)| {
            column >= *start_column && column < start_column.saturating_add(*width)
        })
        .map(|(_, _, command)| command)
}

/// Returns visible agent command link ranges in one rendered line.
pub(super) fn agent_command_links_in_line(line: &str) -> Vec<(usize, usize, String)> {
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
        if !command.starts_with('/') {
            search_start = encoded_end;
            continue;
        }
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
pub(super) fn agent_command_hidden_link_ranges_for_rendered_line(
    source_line: &str,
    display: &str,
) -> Vec<(usize, usize, String)> {
    let mut links = Vec::new();
    let mut source_cursor = 0usize;
    let mut display_cursor = 0usize;
    let mut active_link: Option<(String, Option<usize>)> = None;
    for event in Parser::new_ext(source_line, Options::all()) {
        match event {
            Event::Start(Tag::Link { dest_url, .. })
                if agent_command_link_destination(&dest_url).is_some() =>
            {
                active_link = Some((dest_url.to_string(), None));
            }
            Event::Text(text) | Event::Code(text) => {
                let text = text.as_ref();
                let Some(relative_start) = source_line[source_cursor..].find(text) else {
                    continue;
                };
                source_cursor = source_cursor
                    .saturating_add(relative_start)
                    .saturating_add(text.len());
                let Some(relative_display_start) = display[display_cursor..].find(text) else {
                    continue;
                };
                let absolute_display_start = display_cursor.saturating_add(relative_display_start);
                if let Some((_, display_start)) = active_link.as_mut()
                    && display_start.is_none()
                {
                    *display_start = Some(absolute_display_start);
                }
                display_cursor = display_cursor
                    .saturating_add(relative_display_start)
                    .saturating_add(text.len());
            }
            Event::End(TagEnd::Link) => {
                if let Some((destination, Some(display_start))) = active_link.take()
                    && display_cursor > display_start
                    && let Some(command) = agent_command_link_destination(&destination)
                {
                    let start_column = UnicodeWidthStr::width(&display[..display_start]);
                    let width = UnicodeWidthStr::width(&display[display_start..display_cursor]);
                    links.push((start_column, width, command));
                }
            }
            _ => {}
        }
    }
    links
}

/// Decodes one `mez-agent:` markdown destination into an executable command.
pub(super) fn agent_command_link_destination(destination: &str) -> Option<String> {
    let encoded = destination.strip_prefix("mez-agent:")?;
    let command = percent_decode_agent_command(encoded)?;
    command.starts_with('/').then_some(command)
}

/// Percent-decodes a markdown command link destination.
pub(super) fn percent_decode_agent_command(encoded: &str) -> Option<String> {
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
pub(super) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Display lines and selectable actions derived from command JSON output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeCommandDisplayOverlayContent {
    /// Terminal command that produced these display lines, when present.
    pub(super) command: Option<String>,
    /// Human-readable lines rendered in the command display overlay.
    pub(super) lines: Vec<String>,
    /// Visible terminal styles for each rendered display line.
    pub(super) line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Optional command actions keyed by line index.
    pub(super) selections: Vec<RuntimeDisplayOverlaySelection>,
}

/// One rendered command-overlay display line with selectable choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeDisplayLine {
    /// Human-readable text shown in the overlay.
    pub(super) text: String,
    /// Interactive choices rendered inside `text`.
    pub(super) choices: Vec<RuntimeDisplayChoicePlacement>,
    /// Visible terminal styles applied to `text`.
    pub(super) style_spans: Vec<TerminalStyleSpan>,
}

/// One selectable choice and its location in a display line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeDisplayChoicePlacement {
    /// Zero-based display column where the choice starts.
    pub(super) start_column: usize,
    /// Display-cell width of the choice label.
    pub(super) width: usize,
    /// Human-readable label shown to the user.
    pub(super) label: String,
    /// Terminal command executed by this choice.
    pub(super) command: String,
    /// Visual importance of this choice.
    pub(super) kind: RuntimeDisplayOverlaySelectionKind,
}

/// One parsed executable display choice before it has a line position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeDisplayChoice {
    /// Human-readable label shown to the user.
    pub(super) label: String,
    /// Terminal command executed by this choice.
    pub(super) command: String,
    /// Visual importance of this choice.
    pub(super) kind: RuntimeDisplayOverlaySelectionKind,
}

/// Parses command JSON output into human-readable overlay content.
pub(super) fn runtime_command_display_overlay_content(
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
fn terminal_command_display_body_is_markdown(command: Option<&str>, body: &str) -> bool {
    matches!(command, Some("help")) && body.trim_start().starts_with('#')
}

/// Returns whether a terminal command response needs the modal display overlay.
pub(super) fn runtime_command_display_should_open_overlay(
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
pub(super) fn runtime_command_display_transient_status_line(
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
pub(super) fn runtime_transient_terminal_command_name(command: &str) -> bool {
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
pub(super) fn runtime_immediate_terminal_command_name(command: &str) -> bool {
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
pub(super) fn runtime_human_readable_display_lines(body: &str) -> Vec<String> {
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
pub(super) fn runtime_human_readable_display_line_with_choices(
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
pub(super) fn runtime_custom_human_readable_display_line(
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
pub(super) fn runtime_agent_say_copy_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
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
pub(super) fn runtime_copy_destination_display(record: &RuntimeDisplayRecord) -> String {
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
pub(super) fn runtime_agent_fork_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
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
pub(super) fn runtime_routing_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
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
pub(super) fn runtime_policy_sentence(record: &RuntimeDisplayRecord) -> Option<String> {
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
pub(super) fn runtime_enabled_phrase(value: &str) -> &'static str {
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
                    self.selections.push(RuntimeDisplayOverlaySelection {
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

/// Structured representation of one compact display row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeDisplayRecord {
    /// Leading non-key fields, such as an index or key-binding notation.
    prefix: Vec<String>,
    /// Parsed key/value fields from the display row.
    pub(super) fields: Vec<(String, String)>,
}

impl RuntimeDisplayRecord {
    /// Parses a colon-delimited `key=value:key=value` style display row.
    fn parse_colon_delimited(line: &str) -> Option<Self> {
        if !line.contains('=') || !line.contains(':') {
            return None;
        }
        let mut record = Self {
            prefix: Vec::new(),
            fields: Vec::new(),
        };
        for segment in line.split(':') {
            record.push_segment(segment.trim());
        }
        record.has_fields().then_some(record)
    }

    /// Parses a whitespace-delimited `key=value key=value` style display row.
    fn parse_space_delimited(line: &str) -> Option<Self> {
        if !line.contains('=') {
            return None;
        }
        let fields = line
            .split_whitespace()
            .map(runtime_parse_display_field)
            .collect::<Option<Vec<_>>>()?;
        (!fields.is_empty()).then_some(Self {
            prefix: Vec::new(),
            fields,
        })
    }

    /// Adds one colon-delimited segment to this record.
    fn push_segment(&mut self, segment: &str) {
        if segment.is_empty() {
            return;
        }
        if let Some(field) = runtime_parse_display_field(segment) {
            self.fields.push(field);
        } else if let Some((_, value)) = self.fields.last_mut() {
            value.push(':');
            value.push_str(segment);
        } else {
            self.prefix.push(segment.to_string());
        }
    }

    /// Returns true when this record has at least one key/value field.
    fn has_fields(&self) -> bool {
        !self.fields.is_empty()
    }

    /// Formats this record as a readable display line with action chips.
    fn into_display_line(self) -> RuntimeDisplayLine {
        let choices = self.choices();
        let has_choices = !choices.is_empty();
        let mut text = String::new();
        let mut placements = Vec::new();
        let mut style_spans = Vec::new();
        if has_choices {
            text.push_str("actions: ");
            for choice in choices {
                if !placements.is_empty() {
                    text.push(' ');
                }
                let chip = format!("[{}]", choice.label);
                let start_column = UnicodeWidthStr::width(text.as_str());
                let width = UnicodeWidthStr::width(chip.as_str());
                text.push_str(&chip);
                placements.push(RuntimeDisplayChoicePlacement {
                    start_column,
                    width,
                    label: choice.label,
                    command: choice.command,
                    kind: choice.kind,
                });
            }
        }
        let mut append_part = |part: String| {
            if !text.is_empty() {
                text.push_str(" | ");
            }
            let start = UnicodeWidthStr::width(text.as_str());
            text.push_str(&part);
            start
        };
        if !self.prefix.is_empty() {
            append_part(self.prefix.join(" "));
        }
        for (key, value) in &self.fields {
            if self.choice_field_is_consumed(key, value, has_choices) {
                continue;
            }
            let label = format!("{}: ", runtime_display_field_label(key));
            let display_value = runtime_display_field_value(value);
            let part_start = append_part(format!("{label}{display_value}"));
            if key == "preview" {
                style_spans.extend(runtime_theme_preview_style_spans(
                    part_start.saturating_add(UnicodeWidthStr::width(label.as_str())),
                    display_value.as_str(),
                    self.field_value("preview_colors"),
                ));
            }
        }
        RuntimeDisplayLine {
            text,
            choices: placements,
            style_spans,
        }
    }

    /// Returns executable choices encoded by this row.
    fn choices(&self) -> Vec<RuntimeDisplayChoice> {
        let mut choices = Vec::new();
        for command in self
            .field_values("commands")
            .flat_map(|value| runtime_split_display_commands(value, '|'))
        {
            runtime_push_unique_display_choice(&mut choices, command);
        }
        for key in ["select_command", "command_action", "action"] {
            for value in self.field_values(key) {
                runtime_push_unique_display_choice(&mut choices, value);
            }
        }
        for value in self.field_values("actions") {
            for command in runtime_split_display_commands(value, ',') {
                runtime_push_unique_display_choice(&mut choices, command);
            }
        }
        choices
    }

    /// Returns all values for one field key.
    fn field_values(&self, key: &str) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .filter(move |(field_key, _)| field_key == key)
            .map(|(_, value)| value.as_str())
    }

    /// Returns the first value for one display field key.
    fn field_value(&self, key: &str) -> Option<&str> {
        self.field_values(key).next()
    }

    /// Returns whether a field was used as selector metadata.
    fn choice_field_is_consumed(&self, key: &str, value: &str, has_choices: bool) -> bool {
        match key {
            "commands" | "select_command" | "command_action" => has_choices,
            "actions" => has_choices,
            "action" => runtime_display_executable_choice(value).is_some(),
            "preview_colors" => self.field_value("preview").is_some(),
            _ => false,
        }
    }
}

/// Splits a compact display choice field into command candidates.
pub(super) fn runtime_split_display_commands(
    value: &str,
    separator: char,
) -> impl Iterator<Item = &str> {
    value
        .split(separator)
        .map(str::trim)
        .filter(|command| !command.is_empty() && *command != "none")
}

/// Pushes one executable choice if it is not already present.
pub(super) fn runtime_push_unique_display_choice(
    choices: &mut Vec<RuntimeDisplayChoice>,
    command: &str,
) {
    let Some(choice) = runtime_display_executable_choice(command) else {
        return;
    };
    if choices
        .iter()
        .any(|existing| existing.command == choice.command)
    {
        return;
    }
    choices.push(choice);
}

/// Converts a command string into a selectable display choice when valid.
pub(super) fn runtime_display_executable_choice(command: &str) -> Option<RuntimeDisplayChoice> {
    let command = command.trim();
    let invocations = parse_command_sequence(command).ok()?;
    let first = invocations.first()?;
    if invocations.len() != 1 {
        return None;
    }
    if !runtime_display_is_known_command(&first.name) {
        return None;
    }
    Some(RuntimeDisplayChoice {
        label: runtime_display_choice_label(&first.name),
        command: command.to_string(),
        kind: runtime_display_choice_kind(&first.name),
    })
}

/// Returns whether a command name is part of the Mez terminal command set.
pub(super) fn runtime_display_is_known_command(command_name: &str) -> bool {
    baseline_commands()
        .iter()
        .any(|command| command.name == command_name)
}

/// Returns a concise action label for one command name.
pub(super) fn runtime_display_choice_label(command_name: &str) -> String {
    match command_name {
        "select-window" | "select-group" | "select-pane" | "select-layout" => "select",
        "detach-client" => "detach",
        "approve-observer" => "approve",
        "reject-observer" => "reject",
        "revoke-observer" => "revoke",
        "paste-buffer" | "paste-clipboard" => "paste",
        "delete-buffer" => "delete",
        "copy-selection" => "copy",
        other => other.split('-').next().unwrap_or(other),
    }
    .to_string()
}

/// Returns the themed visual category for one command name.
pub(super) fn runtime_display_choice_kind(
    command_name: &str,
) -> RuntimeDisplayOverlaySelectionKind {
    match command_name {
        "delete-buffer" | "detach-client" | "reject-observer" | "revoke-observer" | "kill-pane"
        | "kill-window" | "kill-group" | "kill-session" => {
            RuntimeDisplayOverlaySelectionKind::Danger
        }
        "paste-buffer" | "paste-clipboard" | "copy-selection" => {
            RuntimeDisplayOverlaySelectionKind::Secondary
        }
        _ => RuntimeDisplayOverlaySelectionKind::Primary,
    }
}

/// Parses one `key=value` display field.
pub(super) fn runtime_parse_display_field(segment: &str) -> Option<(String, String)> {
    let (key, value) = segment.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

/// Returns a lowercase human-readable label for a compact display field name.
pub(super) fn runtime_display_field_label(key: &str) -> String {
    key.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns a readable value for common compact display values.
pub(super) fn runtime_display_field_value(value: &str) -> String {
    match value {
        "true" => "yes".to_string(),
        "false" => "no".to_string(),
        "none" => "none".to_string(),
        _ => value.to_string(),
    }
}

/// Returns per-block color spans for one theme preview field.
fn runtime_theme_preview_style_spans(
    start_column: usize,
    preview: &str,
    preview_colors: Option<&str>,
) -> Vec<TerminalStyleSpan> {
    let colors = preview_colors
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter_map(|value| parse_hex_color(value.trim()))
        .collect::<Vec<_>>();
    let mut spans = Vec::new();
    let mut column = start_column;
    for (index, ch) in preview.chars().enumerate() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if ch.is_whitespace() {
            column = column.saturating_add(width);
            continue;
        }
        let Some(color) = colors.get(index) else {
            column = column.saturating_add(width);
            continue;
        };
        push_or_extend_style_span(
            &mut spans,
            TerminalStyleSpan {
                start: column,
                length: width,
                rendition: GraphicRendition {
                    foreground: Some(*color),
                    ..GraphicRendition::default()
                },
            },
        );
        column = column.saturating_add(width);
    }
    spans
}

/// Returns the rendered line index for the active overlay selection.
pub(super) fn runtime_display_overlay_active_line_index(
    overlay: &RuntimeDisplayOverlay,
) -> Option<usize> {
    overlay
        .active_selection_index
        .and_then(|index| overlay.selections.get(index))
        .map(|selection| selection.line_index)
}

/// Keeps a target overlay line within the modal page.
pub(super) fn runtime_scroll_display_overlay_to_line(
    overlay: &mut RuntimeDisplayOverlay,
    line_index: usize,
    client_size: Size,
) {
    let page_rows = modal_display_overlay_page_rows(client_size).max(1);
    if line_index < overlay.scroll_offset {
        overlay.scroll_offset = line_index;
    } else if line_index >= overlay.scroll_offset.saturating_add(page_rows) {
        overlay.scroll_offset = line_index.saturating_add(1).saturating_sub(page_rows);
    }
    overlay.scroll_offset = overlay.scroll_offset.min(modal_display_overlay_max_scroll(
        &overlay.lines,
        client_size,
    ));
}

/// Clamps overlay scrolling to the visible content range for the client size.
pub(super) fn runtime_clamp_display_overlay_scroll(
    overlay: &mut RuntimeDisplayOverlay,
    client_size: Size,
) {
    overlay.scroll_offset = overlay.scroll_offset.min(modal_display_overlay_max_scroll(
        &overlay.lines,
        client_size,
    ));
}

/// Returns display overlay lines with selector markers on actionable rows.
pub(super) fn runtime_display_overlay_render_lines(overlay: &RuntimeDisplayOverlay) -> Vec<String> {
    let active_line = runtime_display_overlay_active_line_index(overlay);
    overlay
        .lines
        .iter()
        .enumerate()
        .map(|(line_index, line)| {
            if active_line == Some(line_index) {
                format!("{DISPLAY_OVERLAY_ACTIVE_SELECTOR}{line}")
            } else if overlay
                .selections
                .iter()
                .any(|selection| selection.line_index == line_index)
            {
                format!("{DISPLAY_OVERLAY_INACTIVE_SELECTOR}{line}")
            } else {
                line.to_string()
            }
        })
        .collect()
}

/// Returns true when a display overlay line owns at least one choice.
pub(super) fn runtime_display_overlay_line_has_selection(
    overlay: &RuntimeDisplayOverlay,
    line_index: usize,
) -> bool {
    overlay
        .selections
        .iter()
        .any(|selection| selection.line_index == line_index)
}

/// Returns the rendered start column after selector gutters are added.
pub(super) fn runtime_display_overlay_rendered_selection_start(
    overlay: &RuntimeDisplayOverlay,
    selection: &RuntimeDisplayOverlaySelection,
) -> usize {
    selection.start_column
        + runtime_display_overlay_line_prefix_columns(overlay, selection.line_index)
}

/// Returns the terminal-cell width occupied by one rendered overlay row gutter.
pub(super) fn runtime_display_overlay_line_prefix_columns(
    overlay: &RuntimeDisplayOverlay,
    line_index: usize,
) -> usize {
    usize::from(runtime_display_overlay_line_has_selection(
        overlay, line_index,
    )) * runtime_display_overlay_selection_prefix_columns()
}

/// Returns the terminal-cell width occupied by selectable overlay row gutters.
pub(super) fn runtime_display_overlay_selection_prefix_columns() -> usize {
    UnicodeWidthStr::width(DISPLAY_OVERLAY_ACTIVE_SELECTOR)
}

/// Returns the modal overlay footer text for the active overlay.
pub(super) fn runtime_display_overlay_footer(overlay: &RuntimeDisplayOverlay) -> String {
    if let Some(input) = overlay.search_input.as_deref() {
        format!("/{input}")
    } else if let Some(status) = overlay.search_status.as_deref() {
        status.to_string()
    } else if overlay.selections.is_empty() {
        "esc: return | /: search | up/down pgup/pgdn home/end".to_string()
    } else {
        "esc: return | /: search | enter: select | arrows: choose | pgup/pgdn: scroll".to_string()
    }
}

/// Returns the themed choice style for a command-overlay selection.
pub(super) fn runtime_display_overlay_selection_rendition(
    ui_theme: &UiTheme,
    kind: RuntimeDisplayOverlaySelectionKind,
    active: bool,
) -> GraphicRendition {
    let pair = match kind {
        RuntimeDisplayOverlaySelectionKind::Primary => ui_theme.colors.agent_model,
        RuntimeDisplayOverlaySelectionKind::Secondary => ui_theme.colors.agent_reasoning,
        RuntimeDisplayOverlaySelectionKind::Danger => ui_theme.colors.agent_status_failed,
    };
    let mut rendition = GraphicRendition {
        foreground: Some(pair.foreground),
        ..GraphicRendition::default()
    };
    rendition.bold = true;
    rendition.underline = true;
    rendition.inverse = false;
    rendition.background = None;
    rendition.dim = false;
    if active {
        rendition.background = Some(pair.background);
    }
    rendition
}
/// Returns the selector-gutter rendition for a selectable overlay row.
///
/// The gutter marks the active row, but it is not part of the selectable body
/// range. Keep the selector glyph itself unstyled so active link treatment
/// begins only on the first body cell; otherwise front-of-line `/resume` links
/// visibly shift left into the selector prefix even when the body/background
/// math is correct.
pub(super) fn runtime_display_overlay_selection_gutter_rendition(
    _ui_theme: &UiTheme,
    _kind: RuntimeDisplayOverlaySelectionKind,
) -> GraphicRendition {
    GraphicRendition::default()
}
/// Returns the markdown-style rendition used for command-overlay links.
pub(super) fn runtime_display_overlay_link_rendition(ui_theme: &UiTheme) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(ui_theme.colors.agent_transcript_command.foreground),
        bold: true,
        underline: true,
        inverse: false,
        background: None,
        ..GraphicRendition::default()
    }
}
/// Returns the shifted, clipped markdown/body spans for one overlay line.
pub(super) fn runtime_display_overlay_body_style_spans(
    overlay: &RuntimeDisplayOverlay,
    line_index: usize,
    max_columns: usize,
) -> Vec<TerminalStyleSpan> {
    let prefix_columns = runtime_display_overlay_line_prefix_columns(overlay, line_index);
    let visible_columns = max_columns.saturating_sub(prefix_columns);
    overlay
        .line_style_spans
        .get(line_index)
        .into_iter()
        .flatten()
        .filter_map(|span| clipped_overlay_style_span(*span, prefix_columns, visible_columns))
        .collect()
}
/// Appends one selection rendition only where later body spans do not apply.
pub(super) fn append_uncovered_overlay_selection_span(
    spans: &mut Vec<TerminalStyleSpan>,
    selection_start: usize,
    selection_length: usize,
    rendition: GraphicRendition,
    occupied_spans: &[TerminalStyleSpan],
) {
    let selection_end = selection_start.saturating_add(selection_length);
    if selection_start >= selection_end {
        return;
    }
    let mut occupied_ranges: Vec<(usize, usize)> = occupied_spans
        .iter()
        .filter_map(|span| {
            let span_start = span.start.max(selection_start);
            let span_end = span.start.saturating_add(span.length).min(selection_end);
            (span_start < span_end).then_some((span_start, span_end))
        })
        .collect();
    occupied_ranges.sort_unstable_by_key(|(start, _)| *start);
    let mut cursor = selection_start;
    for (occupied_start, occupied_end) in occupied_ranges {
        if cursor < occupied_start {
            push_or_extend_style_span(
                spans,
                TerminalStyleSpan {
                    start: cursor,
                    length: occupied_start.saturating_sub(cursor),
                    rendition,
                },
            );
        }
        cursor = cursor.max(occupied_end);
        if cursor >= selection_end {
            return;
        }
    }
    push_or_extend_style_span(
        spans,
        TerminalStyleSpan {
            start: cursor,
            length: selection_end.saturating_sub(cursor),
            rendition,
        },
    );
}

/// Appends one style span without coalescing it into an adjacent span.
///
/// Overlay selection gutters must remain a standalone cell so later body or
/// fallback selection styling cannot visually absorb the gutter when adjacent
/// rendered spans share the same rendition.
fn push_style_span_without_coalescing(spans: &mut Vec<TerminalStyleSpan>, span: TerminalStyleSpan) {
    if span.length == 0 {
        return;
    }
    spans.push(span);
}

/// Appends active-selection backgrounds over body spans inside a selected range.
fn append_active_overlay_body_selection_spans(
    spans: &mut Vec<TerminalStyleSpan>,
    selection_start: usize,
    selection_length: usize,
    selection_rendition: GraphicRendition,
    body_spans: &[TerminalStyleSpan],
) {
    let selection_end = selection_start.saturating_add(selection_length);
    if selection_start >= selection_end {
        return;
    }
    for body_span in body_spans {
        let body_start = body_span.start.max(selection_start);
        let body_end = body_span
            .start
            .saturating_add(body_span.length)
            .min(selection_end);
        if body_start >= body_end {
            continue;
        }
        let mut rendition = body_span.rendition;
        rendition.background = selection_rendition.background;
        if rendition.foreground.is_none() {
            rendition.foreground = selection_rendition.foreground;
        }
        push_style_span_without_coalescing(
            spans,
            TerminalStyleSpan {
                start: body_start,
                length: body_end.saturating_sub(body_start),
                rendition,
            },
        );
    }
}
/// Returns the fully composed style spans for one rendered overlay line.
pub(super) fn runtime_display_overlay_rendered_line_style_spans(
    overlay: &RuntimeDisplayOverlay,
    line_index: usize,
    max_columns: usize,
    ui_theme: &UiTheme,
) -> Vec<TerminalStyleSpan> {
    let body_spans = runtime_display_overlay_body_style_spans(overlay, line_index, max_columns);
    let prefix_columns = runtime_display_overlay_line_prefix_columns(overlay, line_index);
    let mut spans = Vec::new();
    let search_span = overlay.search_match.and_then(|search_match| {
        if search_match.line_index != line_index || search_match.width == 0 {
            return None;
        }
        let start = prefix_columns.saturating_add(search_match.start_column);
        if start >= max_columns {
            return None;
        }
        Some(TerminalStyleSpan {
            start,
            length: search_match.width.min(max_columns.saturating_sub(start)),
            rendition: ui_theme.colors.copy_selection.rendition(),
        })
    });
    for (selection_index, selection) in overlay.selections.iter().enumerate() {
        if selection.line_index != line_index {
            continue;
        }
        let active = overlay.active_selection_index == Some(selection_index);
        let start = runtime_display_overlay_rendered_selection_start(overlay, selection);
        if start < max_columns && selection.width > 0 {
            append_uncovered_overlay_selection_span(
                &mut spans,
                start,
                selection.width.min(max_columns.saturating_sub(start)),
                runtime_display_overlay_selection_rendition(ui_theme, selection.kind, active),
                &body_spans,
            );
        }
        if active {
            push_style_span_without_coalescing(
                &mut spans,
                TerminalStyleSpan {
                    start: 0,
                    length: prefix_columns.min(max_columns),
                    rendition: runtime_display_overlay_selection_gutter_rendition(
                        ui_theme,
                        selection.kind,
                    ),
                },
            );
        }
    }
    for span in &body_spans {
        push_or_extend_style_span(&mut spans, *span);
    }
    for (selection_index, selection) in overlay.selections.iter().enumerate() {
        if selection.line_index != line_index
            || overlay.active_selection_index != Some(selection_index)
        {
            continue;
        }
        let start = runtime_display_overlay_rendered_selection_start(overlay, selection);
        if start < max_columns && selection.width > 0 {
            append_active_overlay_body_selection_spans(
                &mut spans,
                start,
                selection.width.min(max_columns.saturating_sub(start)),
                runtime_display_overlay_selection_rendition(ui_theme, selection.kind, true),
                &body_spans,
            );
        }
    }
    if let Some(search_span) = search_span {
        push_or_extend_style_span(&mut spans, search_span);
    }
    append_display_overlay_mouse_selection_spans(
        &mut spans,
        overlay.mouse_selection,
        line_index,
        prefix_columns,
        max_columns,
        ui_theme.colors.copy_selection.rendition(),
    );
    spans
}

/// Appends copy-selection style spans for one rendered overlay content row.
fn append_display_overlay_mouse_selection_spans(
    spans: &mut Vec<TerminalStyleSpan>,
    selection: Option<(CopyPosition, CopyPosition)>,
    line_index: usize,
    prefix_columns: usize,
    max_columns: usize,
    rendition: GraphicRendition,
) {
    let Some((start, end)) = selection else {
        return;
    };
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if line_index < start.line || line_index > end.line {
        return;
    }
    let content_start = if line_index == start.line {
        start.column
    } else {
        0
    };
    let content_end = if line_index == end.line {
        end.column
    } else {
        max_columns.saturating_sub(prefix_columns)
    };
    let rendered_start = prefix_columns
        .saturating_add(content_start)
        .min(max_columns);
    let rendered_end = prefix_columns.saturating_add(content_end).min(max_columns);
    if rendered_start >= rendered_end {
        return;
    }
    push_or_extend_style_span(
        spans,
        TerminalStyleSpan {
            start: rendered_start,
            length: rendered_end.saturating_sub(rendered_start),
            rendition,
        },
    );
}

/// Computes terminal placement for a pane agent model/reasoning selector.
pub(super) fn runtime_pane_agent_status_selector_layout(
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

/// Adjusts selector scroll so keyboard-selected rows stay reachable.
pub(super) fn runtime_pane_agent_status_selector_keep_active_visible(
    selector: &mut RuntimePaneAgentStatusSelector,
    visible_rows: usize,
) {
    let visible_rows = visible_rows.max(1);
    let max_offset = selector.items.len().saturating_sub(visible_rows);
    if selector.active_index < selector.scroll_offset {
        selector.scroll_offset = selector.active_index;
    } else if selector.active_index >= selector.scroll_offset.saturating_add(visible_rows) {
        selector.scroll_offset = selector
            .active_index
            .saturating_add(1)
            .saturating_sub(visible_rows);
    }
    selector.scroll_offset = selector.scroll_offset.min(max_offset);
}

/// Builds one padded selector row clipped to the available terminal width.
pub(super) fn runtime_selector_line(marker: &str, value: &str, width: usize) -> String {
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

impl RuntimeSessionService {
    /// Executes one command selected from the primary display overlay.
    pub(super) fn execute_primary_display_overlay_selection_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        command: &str,
    ) -> Result<bool> {
        self.primary_display_overlay = None;
        if command.trim_start().starts_with('/') {
            let pane_id = self.active_pane_id()?.to_string();
            let body = self.execute_agent_shell_command(primary_client_id, command)?;
            let display_output = runtime_agent_shell_display_output(&body, &self.ui_theme)?;
            self.set_agent_prompt_display_output(&pane_id, display_output)?;
            if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
                self.agent_prompt_inputs.remove(&pane_id);
            }
            return Ok(true);
        }
        let content = self
            .execute_terminal_command(primary_client_id, command)
            .and_then(|body| runtime_command_display_overlay_content(&body, &self.ui_theme))?;
        self.present_runtime_command_display_content(content)?;
        Ok(true)
    }

    /// Applies mouse-wheel scrolling to the primary display overlay.
    pub(super) fn apply_primary_display_overlay_scroll(&mut self, lines: isize) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        Ok(apply_display_overlay_scroll_delta(
            overlay,
            lines,
            self.session.authoritative_size,
        ))
    }

    /// Runs the apply primary display overlay input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_primary_display_overlay_input(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &[u8],
    ) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_ref() else {
            return Ok(false);
        };
        if overlay.dismiss_on_any_input && !input.is_empty() {
            self.primary_display_overlay = None;
            return Ok(true);
        }
        if overlay.search_input.is_some() {
            return self.apply_primary_display_overlay_search_input(input);
        }
        match runtime_display_overlay_input_action(input) {
            RuntimeDisplayOverlayInputAction::Exit => {
                self.primary_display_overlay = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::StartSearch => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                overlay.search_input = Some(String::new());
                overlay.search_status = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::EditSearchText
            | RuntimeDisplayOverlayInputAction::EditSearchBackspace => Ok(false),
            RuntimeDisplayOverlayInputAction::SelectActive => {
                let size = self.session.authoritative_size;
                let command = self
                    .primary_display_overlay
                    .as_ref()
                    .and_then(|overlay| {
                        let index = overlay.active_selection_index?;
                        runtime_display_overlay_selection_index_is_visible(overlay, index, size)
                            .then(|| overlay.selections.get(index))
                            .flatten()
                    })
                    .map(|selection| selection.command.clone());
                if let Some(command) = command {
                    self.execute_primary_display_overlay_selection_command(
                        primary_client_id,
                        &command,
                    )
                } else {
                    Ok(false)
                }
            }
            RuntimeDisplayOverlayInputAction::SelectPrevious => {
                self.move_primary_display_overlay_selection(-1)
            }
            RuntimeDisplayOverlayInputAction::SelectNext => {
                self.move_primary_display_overlay_selection(1)
            }
            RuntimeDisplayOverlayInputAction::SelectFirst => {
                self.set_primary_display_overlay_selection_index(0)
            }
            RuntimeDisplayOverlayInputAction::SelectLast => {
                let Some(overlay) = self.primary_display_overlay.as_ref() else {
                    return Ok(false);
                };
                self.set_primary_display_overlay_selection_index(
                    overlay.selections.len().saturating_sub(1),
                )
            }
            RuntimeDisplayOverlayInputAction::ScrollBy(delta) if delta < 0 => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(apply_display_overlay_scroll_delta(
                    overlay,
                    delta,
                    self.session.authoritative_size,
                ))
            }
            RuntimeDisplayOverlayInputAction::ScrollBy(delta) => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(apply_display_overlay_scroll_delta(
                    overlay,
                    delta,
                    self.session.authoritative_size,
                ))
            }
            RuntimeDisplayOverlayInputAction::Ignore => Ok(false),
        }
    }

    /// Applies one input chunk while the command-output pager search prompt is active.
    pub(super) fn apply_primary_display_overlay_search_input(
        &mut self,
        input: &[u8],
    ) -> Result<bool> {
        match runtime_display_overlay_input_action(input) {
            RuntimeDisplayOverlayInputAction::Exit => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                overlay.search_input = None;
                overlay.search_status = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::SelectActive => {
                self.submit_primary_display_overlay_search()
            }
            RuntimeDisplayOverlayInputAction::EditSearchBackspace => {
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                let Some(search_input) = overlay.search_input.as_mut() else {
                    return Ok(false);
                };
                let changed = search_input.pop().is_some();
                Ok(changed)
            }
            RuntimeDisplayOverlayInputAction::EditSearchText => {
                let Ok(text) = std::str::from_utf8(input) else {
                    return Ok(false);
                };
                let Some(overlay) = self.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                let Some(search_input) = overlay.search_input.as_mut() else {
                    return Ok(false);
                };
                search_input.push_str(text);
                Ok(!text.is_empty())
            }
            RuntimeDisplayOverlayInputAction::StartSearch
            | RuntimeDisplayOverlayInputAction::SelectPrevious
            | RuntimeDisplayOverlayInputAction::SelectNext
            | RuntimeDisplayOverlayInputAction::SelectFirst
            | RuntimeDisplayOverlayInputAction::SelectLast
            | RuntimeDisplayOverlayInputAction::ScrollBy(_)
            | RuntimeDisplayOverlayInputAction::Ignore => Ok(false),
        }
    }

    /// Submits the active command-output pager search query.
    pub(super) fn submit_primary_display_overlay_search(&mut self) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        let submitted = overlay.search_input.take().unwrap_or_default();
        let query = if submitted.is_empty() {
            let Some(query) = overlay.search_query.clone() else {
                overlay.search_status = Some("search: enter a query".to_string());
                return Ok(true);
            };
            query
        } else {
            overlay.search_query = Some(submitted.clone());
            submitted
        };
        let start_line = overlay
            .search_match
            .map(|search_match| search_match.line_index)
            .or_else(|| overlay.scroll_offset.checked_sub(1))
            .unwrap_or(overlay.scroll_offset);
        let Some(search_match) =
            runtime_display_overlay_next_search_match(overlay, &query, start_line)
        else {
            overlay.search_status = Some(format!("pattern not found: {query}"));
            return Ok(true);
        };
        overlay.search_match = Some(search_match);
        overlay.scroll_offset = search_match.line_index;
        runtime_clamp_display_overlay_scroll(overlay, self.session.authoritative_size);
        overlay.search_status = None;
        Ok(true)
    }

    /// Moves the active command overlay selection and keeps it visible.
    pub(super) fn move_primary_display_overlay_selection(&mut self, delta: isize) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        if overlay.selections.is_empty() {
            return Ok(apply_display_overlay_scroll_delta(
                overlay,
                delta,
                self.session.authoritative_size,
            ));
        }
        let previous = overlay.active_selection_index.unwrap_or(0);
        let next = runtime_selector_step_index(previous, overlay.selections.len(), delta);
        overlay.active_selection_index = Some(next);
        if let Some(line_index) = overlay
            .selections
            .get(next)
            .map(|selection| selection.line_index)
        {
            runtime_scroll_display_overlay_to_line(
                overlay,
                line_index,
                self.session.authoritative_size,
            );
        }
        Ok(next != previous)
    }

    /// Sets the active command overlay selection and keeps it visible.
    pub(super) fn set_primary_display_overlay_selection_index(
        &mut self,
        index: usize,
    ) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        if overlay.selections.is_empty() {
            let next = if index == 0 {
                0
            } else {
                modal_display_overlay_max_scroll(&overlay.lines, self.session.authoritative_size)
            };
            let changed = next != overlay.scroll_offset;
            overlay.scroll_offset = next;
            return Ok(changed);
        }
        let previous = overlay.active_selection_index.unwrap_or(0);
        let next = index.min(overlay.selections.len().saturating_sub(1));
        overlay.active_selection_index = Some(next);
        if let Some(line_index) = overlay
            .selections
            .get(next)
            .map(|selection| selection.line_index)
        {
            runtime_scroll_display_overlay_to_line(
                overlay,
                line_index,
                self.session.authoritative_size,
            );
        }
        Ok(next != previous)
    }
}

/// Copies the currently selected primary display-overlay text.
pub(super) fn primary_display_overlay_copy_selection(
    overlay: &RuntimeDisplayOverlay,
) -> Option<String> {
    let (start, end) = overlay.mouse_selection?;
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if start.line == end.line {
        return overlay
            .lines
            .get(start.line)
            .map(|line| primary_display_overlay_line_slice(line, start.column, end.column));
    }
    let mut copied = Vec::new();
    let first = overlay.lines.get(start.line)?;
    copied.push(primary_display_overlay_line_slice(
        first,
        start.column,
        terminal_text_width(first),
    ));
    for line_index in start.line.saturating_add(1)..end.line {
        copied.push(overlay.lines.get(line_index)?.clone());
    }
    let last = overlay.lines.get(end.line)?;
    copied.push(primary_display_overlay_line_slice(last, 0, end.column));
    Some(copied.join("\n"))
}

/// Applies a signed scroll delta to a display overlay and clamps the viewport.
pub(super) fn apply_display_overlay_scroll_delta(
    overlay: &mut RuntimeDisplayOverlay,
    delta: isize,
    size: Size,
) -> bool {
    let previous = overlay.scroll_offset;
    if delta.is_negative() {
        overlay.scroll_offset = overlay.scroll_offset.saturating_sub(delta.unsigned_abs());
    } else {
        overlay.scroll_offset = overlay
            .scroll_offset
            .saturating_add(usize::try_from(delta).unwrap_or(usize::MAX));
    }
    runtime_clamp_display_overlay_scroll(overlay, size);
    runtime_display_overlay_update_active_selection_for_viewport(overlay, size);
    previous != overlay.scroll_offset
}

/// Returns whether one overlay selection is currently visible in the viewport.
pub(super) fn runtime_display_overlay_selection_index_is_visible(
    overlay: &RuntimeDisplayOverlay,
    selection_index: usize,
    size: Size,
) -> bool {
    let Some(selection) = overlay.selections.get(selection_index) else {
        return false;
    };
    let page_rows = modal_display_overlay_page_rows(size).max(1);
    let visible_start = overlay.scroll_offset;
    let visible_end = visible_start.saturating_add(page_rows);
    selection.line_index >= visible_start && selection.line_index < visible_end
}

/// Keeps the active overlay selection executable only when it is visible.
pub(super) fn runtime_display_overlay_update_active_selection_for_viewport(
    overlay: &mut RuntimeDisplayOverlay,
    size: Size,
) {
    if overlay.selections.is_empty() {
        overlay.active_selection_index = None;
        return;
    }
    if overlay
        .active_selection_index
        .is_some_and(|selection_index| {
            runtime_display_overlay_selection_index_is_visible(overlay, selection_index, size)
        })
    {
        return;
    }
    let page_rows = modal_display_overlay_page_rows(size).max(1);
    let visible_start = overlay.scroll_offset;
    let visible_end = visible_start.saturating_add(page_rows);
    overlay.active_selection_index = overlay.selections.iter().position(|selection| {
        selection.line_index >= visible_start && selection.line_index < visible_end
    });
}

/// Returns one display-column slice from a primary display-overlay line.
pub(super) fn primary_display_overlay_line_slice(line: &str, start: usize, end: usize) -> String {
    let mut output = String::new();
    let mut column = 0usize;
    for grapheme in terminal_graphemes(line) {
        let width = terminal_grapheme_width(grapheme);
        let next = column.saturating_add(width);
        if next <= start {
            column = next;
            continue;
        }
        if column >= end || next > end {
            break;
        }
        output.push_str(grapheme);
        column = next;
    }
    output
}

/// Returns the overlay selection index under a mouse position.
pub(super) fn runtime_display_overlay_selection_index_at_position(
    overlay: &RuntimeDisplayOverlay,
    line_index: usize,
    column: usize,
) -> Option<usize> {
    overlay
        .selections
        .iter()
        .enumerate()
        .filter(|(_, selection)| selection.line_index == line_index)
        .find(|(_, selection)| {
            let start = runtime_display_overlay_rendered_selection_start(overlay, selection);
            let end = start.saturating_add(selection.width);
            column >= start && column < end
        })
        .map(|(index, _)| index)
}

/// Returns the next forward pager-search match, wrapping once to the start.
pub(super) fn runtime_display_overlay_next_search_match(
    overlay: &RuntimeDisplayOverlay,
    query: &str,
    current_line: usize,
) -> Option<RuntimeDisplayOverlaySearchMatch> {
    if query.is_empty() || overlay.lines.is_empty() {
        return None;
    }
    let start = current_line.saturating_add(1).min(overlay.lines.len());
    overlay.lines[start..]
        .iter()
        .enumerate()
        .find_map(|(index, line)| {
            runtime_display_overlay_search_match_on_line(line, query, start.saturating_add(index))
        })
        .or_else(|| {
            overlay.lines[..start]
                .iter()
                .enumerate()
                .find_map(|(index, line)| {
                    runtime_display_overlay_search_match_on_line(line, query, index)
                })
        })
}

/// Returns the render-cell range for a query match on one pager line.
pub(super) fn runtime_display_overlay_search_match_on_line(
    line: &str,
    query: &str,
    line_index: usize,
) -> Option<RuntimeDisplayOverlaySearchMatch> {
    let byte_start = line.find(query)?;
    let byte_end = byte_start.saturating_add(query.len());
    Some(RuntimeDisplayOverlaySearchMatch {
        line_index,
        start_column: UnicodeWidthStr::width(&line[..byte_start]),
        width: UnicodeWidthStr::width(&line[byte_start..byte_end]),
    })
}

/// Replaces a fixed-width region of a rendered line with overlay text.
pub(super) fn runtime_overlay_text_at(line: &mut String, column: usize, width: usize, text: &str) {
    let mut cells = line.chars().collect::<Vec<_>>();
    let required = column.saturating_add(width);
    if cells.len() < required {
        cells.resize(required, ' ');
    }
    for (offset, ch) in text.chars().take(width).enumerate() {
        if let Some(cell) = cells.get_mut(column.saturating_add(offset)) {
            *cell = ch;
        }
    }
    *line = cells.into_iter().collect();
}

/// Returns a selector row rendition, highlighting the hovered item.
pub(super) fn runtime_pane_agent_selector_rendition(
    field: PaneAgentStatusField,
    active: bool,
    ui_theme: &crate::terminal::UiTheme,
) -> crate::terminal::GraphicRendition {
    let pair = if active {
        match field {
            PaneAgentStatusField::Model => ui_theme.colors.agent_model,
            PaneAgentStatusField::Reasoning => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::Thinking => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::Routing => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::ApprovalPolicy => ui_theme.colors.agent_status_blocked,
            PaneAgentStatusField::Latency => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::Preset => ui_theme.colors.agent_model,
        }
    } else {
        ui_theme.colors.display_overlay
    };
    let mut rendition = pair.rendition();
    rendition.bold = active;
    rendition
}

impl RuntimeSessionService {
    /// Shows or clears the primary-client command display overlay.
    ///
    /// Non-empty line sets are rendered as a modal full-window view on the next
    /// primary render pass. An empty line set clears any active overlay. This
    /// fails when the runtime is no longer live.
    pub fn show_primary_display_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        let line_style_spans = vec![Vec::new(); lines.len()];
        self.show_primary_display_overlay_inner(lines, line_style_spans, Vec::new(), false)
    }

    /// Shows or clears the primary-client recoverable error status overlay.
    ///
    /// Error overlays render over the window status bar and are dismissed by
    /// the next user action without consuming that action. This keeps runtime
    /// errors visible without turning them into modal state.
    pub fn show_primary_error_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        self.require_live()?;
        self.primary_error_status_overlay = lines
            .into_iter()
            .find(|line| !line.trim().is_empty())
            .map(|line| runtime_primary_error_status_text(&line));
        Ok(())
    }

    /// Shows or clears the primary-client transient success notice overlay.
    ///
    /// Notice overlays share the status-bar dismissal lifecycle with
    /// recoverable errors while keeping successful command acknowledgements out
    /// of pane transcripts.
    pub fn show_primary_notice_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        self.require_live()?;
        self.primary_error_status_overlay = lines
            .into_iter()
            .find(|line| !line.trim().is_empty())
            .map(|line| runtime_primary_notice_status_text(&line));
        Ok(())
    }

    /// Runs the show primary display overlay inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn show_primary_display_overlay_inner(
        &mut self,
        lines: Vec<String>,
        mut line_style_spans: Vec<Vec<TerminalStyleSpan>>,
        selections: Vec<RuntimeDisplayOverlaySelection>,
        dismiss_on_any_input: bool,
    ) -> Result<()> {
        self.require_live()?;
        self.primary_display_overlay = if lines.is_empty() {
            None
        } else {
            line_style_spans.truncate(lines.len());
            line_style_spans.resize(lines.len(), Vec::new());
            let active_selection_index = (!selections.is_empty()).then_some(0);
            Some(RuntimeDisplayOverlay {
                lines,
                line_style_spans,
                scroll_offset: 0,
                search_input: None,
                search_query: None,
                search_match: None,
                search_status: None,
                mouse_selection: None,
                selections,
                active_selection_index,
                dismiss_on_any_input,
            })
        };
        Ok(())
    }

    /// Clears the primary-client command display overlay.
    ///
    /// Returns true when an overlay was active before the call.
    pub fn clear_primary_display_overlay(&mut self) -> bool {
        self.primary_display_overlay.take().is_some()
    }

    /// Appends terminal-command display output to the active pane buffer.
    ///
    /// Short acknowledgement-style command output should remain in the pane
    /// transcript instead of forcing a modal command-output overlay. The bytes
    /// are fed through the same pane-screen ingestion path as process output so
    /// rendering state, scrollback, and observers stay consistent.
    fn append_runtime_command_display_lines_to_active_pane(
        &mut self,
        lines: &[String],
    ) -> Result<()> {
        let visible_lines = lines
            .iter()
            .map(|line| sanitized_agent_terminal_line(line))
            .filter(|line| !line.trim().is_empty())
            .take(200)
            .collect::<Vec<_>>();
        if visible_lines.is_empty() {
            return Ok(());
        }
        let pane_id = self.active_pane_id()?.to_string();
        let mut bytes = Vec::new();
        for line in visible_lines {
            bytes.extend_from_slice(b"\r\nmez: ");
            bytes.extend_from_slice(line.as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        self.apply_pane_output_bytes(pane_id, bytes)?;
        Ok(())
    }

    /// Presents terminal command display content according to its feedback policy.
    pub(super) fn present_runtime_command_display_content(
        &mut self,
        content: RuntimeCommandDisplayOverlayContent,
    ) -> Result<()> {
        if runtime_command_display_should_open_overlay(&content) {
            return self.show_primary_display_overlay_inner(
                content.lines,
                content.line_style_spans,
                content.selections,
                false,
            );
        }
        if let Some(line) = runtime_command_display_transient_status_line(&content) {
            return self.show_primary_notice_overlay(vec![line]);
        }
        self.append_runtime_command_display_lines_to_active_pane(&content.lines)
    }

    /// Runs the apply primary display overlay terminal action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_primary_display_overlay_terminal_action(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        action: &TerminalClientLoopAction,
    ) -> Result<bool> {
        match action {
            TerminalClientLoopAction::ForwardToPane(input)
            | TerminalClientLoopAction::ForwardMouseToPane { input, .. } => {
                self.apply_primary_display_overlay_input(primary_client_id, input)
            }
            TerminalClientLoopAction::HandleMouse(MouseAction::SelectDisplayOverlay {
                position,
            }) => self.apply_primary_display_overlay_selection(primary_client_id, *position),
            TerminalClientLoopAction::HandleMouse(MouseAction::BeginDisplayOverlaySelection {
                position,
            }) => self.begin_primary_display_overlay_mouse_selection(*position),
            TerminalClientLoopAction::HandleMouse(MouseAction::UpdateDisplayOverlaySelection {
                position,
            }) => self.update_primary_display_overlay_mouse_selection(*position),
            TerminalClientLoopAction::HandleMouse(MouseAction::FinishDisplayOverlaySelection {
                position,
            }) => self.finish_primary_display_overlay_mouse_selection(primary_client_id, *position),
            TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { lines }) => {
                self.apply_primary_display_overlay_scroll(*lines)
            }
            TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => Ok(false),
        }
    }

    /// Reports whether one primary display overlay action should invalidate the
    /// attached client's retained output frame.
    ///
    /// Keyboard and mouse-wheel navigation only move the overlay viewport or
    /// active row, so the next rendered view can be applied through the normal
    /// diff renderer. Exiting the modal overlay or executing a selected row can
    /// expose a different underlying view or run a command, so those paths keep
    /// the stronger redraw signal.
    pub(super) fn primary_display_overlay_action_requires_full_redraw(
        &self,
        action: &TerminalClientLoopAction,
    ) -> bool {
        match action {
            TerminalClientLoopAction::ForwardToPane(input)
            | TerminalClientLoopAction::ForwardMouseToPane { input, .. } => {
                if self
                    .primary_display_overlay
                    .as_ref()
                    .is_some_and(|overlay| overlay.dismiss_on_any_input && !input.is_empty())
                {
                    return true;
                }
                matches!(
                    runtime_display_overlay_input_action(input),
                    RuntimeDisplayOverlayInputAction::Exit
                        | RuntimeDisplayOverlayInputAction::SelectActive
                ) && self
                    .primary_display_overlay
                    .as_ref()
                    .is_none_or(|overlay| overlay.search_input.is_none())
            }
            TerminalClientLoopAction::HandleMouse(MouseAction::SelectDisplayOverlay { .. }) => true,
            TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { .. }) => {
                false
            }
            TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => false,
        }
    }

    /// Executes the selectable command row under a primary display overlay
    /// mouse click.
    fn apply_primary_display_overlay_selection(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(overlay) = self.primary_display_overlay.as_ref() else {
            return Ok(false);
        };
        if position.line == 0 {
            return Ok(false);
        }
        let display_line_index = overlay
            .scroll_offset
            .saturating_add(position.line.saturating_sub(1));
        let selection_index = runtime_display_overlay_selection_index_at_position(
            overlay,
            display_line_index,
            position.column,
        );
        let Some(command) = selection_index
            .and_then(|index| overlay.selections.get(index))
            .map(|selection| selection.command.clone())
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.primary_display_overlay.as_mut() {
            overlay.active_selection_index = selection_index;
        }
        self.execute_primary_display_overlay_selection_command(primary_client_id, &command)
    }

    /// Starts a mouse text selection in the primary command-output overlay.
    fn begin_primary_display_overlay_mouse_selection(
        &mut self,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.primary_display_overlay.as_mut() {
            overlay.mouse_selection = Some((selection_position, selection_position));
        }
        Ok(true)
    }

    /// Extends a mouse text selection in the primary command-output overlay.
    fn update_primary_display_overlay_mouse_selection(
        &mut self,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.primary_display_overlay.as_mut() {
            let start = overlay
                .mouse_selection
                .map(|(start, _)| start)
                .unwrap_or(selection_position);
            overlay.mouse_selection = Some((start, selection_position));
        }
        Ok(true)
    }

    /// Finishes a mouse text selection in the primary command-output overlay and copies it.
    fn finish_primary_display_overlay_mouse_selection(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        let copied = if let Some(overlay) = self.primary_display_overlay.as_mut() {
            let start = overlay
                .mouse_selection
                .map(|(start, _)| start)
                .unwrap_or(selection_position);
            overlay.mouse_selection = Some((start, selection_position));
            primary_display_overlay_copy_selection(overlay)
        } else {
            None
        };
        if let Some(copied) = copied.filter(|text| !text.is_empty()) {
            self.copy_text_to_buffer_and_host_clipboard(
                "mouse",
                copied,
                "display-overlay:mouse".to_string(),
            )?;
            return Ok(true);
        }
        self.apply_primary_display_overlay_selection(primary_client_id, position)
    }

    /// Converts one terminal mouse cell to overlay-content coordinates.
    fn primary_display_overlay_position_for_mouse(
        &self,
        position: CopyPosition,
    ) -> Option<CopyPosition> {
        let overlay = self.primary_display_overlay.as_ref()?;
        let line = position.line.checked_sub(1)?;
        let line = overlay.scroll_offset.saturating_add(line);
        let text = overlay.lines.get(line)?;
        let prefix_columns = runtime_display_overlay_line_prefix_columns(overlay, line);
        let column = position.column.saturating_sub(prefix_columns);
        let column = column.min(terminal_text_width(text));
        Some(CopyPosition { line, column })
    }
}
