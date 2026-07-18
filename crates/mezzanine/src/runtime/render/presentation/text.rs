//! Agent text, Markdown, wrapping, and styled-line composition.

use super::diff::{agent_command_syntax_theme, agent_shell_command_highlighter};
use super::style::{
    AGENT_TERMINAL_MESSAGE_PREFIX, AgentTerminalPresentationStyle, agent_terminal_sgr_sequence,
    agent_text_foreground_rendition,
};
use super::{
    GraphicRendition, MARKDOWN_DARK_MUTED_FOREGROUND, MARKDOWN_DARK_NEUTRAL_FOREGROUND,
    MARKDOWN_LIGHT_NEUTRAL_FOREGROUND, RenderedClientView, RichTextLine, RichTextLineKind,
    RichTextTheme, ShellClassification, TerminalColor, TerminalStyleSpan, TerminalStyledLine,
    UiTheme, UnicodeSegmentation, UnicodeWidthStr, agent_wrap_column_cap, append_syntax_spans,
    overlay_fixed_column_style_spans, overlay_text_cells, prefix_rich_text_lines, render_markdown,
    runtime_mezzanine_error_code, terminal_grapheme_width, wrap_rich_text_lines_to_width,
};
use crate::error::MezError;
use mez_mux::render::{push_or_extend_style_span, terminal_color_luminance};

/// Runs the sanitized agent terminal line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn sanitized_agent_terminal_line(line: &str) -> String {
    line.chars()
        .map(|ch| {
            if ch == '\t' || !ch.is_control() {
                ch
            } else {
                ' '
            }
        })
        .collect()
}

/// Runs the prefixed agent terminal lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn prefixed_agent_terminal_lines(prefix: &str, text: &str) -> Vec<String> {
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return vec![prefix.to_string()];
    }
    let continuation = " ".repeat(prefix.chars().count());
    trimmed
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line = sanitized_agent_terminal_line(line);
            if index == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{continuation}{line}")
            }
        })
        .collect()
}

/// Builds width-wrapped rendered agent transcript rows for simple text.
///
/// Plain `say` output and display-only patch examples should wrap through the
/// same presentation engine as markdown so continuation rows align under the
/// first writable column after the speaker indicator.
pub(crate) fn wrapped_prefixed_agent_terminal_lines(
    prefix: &str,
    text: &str,
    display_width: usize,
) -> Vec<RichTextLine> {
    let lines = prefixed_agent_terminal_lines(prefix, text)
        .into_iter()
        .map(|display| RichTextLine {
            display,
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        })
        .collect::<Vec<_>>();
    wrap_rich_text_lines_to_width(lines, display_width, display_width)
}

/// Returns true when a display-only `say` body is a raw Mezzanine patch example.
///
/// Markdown treats leading `***` as structural syntax in some contexts. Raw
/// patch examples should stay literal and copyable instead of being parsed as
/// markdown or an executable action.
pub(crate) fn agent_say_text_is_displayed_patch_block(text: &str) -> bool {
    let trimmed = text.trim_start_matches(['\r', '\n']);
    trimmed.starts_with("*** Begin Patch")
        || trimmed
            .strip_prefix("\\n")
            .is_some_and(|rest| rest.starts_with("*** Begin Patch"))
}

/// Renders model-authored markdown body lines without the surrounding frame.
///
/// The returned display text intentionally omits markdown delimiters where the
/// terminal style can carry the same meaning. Callers add frame rows and keep
/// the raw markdown in copy metadata so this is only a visual transformation.
pub(crate) fn render_agent_markdown_body_lines(
    markdown: &str,
    ui_theme: &UiTheme,
    table_display_width: usize,
) -> Vec<RichTextLine> {
    let trimmed = markdown.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return vec![RichTextLine {
            display: "mez> ".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        }];
    }
    let table_body_display_width = table_display_width
        .saturating_sub(UnicodeWidthStr::width("mez> "))
        .saturating_sub(1)
        .max(1);
    prefix_rich_text_lines(
        render_markdown(
            trimmed,
            &agent_rich_text_theme(ui_theme),
            Some(table_body_display_width),
        ),
        "mez> ",
        "     ",
    )
}

/// Renders runtime command markdown body lines without the surrounding frame.
#[cfg(test)]
pub(crate) fn render_command_markdown_body_lines(
    markdown: &str,
    ui_theme: &UiTheme,
) -> Vec<RichTextLine> {
    render_command_markdown_body_lines_for_width(markdown, ui_theme, None)
}

