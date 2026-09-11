//! Product prompt, shell-result, listing-action, and status content projection.

use super::action_registry::{
    OverlayActionTarget, RuntimeOverlayAction, overlay_set_key_preset_target,
    overlay_set_theme_target,
};
use super::display_content::{
    RuntimeCommandDisplayOverlayContent, runtime_command_overlay_available_width,
    runtime_human_readable_display_lines, runtime_live_overlay_source_from_json,
    wrap_runtime_command_display_overlay_content,
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
        selector_extra_candidates_loaded: false,
        selector_extra_candidates_initialized: false,
        selector_extra_candidates_generation: 1,
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
    /// One-line recoverable command failure rendered through the error status bar.
    TransientErrorStatus(Vec<String>),
    /// Preformatted command feedback rendered through a transient status bar.
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
    let live_source = runtime_live_overlay_source_from_json(parsed.get("live_source"));
    if let Some(presentation) = parsed
        .get("presentation")
        .and_then(serde_json::Value::as_str)
    {
        let body = parsed
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        return match presentation {
            "pager" => {
                let mut content = runtime_agent_shell_markdown_overlay_content_for_layout(
                    command,
                    body,
                    ui_theme,
                    terminal_width,
                    terminal_width.max(1),
                );
                content.live_source = live_source;
                Ok(RuntimeAgentShellDisplayOutput::Overlay(content))
            }
            "notice" => {
                let mut lines = runtime_human_readable_display_lines(body);
                lines.truncate(200);
                Ok(RuntimeAgentShellDisplayOutput::TransientStatus(lines))
            }
            "error_notice" => {
                let mut lines = runtime_human_readable_display_lines(body);
                lines.truncate(200);
                Ok(RuntimeAgentShellDisplayOutput::TransientErrorStatus(lines))
            }
            _ => Err(MezError::invalid_args(
                "agent shell response has an unsupported presentation destination",
            )),
        };
    }
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
            let mut content = runtime_agent_shell_markdown_overlay_content_for_layout(
                command.clone(),
                body,
                ui_theme,
                terminal_width,
                display_width,
            );
            content.live_source = live_source;
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

/// Explicit pager metadata opens the command overlay even when the rendered
/// Markdown contains only one short line.
#[cfg(test)]
#[test]
fn explicit_agent_shell_pager_ignores_rendered_line_count() {
    let output = runtime_agent_shell_display_output(
        r#"{"kind":"display","command":"inspect","content_type":"text/markdown; charset=utf-8","presentation":"pager","body":"one line"}"#,
        &mez_mux::theme::deepforest_ui_theme(),
        80,
        32,
    )
    .expect("explicit pager response should render");

    let RuntimeAgentShellDisplayOutput::Overlay(content) = output else {
        panic!("explicit pager response must not become pane lines");
    };
    assert_eq!(content.command.as_deref(), Some("inspect"));
    assert!(content.lines.iter().any(|line| line.contains("one line")));
}

/// Explicit success and error notices remain transient even when their bodies
/// contain multiple lines that legacy inference would append to the pane log.
#[cfg(test)]
#[test]
fn explicit_agent_shell_notices_never_become_pane_lines() {
    let theme = mez_mux::theme::deepforest_ui_theme();
    let notice = runtime_agent_shell_display_output(
        r#"{"kind":"display","command":"mutate","presentation":"notice","body":"changed\nsubsequent actions only"}"#,
        &theme,
        80,
        32,
    )
    .expect("explicit notice should decode");
    assert!(matches!(
        notice,
        RuntimeAgentShellDisplayOutput::TransientStatus(ref lines)
            if lines == &["changed".to_string(), "subsequent actions only".to_string()]
    ));

    let error = runtime_agent_shell_display_output(
        r#"{"kind":"display","command":"mutate","presentation":"error_notice","body":"failed\nsee audit"}"#,
        &theme,
        80,
        32,
    )
    .expect("explicit error notice should decode");
    assert!(matches!(
        error,
        RuntimeAgentShellDisplayOutput::TransientErrorStatus(ref lines)
            if lines == &["failed".to_string(), "see audit".to_string()]
    ));
}

/// Runtime JSON preserves the typed presentation destination so decoding does
/// not need to infer placement from command names or rendered body shape.
#[cfg(test)]
#[test]
fn explicit_agent_shell_presentation_serializes_for_runtime_decoding() {
    use crate::integrations::agent::slash::{AgentShellCommandOutcome, AgentShellPresentation};

    for (presentation, expected) in [
        (AgentShellPresentation::Pager, "pager"),
        (AgentShellPresentation::ErrorNotice, "error_notice"),
    ] {
        let response = crate::runtime::runtime_agent_shell_command_response_json(
            "%1",
            "/inspect",
            Some(&AgentShellCommandOutcome::Presented {
                command: "inspect".to_string(),
                body: "body".to_string(),
                presentation,
            }),
        );
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed
                .get("presentation")
                .and_then(serde_json::Value::as_str),
            Some(expected)
        );
    }
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
    assert!(table_rows.iter().all(|width| *width <= 32), "{content:?}");
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
        content.actions.is_empty(),
        "untrusted markdown link syntax must register no action: {content:?}"
    );
    assert!(
        content.lines.iter().any(|line| line.contains("open issue")),
        "the link label must still render as text: {content:?}"
    );
    assert!(
        content.line_copy_texts.iter().flatten().any(|copy_text| {
            copy_text == "A long prose sentence with enough words to wrap repeatedly."
        }),
        "{content:?}"
    );
    assert!(
        content
            .line_copy_texts
            .iter()
            .flatten()
            .any(|copy_text| { copy_text.contains("(mez-agent:%2Fshow-issues%20issue-1)") }),
        "the hidden destination must stay copyable: {content:?}"
    );
}

