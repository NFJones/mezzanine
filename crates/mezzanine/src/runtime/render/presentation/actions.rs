//! Agent action headers, previews, thinking text, and result bounds.

use super::style::{
    AGENT_ACTION_RESULT_DISPLAY_MAX_BYTES, AGENT_ACTION_RESULT_DISPLAY_MAX_LINES,
    AGENT_TERMINAL_MESSAGE_PREFIX, agent_text_foreground_rendition,
};
use super::text::{
    agent_terminal_text_width, bounded_agent_terminal_presentation_columns,
    sanitized_agent_terminal_line, wrap_agent_terminal_text,
};
use super::{
    AgentAction, AgentActionPayload, GraphicRendition, RichTextLine, RichTextLineKind,
    TerminalStyleSpan, UiTheme, UnicodeWidthStr, apply_patch_touched_paths,
};
use mez_mux::render::push_or_extend_style_span;

/// Builds the compact header shown for action execution/result output.
pub(crate) fn agent_action_execution_display_header(action: &AgentAction) -> Option<String> {
    let header = match &action.payload {
        AgentActionPayload::WebSearch { query, .. } => {
            format!("web search: {}", agent_action_display_preview(query))
        }
        AgentActionPayload::FetchUrl { url, .. } => {
            format!("fetch url: {}", agent_action_display_preview(url))
        }
        AgentActionPayload::ApplyPatch { patch, .. } => {
            let paths = apply_patch_touched_paths(patch).unwrap_or_default();
            if paths.is_empty() {
                "apply patch".to_string()
            } else {
                format!("apply patch: {}", agent_action_path_list_preview(&paths))
            }
        }
        AgentActionPayload::ConfigChange {
            setting_path,
            operation,
            ..
        } => format!(
            "config change: {} {}",
            agent_action_display_preview(operation),
            agent_action_display_preview(setting_path)
        ),
        AgentActionPayload::MemorySearch { query, limit } => {
            let mut header = format!("memory search: {}", agent_action_display_preview(query));
            if let Some(limit) = limit {
                header.push_str(" limit=");
                header.push_str(&limit.to_string());
            }
            header
        }
        AgentActionPayload::MemoryStore {
            kind,
            priority,
            scope,
            keywords,
            content,
            expires_in_days,
        } => {
            let mut header = format!(
                "memory store: kind={} keywords={} content={}",
                agent_action_display_preview(kind),
                keywords.len(),
                agent_action_display_preview(content)
            );
            if let Some(scope) = scope.as_deref().map(str::trim)
                && !scope.is_empty()
            {
                header.push_str(" scope=");
                header.push_str(&agent_action_display_preview(scope));
            }
            if let Some(priority) = priority {
                header.push_str(" priority=");
                header.push_str(&priority.to_string());
            }
            if let Some(expires_in_days) = expires_in_days {
                header.push_str(" ttl_days=");
                header.push_str(&expires_in_days.to_string());
            }
            header
        }
        AgentActionPayload::IssueAdd {
            kind,
            title,
            body,
            notes,
            ..
        } => {
            let mut header = format!(
                "issue add: kind={} title={}",
                agent_action_display_preview(kind),
                agent_action_display_preview(title)
            );
            if let Some(body) = body.as_deref().map(str::trim)
                && !body.is_empty()
            {
                header.push_str(" body=");
                header.push_str(&agent_action_display_preview(body));
            }
            if let Some(notes) = notes.as_deref().map(str::trim)
                && !notes.is_empty()
            {
                header.push_str(" notes=");
                header.push_str(&agent_action_display_preview(notes));
            }
            header
        }
        AgentActionPayload::IssueUpdate {
            id,
            kind,
            title,
            body,
            clear_body,
            notes,
            clear_notes,
            ..
        } => {
            let mut header = format!("issue update: id={}", agent_action_display_preview(id));
            if let Some(kind) = kind.as_deref().map(str::trim)
                && !kind.is_empty()
            {
                header.push_str(" kind=");
                header.push_str(&agent_action_display_preview(kind));
            }
            if let Some(title) = title.as_deref().map(str::trim)
                && !title.is_empty()
            {
                header.push_str(" title=");
                header.push_str(&agent_action_display_preview(title));
            }
            if let Some(body) = body.as_deref().map(str::trim)
                && !body.is_empty()
            {
                header.push_str(" body=");
                header.push_str(&agent_action_display_preview(body));
            }
            if *clear_body {
                header.push_str(" clear_body=true");
            }
            if let Some(notes) = notes.as_deref().map(str::trim)
                && !notes.is_empty()
            {
                header.push_str(" notes=");
                header.push_str(&agent_action_display_preview(notes));
            }
            if *clear_notes {
                header.push_str(" clear_notes=true");
            }
            header
        }
        AgentActionPayload::IssueQuery {
            kind,
            state,
            text,
            limit,
            refresh,
        } => {
            let mut header = match kind
                .as_deref()
                .map(str::trim)
                .filter(|kind| !kind.is_empty())
            {
                Some(kind) => format!("issue query: kind={}", agent_action_display_preview(kind)),
                None => "issue query: current project".to_string(),
            };
            if let Some(state) = state.as_deref().map(str::trim)
                && !state.is_empty()
            {
                header.push_str(" state=");
                header.push_str(&agent_action_display_preview(state));
            }
            if let Some(text) = text.as_deref().map(str::trim)
                && !text.is_empty()
            {
                header.push_str(" text=");
                header.push_str(&agent_action_display_preview(text));
            }
            if let Some(limit) = limit {
                header.push_str(" limit=");
                header.push_str(&limit.to_string());
            }
            if *refresh {
                header.push_str(" refresh=true");
            }
            header
        }
        AgentActionPayload::IssueDelete { id } => {
            format!("issue delete: id={}", agent_action_display_preview(id))
        }
        AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } => {
            let mut header = format!(
                "mcp call: {}/{}",
                agent_action_display_preview(server),
                agent_action_display_preview(tool)
            );
            let arguments = agent_action_json_argument_preview(arguments_json);
            if !arguments.is_empty() {
                header.push_str(" args=");
                header.push_str(&arguments);
            }
            header
        }
        AgentActionPayload::RequestSkills => "skill lookup: available skills".to_string(),
        AgentActionPayload::CallSkill {
            name,
            additional_context,
        } => {
            let mut header = format!("skill load: {}", agent_action_display_preview(name));
            if let Some(context) = additional_context.as_deref().map(str::trim)
                && !context.is_empty()
            {
                header.push_str(" context=");
                header.push_str(&agent_action_display_preview(context));
            }
            header
        }
        AgentActionPayload::SpawnAgent {
            role,
            placement,
            cooperation_mode,
            task_prompt,
            ..
        } => format!(
            "spawn agent: {} ({}, {}): {}",
            agent_action_display_preview(role),
            agent_action_display_preview(placement),
            agent_action_display_preview(cooperation_mode),
            agent_action_display_preview(task_prompt)
        ),
        _ => return None,
    };
    Some(header)
}