/// Renders runtime command Markdown with width-aware table layout.
pub(crate) fn render_command_markdown_body_lines_for_width(
    markdown: &str,
    ui_theme: &UiTheme,
    table_display_width: Option<usize>,
) -> Vec<RichTextLine> {
    let trimmed = markdown.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Vec::new();
    }
    render_markdown(
        trimmed,
        &agent_rich_text_theme(ui_theme),
        table_display_width,
    )
}

/// Maps product theme slots onto the neutral rich-text semantic palette.
pub(super) fn agent_rich_text_theme(ui_theme: &UiTheme) -> RichTextTheme {
    RichTextTheme {
        heading: ui_theme.colors.agent_transcript_user.foreground,
        structural: markdown_structural_foreground(ui_theme),
        link: ui_theme.colors.agent_transcript_command.foreground,
        inline_code: markdown_inline_code_foreground(ui_theme),
        table_alternate_row: markdown_table_alternate_row_foreground(ui_theme),
        diff_addition: ui_theme.colors.agent_transcript_user.foreground,
        diff_deletion: ui_theme.colors.agent_transcript_error.foreground,
    }
}

/// Returns the foreground used for inline markdown code on the active theme.
pub(crate) fn markdown_inline_code_foreground(ui_theme: &UiTheme) -> TerminalColor {
    if markdown_surface_is_light(ui_theme) {
        MARKDOWN_DARK_NEUTRAL_FOREGROUND
    } else {
        MARKDOWN_LIGHT_NEUTRAL_FOREGROUND
    }
}

/// Returns the foreground used to distinguish alternating markdown table rows.
pub(crate) fn markdown_table_alternate_row_foreground(ui_theme: &UiTheme) -> TerminalColor {
    if markdown_surface_is_light(ui_theme) {
        MARKDOWN_DARK_MUTED_FOREGROUND
    } else {
        MARKDOWN_LIGHT_NEUTRAL_FOREGROUND
    }
}

/// Returns the foreground used for subdued markdown structural accents.
pub(crate) fn markdown_structural_foreground(ui_theme: &UiTheme) -> TerminalColor {
    if markdown_surface_is_light(ui_theme) {
        MARKDOWN_DARK_MUTED_FOREGROUND
    } else {
        ui_theme.colors.agent_transcript_status.foreground
    }
}

/// Returns whether markdown should use dark neutral text accents.
pub(crate) fn markdown_surface_is_light(ui_theme: &UiTheme) -> bool {
    terminal_color_luminance(ui_theme.colors.agent_transcript_assistant.background)
        .or_else(|| terminal_color_luminance(ui_theme.colors.frame_fill.background))
        .is_some_and(|luminance| luminance >= 140)
}

/// Runs the command preview terminal lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn command_preview_terminal_lines(
    command: &str,
    columns: usize,
    max_lines: usize,
) -> Vec<String> {
    let prefix = "$ ";
    let continuation = " ".repeat(prefix.chars().count());
    let content_width = columns.max(1);
    let wrapped = wrap_agent_terminal_text(command, content_width);
    let total_lines = wrapped.len();
    let capped = if max_lines > 0 && total_lines > max_lines {
        let mut lines = wrapped
            .iter()
            .take(max_lines.saturating_sub(1))
            .cloned()
            .collect::<Vec<_>>();
        let count_prefix = format!("[{total_lines}] ");
        let count_width = agent_terminal_text_width(count_prefix.as_str());
        let available = content_width.saturating_sub(count_width).max(1);
        let tail = wrapped
            .last()
            .map(|line| fit_agent_terminal_text_width(line, available))
            .unwrap_or_default();
        lines.push(format!("{count_prefix}{tail}"));
        lines
    } else {
        wrapped
    };
    capped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{continuation}{line}")
            }
        })
        .collect()
}