/// Verifies raw and percent-encoded `mez-agent:` destinations in any rendered
/// body register no selectable action while their text stays copyable.
///
/// The product registers actions only for ranges it composes itself, so a
/// record body, metadata value, title, prompt, or MCP description containing
/// link syntax cannot create an executable control or repeat a prior action.
#[cfg(test)]
#[test]
fn untrusted_markdown_links_register_no_overlay_actions() {
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
    for body in [
        "run [approve](mez-agent:%2Fapprove) now",
        "run [approve](mez-agent:/approve) now",
        "run mez-agent:%2Fapprove now",
        "> [neighbor](mez-agent:%2Fapprove) needs a decision",
        "| ID | Action |\n| --- | --- |\n| a1 | [approve](mez-agent:%2Fapprove) |",
        "`mez-agent:%2Fapprove`",
    ] {
        let content = runtime_agent_shell_markdown_overlay_content_for_layout(
            Some("show-issues".to_string()),
            body,
            &ui_theme,
            80,
            40,
        );
        assert!(content.actions.is_empty(), "{body}: {content:?}");
        assert!(
            content.lines.iter().any(|line| line.contains("approve")),
            "{body}: {content:?}"
        );
        assert!(
            content
                .line_copy_texts
                .iter()
                .flatten()
                .any(|copy_text| copy_text.contains("approve")),
            "{body}: {content:?}"
        );
    }
}

/// Renders slash-command markdown display output into the command overlay
/// pager while keeping every rendered link inert.
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
        live_source: None,
        lines: Vec::new(),
        line_style_spans: Vec::new(),
        line_kinds: Vec::new(),
        line_copy_texts: Vec::new(),
        actions: Vec::new(),
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
        for (start_column, width, target) in runtime_markdown_body_row_actions(
            content.command.as_deref(),
            copy_text.as_deref(),
            &display,
        ) {
            let logical_id = content.actions.len();
            content.actions.push(RuntimeOverlayAction {
                logical_id,
                line_index,
                start_column,
                width,
                target,
                kind: OverlaySelectionKind::Primary,
            });
            push_or_extend_style_span(
                &mut style_spans,
                TerminalStyleSpan {
                    start: start_column,
                    length: width,
                    rendition: overlay_link_rendition(ui_theme),
                },
            );
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
        runtime_command_overlay_available_width(terminal_width, !initial.actions.is_empty());
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

/// Returns the product-owned action range for one rendered Markdown body row.
///
/// Only rows the product itself composes are considered. Listing commands
/// publish one validated selection action per row; every other body, including
/// untrusted record markdown, keeps any link syntax as inert, copyable text
/// because no rendered text can register an executable target.
pub(crate) fn runtime_markdown_body_row_actions(
    command: Option<&str>,
    source_line: Option<&str>,
    display: &str,
) -> Vec<(usize, usize, OverlayActionTarget)> {
    let Some(source_line) = source_line else {
        return Vec::new();
    };
    match command {
        Some("list-themes") => {
            runtime_markdown_listing_row_action(source_line, display, overlay_set_theme_target)
        }
        Some("list-key-presets") => {
            runtime_markdown_listing_row_action(source_line, display, overlay_set_key_preset_target)
        }
        _ => Vec::new(),
    }
}

/// Returns the rendered range of one validated listing action cell.
///
/// Listing producers author a code span whose text is the exact command label,
/// so the range is located by its rendered text and validated by the supplied
/// target builder. A clipped or reshaped row registers nothing rather than
/// falling back to raw text.
fn runtime_markdown_listing_row_action(
    source_line: &str,
    display: &str,
    resolve: impl Fn(&str) -> Option<OverlayActionTarget>,
) -> Vec<(usize, usize, OverlayActionTarget)> {
    let cells = runtime_markdown_table_cells(source_line);
    let Some(label) = cells.last() else {
        return Vec::new();
    };
    let label = label.trim_matches('`').trim();
    let Some(target) = resolve(label) else {
        return Vec::new();
    };
    let Some(start_byte) = display.find(label) else {
        return Vec::new();
    };
    let start_column = UnicodeWidthStr::width(&display[..start_byte]);
    let width = UnicodeWidthStr::width(label);
    if width == 0 {
        return Vec::new();
    }
    vec![(start_column, width, target)]
}