/// Returns model-authored action summary lines for normal thinking logs.
pub(crate) fn agent_action_model_thinking_lines(action: &AgentAction) -> Vec<String> {
    match &action.payload {
        AgentActionPayload::ShellCommand { summary, .. } => {
            let summary = sanitized_agent_terminal_line(summary.trim());
            if summary.trim().is_empty() {
                Vec::new()
            } else {
                vec![summary]
            }
        }
        _ => Vec::new(),
    }
}

/// Normalizes model-authored thinking text before presenting it as assistant output.
pub(crate) fn agent_thinking_display_text(text: &str) -> String {
    text.trim_end_matches(['\r', '\n'])
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            trimmed
                .strip_prefix("agent thinking:")
                .or_else(|| trimmed.strip_prefix("thinking:"))
                .map(str::trim_start)
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Builds width-bounded status-style thinking lines from rationale text.
pub(crate) fn agent_thinking_display_lines_for_width(text: &str, columns: usize) -> Vec<String> {
    let prefix = "thinking: ";
    let prefix_width = UnicodeWidthStr::width(prefix);
    let content_width = bounded_agent_terminal_presentation_columns(columns)
        .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
        .max(1);
    let segment_width = content_width.saturating_sub(prefix_width).max(1);
    let continuation = " ".repeat(prefix_width);
    agent_thinking_display_text(text)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .flat_map(|line| {
            wrap_agent_terminal_text(&sanitized_agent_terminal_line(line), segment_width)
                .into_iter()
                .enumerate()
                .map(|(index, segment)| {
                    if index == 0 {
                        format!("{prefix}{segment}")
                    } else {
                        format!("{continuation}{segment}")
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Builds one width-bounded macro lifecycle line for the parent transcript.
pub(crate) fn agent_macro_lifecycle_display_lines_for_width(
    macro_name: &str,
    step_index: Option<usize>,
    total_steps: usize,
    status: &str,
    columns: usize,
) -> Vec<String> {
    let content_width = bounded_agent_terminal_presentation_columns(columns)
        .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
        .max(1);
    let macro_name = macro_name.split_whitespace().collect::<Vec<_>>().join(" ");
    let status = status.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = match step_index {
        Some(step_index) => format!(
            "macro {macro_name} ({}/{}): {status}",
            step_index.saturating_add(1),
            total_steps.max(1)
        ),
        None => format!("macro {macro_name}: {status}"),
    };
    wrap_agent_terminal_text(&sanitized_agent_terminal_line(&text), content_width)
        .into_iter()
        .next()
        .into_iter()
        .collect()
}

/// Builds the compact header shown above elevated action result output.
pub(crate) fn agent_action_result_display_header(action: &AgentAction) -> Option<String> {
    agent_action_execution_display_header(action)
}

/// Builds the normal-mode action execution line with balanced visual weight.
///
/// The display text deliberately keeps the established `agent: action: target`
/// grammar while applying color only to the semantic pieces that need emphasis:
/// the prefix is quiet status text, the action phrase is command-accented, and
/// arguments fall back to the terminal foreground for readability.
pub(crate) fn agent_action_execution_rendered_line(
    header: &str,
    ui_theme: &UiTheme,
) -> RichTextLine {
    let display = format!("agent: {header}");
    let mut style_spans = Vec::new();
    let mut status_rendition =
        agent_text_foreground_rendition(ui_theme.colors.agent_transcript_status);
    status_rendition.dim = true;
    let mut command_rendition =
        agent_text_foreground_rendition(ui_theme.colors.agent_transcript_command);
    command_rendition.bold = true;

    push_agent_action_execution_style_span(
        &mut style_spans,
        &display,
        0,
        "agent:".len(),
        status_rendition,
    );

    let action_start_bytes = "agent: ".len();
    let (action_end_bytes, separator_end_bytes) = header
        .find(':')
        .map(|separator| {
            let action_end = action_start_bytes.saturating_add(separator);
            (action_end, Some(action_end.saturating_add(1)))
        })
        .unwrap_or_else(|| (display.len(), None));
    push_agent_action_execution_style_span(
        &mut style_spans,
        &display,
        action_start_bytes,
        action_end_bytes,
        command_rendition,
    );
    if let Some(separator_end_bytes) = separator_end_bytes {
        push_agent_action_execution_style_span(
            &mut style_spans,
            &display,
            action_end_bytes,
            separator_end_bytes,
            status_rendition,
        );
    }
    push_agent_action_execution_secondary_spans(&mut style_spans, &display, status_rendition);

    RichTextLine {
        display,
        style_spans,
        copy_text: None,
        kind: RichTextLineKind::Normal,
    }
}

/// Adds one action-execution style span from byte offsets.
///
/// # Parameters
/// - `spans`: The style span collection being assembled.
/// - `display`: The full action execution line.
/// - `start_bytes`: The byte offset where styling begins.
/// - `end_bytes`: The byte offset where styling ends.
/// - `rendition`: The terminal style applied to the range.
pub(crate) fn push_agent_action_execution_style_span(
    spans: &mut Vec<TerminalStyleSpan>,
    display: &str,
    start_bytes: usize,
    end_bytes: usize,
    rendition: GraphicRendition,
) {
    if start_bytes >= end_bytes || end_bytes > display.len() {
        return;
    }
    let start = agent_terminal_text_width(&display[..start_bytes]);
    let length = agent_terminal_text_width(&display[start_bytes..end_bytes]);
    push_or_extend_style_span(
        spans,
        TerminalStyleSpan {
            start,
            length,
            rendition,
        },
    );
}

/// Styles quiet secondary action-header fragments such as `(+3 more)`.
///
/// # Parameters
/// - `spans`: The style span collection being assembled.
/// - `display`: The full action execution line.
/// - `rendition`: The muted terminal style applied to secondary fragments.
pub(crate) fn push_agent_action_execution_secondary_spans(
    spans: &mut Vec<TerminalStyleSpan>,
    display: &str,
    rendition: GraphicRendition,
) {
    let mut search_start = 0usize;
    while let Some(relative_start) = display[search_start..].find("(+") {
        let start = search_start.saturating_add(relative_start);
        let Some(relative_end) = display[start..].find(" more)") else {
            search_start = start.saturating_add(2);
            continue;
        };
        let end = start
            .saturating_add(relative_end)
            .saturating_add(" more)".len());
        push_agent_action_execution_style_span(spans, display, start, end, rendition);
        search_start = end;
    }
}

/// Builds a compact, single-line preview for action-result headers.
pub(crate) fn agent_action_display_preview(value: &str) -> String {
    /// Maximum preview characters included in an action-result header.
    const MAX_AGENT_ACTION_RESULT_HEADER_CHARS: usize = 120;
    let trimmed = value.trim();
    let mut preview = String::new();
    let mut chars = trimmed.chars();
    for _ in 0..MAX_AGENT_ACTION_RESULT_HEADER_CHARS {
        let Some(ch) = chars.next() else {
            return preview;
        };
        preview.push(match ch {
            '\r' | '\n' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        });
    }
    if chars.next().is_some() {
        preview.push_str("...");
    }
    preview
}

/// Builds a compact preview for action arguments that are already JSON.
pub(crate) fn agent_action_json_argument_preview(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "{}" || trimmed == "null" {
        return String::new();
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
        .map(|text| agent_action_display_preview(&text))
        .unwrap_or_else(|| agent_action_display_preview(trimmed))
}

/// Builds a compact preview for one or more action paths.
pub(crate) fn agent_action_path_list_preview(paths: &[String]) -> String {
    match paths {
        [] => "(none)".to_string(),
        [single] => agent_action_display_preview(single),
        many => {
            let first = agent_action_display_preview(&many[0]);
            format!("{first} (+{} more)", many.len().saturating_sub(1))
        }
    }
}

/// Returns bounded, sanitized payload lines for normal pane display.
pub(crate) fn bounded_agent_action_result_display_lines(text: &str) -> Vec<String> {
    let normalized = text
        .trim_end_matches(['\r', '\n'])
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if normalized.is_empty() {
        return vec!["[mez: no output]".to_string()];
    }
    let mut lines = Vec::new();
    let mut used_bytes = 0usize;
    let mut truncated = false;
    for (index, line) in normalized.lines().enumerate() {
        if index >= AGENT_ACTION_RESULT_DISPLAY_MAX_LINES {
            truncated = true;
            break;
        }
        let mut line = sanitized_agent_terminal_line(line);
        let remaining = AGENT_ACTION_RESULT_DISPLAY_MAX_BYTES.saturating_sub(used_bytes);
        if remaining == 0 {
            truncated = true;
            break;
        }
        if line.len() > remaining {
            line = truncate_to_utf8_boundary(&line, remaining);
            line.push_str("...");
            truncated = true;
            lines.push(line);
            break;
        }
        used_bytes = used_bytes.saturating_add(line.len()).saturating_add(1);
        lines.push(line);
    }
    if truncated {
        lines.push("[mez: output truncated for pane display]".to_string());
    }
    if lines.is_empty() {
        lines.push("[mez: no output]".to_string());
    }
    lines
}

/// Truncates text to a valid UTF-8 byte boundary.
pub(crate) fn truncate_to_utf8_boundary(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