/// Renders a shell command preview with bounded wrapping and syntax spans.
pub(crate) fn command_preview_terminal_rendered_lines(
    command: &str,
    columns: usize,
    max_lines: usize,
    classification: ShellClassification,
    ui_theme: &UiTheme,
) -> Vec<RichTextLine> {
    let syntax_theme = agent_command_syntax_theme(ui_theme);
    let mut highlighter = agent_shell_command_highlighter(classification, &syntax_theme);
    let command_rendition =
        agent_terminal_label_rendition(AgentTerminalPresentationStyle::Command, ui_theme);
    command_preview_terminal_lines(command, columns, max_lines)
        .into_iter()
        .map(|display| {
            let mut rendered = RichTextLine {
                display,
                style_spans: Vec::new(),
                copy_text: None,
                kind: RichTextLineKind::Normal,
            };
            let line_width = agent_terminal_text_width(rendered.display.as_str());
            push_or_extend_style_span(
                &mut rendered.style_spans,
                TerminalStyleSpan {
                    start: 0,
                    length: line_width,
                    rendition: command_rendition,
                },
            );
            let (text_start, syntax_text) = rendered
                .display
                .strip_prefix("$ ")
                .or_else(|| rendered.display.strip_prefix("  "))
                .map(|text| (2, text.to_string()))
                .unwrap_or_else(|| (0, rendered.display.clone()));
            if let Some(highlighter) = highlighter.as_mut() {
                append_syntax_spans(&mut rendered, text_start, &syntax_text, highlighter);
            }
            rendered
        })
        .collect()
}

/// Runs the wrap agent terminal text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn wrap_agent_terminal_text(text: &str, columns: usize) -> Vec<String> {
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return vec![String::new()];
    }
    let mut output = Vec::new();
    for source_line in trimmed.lines() {
        let sanitized = sanitized_agent_terminal_line(source_line);
        if sanitized.is_empty() {
            output.push(String::new());
            continue;
        }
        let mut remaining = sanitized.as_str();
        while !remaining.is_empty() {
            if agent_terminal_text_width(remaining) <= columns {
                output.push(remaining.to_string());
                break;
            }
            let Some((segment, consumed)) =
                take_agent_terminal_word_wrapped_segment(remaining, columns)
            else {
                output.push(remaining.to_string());
                break;
            };
            output.push(segment);
            remaining = remaining[consumed..].trim_start();
        }
    }
    if output.is_empty() {
        output.push(String::new());
    }
    output
}

/// Returns one command-preview segment using whitespace first and a hard cell
/// boundary only when no whitespace break exists.
pub(crate) fn take_agent_terminal_word_wrapped_segment(
    text: &str,
    columns: usize,
) -> Option<(String, usize)> {
    let mut width = 0usize;
    let mut last_space_break: Option<(usize, usize)> = None;
    let mut boundary_consumed = 0usize;
    for (index, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
        let grapheme_width = agent_terminal_grapheme_width(grapheme);
        if width > 0 && width.saturating_add(grapheme_width) > columns {
            break;
        }
        if grapheme.chars().all(char::is_whitespace) && width > 0 {
            last_space_break = Some((index, index.saturating_add(grapheme.len())));
        }
        boundary_consumed = index.saturating_add(grapheme.len());
        width = width.saturating_add(grapheme_width);
        if width >= columns {
            break;
        }
    }
    last_space_break
        .filter(|(space_start, _)| *space_start > 0)
        .map(|(space_start, consumed)| (text[..space_start].to_string(), consumed))
        .or_else(|| {
            (boundary_consumed > 0)
                .then(|| (text[..boundary_consumed].to_string(), boundary_consumed))
        })
}

/// Runs the fit agent terminal text width operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn fit_agent_terminal_text_width(text: &str, columns: usize) -> String {
    if agent_terminal_text_width(text) <= columns {
        return text.to_string();
    }

    let mut width = 0usize;
    let mut last_space_break = None;
    for (index, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
        let grapheme_width = agent_terminal_grapheme_width(grapheme);
        if width > 0 && width.saturating_add(grapheme_width) > columns {
            break;
        }
        if grapheme.chars().all(char::is_whitespace) && width > 0 {
            last_space_break = Some(index);
        }
        width = width.saturating_add(grapheme_width);
        if width >= columns {
            break;
        }
    }

    last_space_break
        .filter(|end| *end > 0)
        .map(|end| text[..end].to_string())
        .unwrap_or_else(|| text.to_string())
}

/// Bounds agent transcript presentation width to the pane width or 120 cells.
///
/// # Parameters
/// - `columns`: The current pane width in terminal display cells.
pub(crate) fn bounded_agent_terminal_presentation_columns(columns: usize) -> usize {
    columns.clamp(1, agent_wrap_column_cap())
}

/// Returns the display width of agent transcript text.
///
/// # Parameters
/// - `text`: The agent transcript text to measure.
pub(crate) fn agent_terminal_text_width(text: &str) -> usize {
    UnicodeSegmentation::graphemes(text, true)
        .map(agent_terminal_grapheme_width)
        .sum()
}

/// Returns the display width of one agent transcript grapheme cluster.
///
/// # Parameters
/// - `grapheme`: The grapheme cluster to measure.
pub(crate) fn agent_terminal_grapheme_width(grapheme: &str) -> usize {
    if grapheme == "\t" {
        4
    } else {
        terminal_grapheme_width(grapheme)
    }
}

/// Runs the agent display lines are error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn agent_display_lines_are_error(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        line.contains("provider_error")
            || line.contains("hook_blocked")
            || line.contains("failed")
            || line.contains("error:")
    })
}

/// Runs the agent display lines are low level status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn agent_display_lines_are_low_level_status(lines: &[String]) -> bool {
    let mut nonempty = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty());
    let Some(first) = nonempty.next() else {
        return false;
    };
    let first_is_low_level =
        first == "agent-shell:turn_started" || agent_display_line_is_low_level_status(first);
    first_is_low_level && nonempty.all(agent_display_line_is_low_level_status)
}

/// Runs the agent display line is low level status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn agent_display_line_is_low_level_status(line: &str) -> bool {
    line.starts_with("agent: turn ")
        || line.starts_with("agent: context ")
        || line.starts_with("agent: provider ")
        || line.starts_with("agent: dispatched ")
        || line.starts_with("agent: recorded ")
        || line.starts_with("agent: waiting ")
        || line.starts_with("agent: submitting ")
}

/// Runs the agent prompt error display lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn agent_prompt_error_display_lines(error: &MezError) -> Vec<String> {
    vec![format!(
        "agent command error: {} ({})",
        error.message(),
        runtime_mezzanine_error_code(error.kind())
    )]
}

/// Runs the overlay styled lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn overlay_styled_lines(
    view: &mut RenderedClientView,
    row_start: usize,
    column_start: usize,
    columns: usize,
    rows: usize,
    lines: &[TerminalStyledLine],
) {
    if columns == 0 || rows == 0 {
        return;
    }
    for row_offset in 0..rows {
        let row_index = row_start.saturating_add(row_offset);
        let Some(row) = view.lines.get_mut(row_index) else {
            break;
        };
        let line = lines.get(row_offset);
        let text = line.map(|line| line.text.as_str()).unwrap_or_default();
        overlay_text_cells(row, column_start, columns, text);
        let Some(style_spans) = view.line_style_spans.get_mut(row_index) else {
            continue;
        };
        overlay_fixed_column_style_spans(
            style_spans,
            column_start,
            columns,
            line.map(|line| line.style_spans.as_slice())
                .unwrap_or_default(),
        );
    }
}

/// Appends one agent presentation line with the right color span boundaries.
pub(crate) fn append_styled_agent_terminal_line(
    bytes: &mut String,
    style: AgentTerminalPresentationStyle,
    line: &str,
    ui_theme: &UiTheme,
) {
    let line = sanitized_agent_terminal_line(line);
    bytes.push_str(&style.sgr_prefix(ui_theme));
    bytes.push_str(AGENT_TERMINAL_MESSAGE_PREFIX);
    let Some(indicator) = style.speaker_indicator() else {
        bytes.push_str(&line);
        return;
    };
    if let Some(rest) = line.strip_prefix(indicator) {
        bytes.push_str(indicator);
        bytes.push_str("\x1b[0m");
        bytes.push_str(rest);
    } else {
        bytes.push_str("\x1b[0m");
        bytes.push_str(&line);
    }
}

/// Appends one transformed agent line, styling the gutter/label and body spans.
pub(crate) fn append_styled_agent_terminal_rendered_line(
    bytes: &mut String,
    style: AgentTerminalPresentationStyle,
    line: &RichTextLine,
    ui_theme: &UiTheme,
) {
    let line_text = sanitized_agent_terminal_line(&line.display);
    let label_rendition = agent_terminal_label_rendition(style, ui_theme);
    bytes.push_str(&agent_terminal_sgr_sequence(label_rendition));
    bytes.push_str(AGENT_TERMINAL_MESSAGE_PREFIX);
    let indicator_width = style
        .speaker_indicator()
        .filter(|indicator| line_text.starts_with(indicator))
        .map(agent_terminal_text_width)
        .unwrap_or_default();
    let mut active = label_rendition;
    let mut column = 0usize;
    for grapheme in UnicodeSegmentation::graphemes(line_text.as_str(), true) {
        let width = agent_terminal_grapheme_width(grapheme);
        if width == 0 {
            bytes.push_str(grapheme);
            continue;
        }
        let rendition = if column < indicator_width {
            label_rendition
        } else {
            rendered_line_rendition_at(&line.style_spans, column)
        };
        if rendition != active {
            bytes.push_str(&agent_terminal_sgr_sequence(rendition));
            active = rendition;
        }
        bytes.push_str(grapheme);
        column = column.saturating_add(width);
    }
}

/// Returns the themed rendition used for an agent gutter and optional label.
pub(crate) fn agent_terminal_label_rendition(
    style: AgentTerminalPresentationStyle,
    ui_theme: &UiTheme,
) -> GraphicRendition {
    let mut rendition = agent_text_foreground_rendition(style.color_pair(ui_theme));
    match style {
        AgentTerminalPresentationStyle::Status | AgentTerminalPresentationStyle::DiffContext => {
            rendition.dim = true;
        }
        AgentTerminalPresentationStyle::UserPrompt
        | AgentTerminalPresentationStyle::Assistant
        | AgentTerminalPresentationStyle::Error
        | AgentTerminalPresentationStyle::Command
        | AgentTerminalPresentationStyle::DiffHeader
        | AgentTerminalPresentationStyle::DiffAddition
        | AgentTerminalPresentationStyle::DiffDeletion => {
            rendition.bold = true;
        }
        AgentTerminalPresentationStyle::CommandDisplay => {}
    }
    rendition
}

/// Returns the presentation rendition active at one display column.
pub(crate) fn rendered_line_rendition_at(
    spans: &[TerminalStyleSpan],
    column: usize,
) -> GraphicRendition {
    spans
        .iter()
        .filter(|span| column >= span.start && column < span.start.saturating_add(span.length))
        .fold(GraphicRendition::default(), |active, span| {
            merge_agent_rendered_line_renditions(active, span.rendition)
        })
}

/// Merges layered rendered-line style spans without treating partial overlays
/// as full terminal-state replacements.
pub(super) fn merge_agent_rendered_line_renditions(
    mut active: GraphicRendition,
    overlay: GraphicRendition,
) -> GraphicRendition {
    if overlay.background.is_some() {
        return overlay;
    }
    active.bold |= overlay.bold;
    active.dim |= overlay.dim;
    active.italic |= overlay.italic;
    active.underline |= overlay.underline;
    active.double_underline |= overlay.double_underline;
    active.strikethrough |= overlay.strikethrough;
    active.inverse |= overlay.inverse;
    active.hidden |= overlay.hidden;
    if overlay.foreground.is_some() {
        active.foreground = overlay.foreground;
    }
    if overlay.background.is_some() {
        active.background = overlay.background;
    }
    active
}

/// Verifies syntax-token foreground spans preserve surrounding diff emphasis.
///
/// Apply-patch previews can layer syntax colors over diff addition/deletion
/// styling. The focused pane path renders from the live terminal screen, so
/// dropping the base diff attributes at colored tokens can make those symbols
/// visually diverge from the same row in unfocused or copy-mode redraws.
#[cfg(test)]
#[test]
pub(super) fn rendered_line_rendition_at_merges_diff_base_with_syntax_foreground() {
    let base = GraphicRendition {
        foreground: Some(TerminalColor::Indexed(2)),
        bold: true,
        ..GraphicRendition::default()
    };
    let syntax = GraphicRendition {
        foreground: Some(TerminalColor::Indexed(6)),
        ..GraphicRendition::default()
    };
    let spans = [
        TerminalStyleSpan {
            start: 0,
            length: 20,
            rendition: base,
        },
        TerminalStyleSpan {
            start: 9,
            length: 4,
            rendition: syntax,
        },
    ];

    let rendition = rendered_line_rendition_at(&spans, 10);

    assert_eq!(rendition.foreground, syntax.foreground);
    assert!(rendition.bold);
}
