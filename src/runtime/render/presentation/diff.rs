//! Unified-diff cleanup, syntax projection, and bounded rendering.

use super::actions::{bounded_agent_action_result_display_lines, truncate_to_utf8_boundary};
use super::style::{
    AGENT_ACTION_RESULT_DISPLAY_MAX_BYTES, AGENT_ACTION_RESULT_DISPLAY_MAX_LINES,
    AgentTerminalPresentationStyle,
};
use super::text::{
    agent_terminal_label_rendition, agent_terminal_text_width, sanitized_agent_terminal_line,
};
use super::{
    AgentAction, AgentActionPayload, DiffDisplayLine, DiffDisplaySection, RichTextLine,
    RichTextLineKind, ShellClassification, SyntaxHighlighter, SyntaxTheme, SyntaxThemePalette,
    TerminalColor, TerminalStyleSpan, UiTheme, append_syntax_spans, diff_highlighter_for_path,
    diff_section_path, format_diff_display_line, parse_unified_diff_sections,
    syntax_highlighter_for_extension, syntax_theme, wrap_rich_text_line_to_width,
};
use mez_mux::render::push_or_extend_style_span;

/// Chooses the presentation style for one generated diff preview line.
///
/// Semantic action diff lines use standard unified diff markers, while older
/// path-only previews use fixed-width old/new line number columns followed by a
/// marker. Recognizing both forms lets the pane transcript color additions and
/// deletions without requiring raw ANSI from the hidden shell transaction to
/// reach the user view.
pub(in crate::runtime::render) fn agent_diff_line_style(
    line: &str,
) -> AgentTerminalPresentationStyle {
    if line.starts_with("diff --")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("@@ ")
    {
        return AgentTerminalPresentationStyle::DiffHeader;
    }
    if line.starts_with('+') {
        return AgentTerminalPresentationStyle::DiffAddition;
    }
    if line.starts_with('-') {
        return AgentTerminalPresentationStyle::DiffDeletion;
    }
    match line.as_bytes().get(14).copied() {
        Some(b'+') => AgentTerminalPresentationStyle::DiffAddition,
        Some(b'-') => AgentTerminalPresentationStyle::DiffDeletion,
        _ => AgentTerminalPresentationStyle::DiffContext,
    }
}

/// Returns true when an action already emits a diff-shaped preview.
pub(in crate::runtime::render) fn agent_action_result_uses_diff_preview(
    action: &AgentAction,
) -> bool {
    matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Builds readable styled diff display lines from raw shell diff output.
///
/// Semantic mutation helpers execute inside a PTY, so their captured output may
/// include prompt redraws, wrapper variables, and echoed command fragments
/// around the actual diff. The pane should present the semantic change, not the
/// mechanics used to collect it.
#[cfg(test)]
pub(in crate::runtime::render) fn readable_agent_diff_display_lines(
    text: &str,
    ui_theme: &UiTheme,
) -> Vec<RichTextLine> {
    readable_agent_diff_display_lines_for_width(text, ui_theme, usize::MAX)
}

/// Builds readable styled diff display lines and wraps them to a display width.
///
/// # Parameters
/// - `text`: Raw hidden-shell diff output.
/// - `ui_theme`: The active UI theme.
/// - `display_width`: Cells available after the agent transcript gutter.
pub(in crate::runtime::render) fn readable_agent_diff_display_lines_for_width(
    text: &str,
    ui_theme: &UiTheme,
    display_width: usize,
) -> Vec<RichTextLine> {
    let source_lines = cleaned_agent_diff_source_lines(text);
    let sections = parse_unified_diff_sections(&source_lines);
    let mut lines = if sections.is_empty() {
        parse_agent_path_delta_display_lines(&source_lines, ui_theme)
    } else {
        render_agent_unified_diff_sections(&sections, ui_theme)
    };
    if lines.is_empty() {
        lines = bounded_agent_action_result_display_lines(&source_lines.join("\n"))
            .into_iter()
            .map(|line| {
                rendered_agent_diff_plain_line(agent_diff_line_style(&line), &line, ui_theme)
            })
            .collect();
    }
    let wrapped = lines
        .into_iter()
        .flat_map(|line| wrap_rich_text_line_to_width(line, display_width.max(1)))
        .collect();
    bound_agent_diff_display_lines(wrapped)
}

/// Removes Mezzanine wrapper and prompt echo lines around a diff.
pub(in crate::runtime::render) fn cleaned_agent_diff_source_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen_diff = false;
    for raw_line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let preserves_diff_prefix =
            seen_diff && matches!(raw_line.chars().next(), Some(' ' | '+' | '-' | '\\'));
        let line = if preserves_diff_prefix {
            raw_line
        } else {
            strip_agent_diff_prompt_prefix(raw_line)
        };
        let is_diff_body_line =
            seen_diff && matches!(line.chars().next(), Some(' ' | '+' | '-' | '\\'));
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if is_diff_body_line {
                lines.push(line.to_string());
            }
            continue;
        }
        if !is_diff_body_line && agent_diff_line_is_prompt_glyph(trimmed) {
            continue;
        }
        if !is_diff_body_line && agent_diff_line_is_wrapper_traffic(trimmed) {
            continue;
        }
        let starts_diff = trimmed.starts_with("diff --")
            || trimmed.starts_with("--- ")
            || trimmed.starts_with("+++ ")
            || trimmed.starts_with("@@ ");
        if starts_diff {
            seen_diff = true;
        }
        if !seen_diff {
            continue;
        }
        lines.push(line.to_string());
    }
    lines
}

/// Strips prompt glyphs that can be echoed by the shell around commands.
pub(in crate::runtime::render) fn strip_agent_diff_prompt_prefix(line: &str) -> &str {
    let mut remaining = line.trim_start();
    loop {
        let trimmed = remaining.trim_start();
        if let Some(next) = trimmed.strip_prefix('') {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix('∙') {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("$ ") {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("> ") {
            remaining = next;
            continue;
        }
        return trimmed;
    }
}

/// Returns true when a line only contains decorative prompt glyphs.
pub(in crate::runtime::render) fn agent_diff_line_is_prompt_glyph(trimmed: &str) -> bool {
    trimmed
        .chars()
        .all(|ch| matches!(ch, '' | '∙' | ' ' | '\t'))
}

/// Returns true for shell wrapper echo that should never appear in diff output.
pub(in crate::runtime::render) fn agent_diff_line_is_wrapper_traffic(trimmed: &str) -> bool {
    [
        "MEZ_MARKER_TOKEN",
        "MEZ_TURN",
        "MEZ_AGENT",
        "MEZ_PANE",
        "MEZ_STATUS",
        "MEZ_RESTORE_",
        "MEZ_HISTORY_",
        "HISTFILE=/dev/null",
        "MEZ_COMMAND_",
        "mez_marker=",
        "printf '\\033]133",
        "command printf '\\033]133",
        "env -u MEZ_MARKER_TOKEN",
        "command env -u MEZ_MARKER_TOKEN",
        "unset MEZ_",
        "set +o history",
        "set -o history",
        "history -d",
    ]
    .iter()
    .any(|marker| trimmed.contains(marker))
}

/// Renders parsed unified diff sections into visible unified-diff previews.
pub(in crate::runtime::render) fn render_agent_unified_diff_sections(
    sections: &[DiffDisplaySection],
    ui_theme: &UiTheme,
) -> Vec<RichTextLine> {
    let mut rendered = Vec::new();
    let syntax_theme = agent_diff_syntax_theme(ui_theme);
    for section in sections {
        rendered.push(rendered_agent_diff_plain_line(
            AgentTerminalPresentationStyle::DiffHeader,
            &format!("--- {}", section.old_label),
            ui_theme,
        ));
        rendered.push(rendered_agent_diff_plain_line(
            AgentTerminalPresentationStyle::DiffHeader,
            &format!("+++ {}", section.new_label),
            ui_theme,
        ));
        let mut highlighter = diff_highlighter_for_path(diff_section_path(section), &syntax_theme);
        for (index, line) in section.lines.iter().enumerate() {
            for (_, hunk_header) in section
                .hunk_headers
                .iter()
                .filter(|(line_index, _)| *line_index == index)
            {
                rendered.push(rendered_agent_diff_plain_line(
                    AgentTerminalPresentationStyle::DiffHeader,
                    hunk_header,
                    ui_theme,
                ));
            }
            rendered.push(render_agent_diff_display_line(
                line,
                highlighter.as_mut(),
                ui_theme,
            ));
        }
    }
    rendered
}

/// Renders one parsed hunk line with a diff gutter and file-aware code spans.
pub(in crate::runtime::render) fn render_agent_diff_display_line(
    line: &DiffDisplayLine,
    highlighter: Option<&mut SyntaxHighlighter<'_>>,
    ui_theme: &UiTheme,
) -> RichTextLine {
    let display = format_diff_display_line(line);
    let marker_style = agent_diff_display_line_style(line.marker);
    let marker_rendition = agent_terminal_label_rendition(marker_style, ui_theme);
    let mut rendered = RichTextLine {
        display,
        style_spans: Vec::new(),
        copy_text: None,
        kind: RichTextLineKind::Normal,
    };
    push_or_extend_style_span(
        &mut rendered.style_spans,
        TerminalStyleSpan {
            start: 0,
            length: rendered.display.chars().count(),
            rendition: marker_rendition,
        },
    );
    if let Some(highlighter) = highlighter {
        append_syntax_spans(&mut rendered, 15, &line.text, highlighter);
    }
    rendered
}

/// Returns the presentation style for one parsed diff hunk line.
pub(in crate::runtime::render) fn agent_diff_display_line_style(
    marker: char,
) -> AgentTerminalPresentationStyle {
    match marker {
        '+' => AgentTerminalPresentationStyle::DiffAddition,
        '-' => AgentTerminalPresentationStyle::DiffDeletion,
        _ => AgentTerminalPresentationStyle::DiffContext,
    }
}

/// Creates a syntax highlighter for shell command previews.
pub(in crate::runtime::render) fn agent_shell_command_highlighter<'a>(
    classification: ShellClassification,
    theme: &'a SyntaxTheme,
) -> Option<SyntaxHighlighter<'a>> {
    let extensions = match classification {
        ShellClassification::Fish => &["fish"][..],
        ShellClassification::Bash => &["bash", "sh"][..],
        ShellClassification::Zsh => &["zsh", "sh"][..],
        ShellClassification::PosixSh | ShellClassification::UnknownUnix => &["sh"][..],
    };
    extensions
        .iter()
        .find_map(|extension| syntax_highlighter_for_extension(extension, theme))
}

/// Maps configured UI syntax slots onto the neutral mux palette.
pub(super) fn agent_syntax_theme_palette(
    ui_theme: &UiTheme,
    background: Option<TerminalColor>,
) -> SyntaxThemePalette {
    SyntaxThemePalette {
        plain: ui_theme.colors.syntax_plain.foreground,
        background,
        comment: ui_theme.colors.syntax_comment.foreground,
        string: ui_theme.colors.syntax_string.foreground,
        number: ui_theme.colors.syntax_number.foreground,
        keyword: ui_theme.colors.syntax_keyword.foreground,
        r#type: ui_theme.colors.syntax_type.foreground,
        function: ui_theme.colors.syntax_function.foreground,
        operator: ui_theme.colors.syntax_operator.foreground,
    }
}

/// Builds the syntax theme used for shell command previews.
pub(in crate::runtime::render) fn agent_command_syntax_theme(ui_theme: &UiTheme) -> SyntaxTheme {
    syntax_theme(
        &format!("mezzanine-{}", ui_theme.name),
        agent_syntax_theme_palette(ui_theme, Some(ui_theme.colors.syntax_plain.background)),
    )
}

/// Builds the syntax theme used for terminal diff body highlighting.
pub(in crate::runtime::render) fn agent_diff_syntax_theme(ui_theme: &UiTheme) -> SyntaxTheme {
    syntax_theme(
        &format!("mezzanine-{}", ui_theme.name),
        agent_syntax_theme_palette(ui_theme, None),
    )
}

/// Parses and renders simple path-only delta output.
pub(in crate::runtime::render) fn parse_agent_path_delta_display_lines(
    lines: &[String],
    ui_theme: &UiTheme,
) -> Vec<RichTextLine> {
    let mut rendered = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let Some(title) = lines[index].strip_prefix("diff -- ") else {
            index += 1;
            continue;
        };
        let mut added = Vec::new();
        let mut removed = Vec::new();
        index += 1;
        while index < lines.len() && !lines[index].starts_with("diff -- ") {
            if let Some(path) = lines[index].strip_prefix("+ ") {
                added.push(path.trim().to_string());
            } else if let Some(path) = lines[index].strip_prefix("- ") {
                removed.push(path.trim().to_string());
            }
            index += 1;
        }
        if added.is_empty() && removed.is_empty() {
            continue;
        }
        rendered.push(rendered_agent_diff_plain_line(
            AgentTerminalPresentationStyle::DiffHeader,
            &format!(
                "• {} {} (+{} -{})",
                agent_path_delta_verb(title),
                agent_path_delta_header_path(&added, &removed),
                added.len(),
                removed.len()
            ),
            ui_theme,
        ));
        for path in &removed {
            rendered.push(rendered_agent_diff_plain_line(
                AgentTerminalPresentationStyle::DiffDeletion,
                &format!("         - {path}"),
                ui_theme,
            ));
        }
        for path in &added {
            rendered.push(rendered_agent_diff_plain_line(
                AgentTerminalPresentationStyle::DiffAddition,
                &format!("         + {path}"),
                ui_theme,
            ));
        }
    }
    rendered
}

/// Returns a display verb for a path-only delta title.
pub(in crate::runtime::render) fn agent_path_delta_verb(title: &str) -> &'static str {
    if title.contains("create") {
        "Created"
    } else if title.contains("delete") {
        "Deleted"
    } else if title.contains("move") {
        "Moved"
    } else {
        "Changed"
    }
}

/// Returns the compact display path for a path-only delta section.
pub(in crate::runtime::render) fn agent_path_delta_header_path<'a>(
    added: &'a [String],
    removed: &'a [String],
) -> &'a str {
    added
        .first()
        .or_else(|| removed.first())
        .map(String::as_str)
        .unwrap_or("paths")
}

/// Builds a rendered diff line whose entire body uses one diff style.
pub(in crate::runtime::render) fn rendered_agent_diff_plain_line(
    style: AgentTerminalPresentationStyle,
    line: &str,
    ui_theme: &UiTheme,
) -> RichTextLine {
    let display = sanitized_agent_terminal_line(line);
    let length = agent_terminal_text_width(display.as_str());
    let mut rendered = RichTextLine {
        display,
        style_spans: Vec::new(),
        copy_text: None,
        kind: RichTextLineKind::Normal,
    };
    push_or_extend_style_span(
        &mut rendered.style_spans,
        TerminalStyleSpan {
            start: 0,
            length,
            rendition: agent_terminal_label_rendition(style, ui_theme),
        },
    );
    rendered
}

/// Bounds rendered diff display lines for the pane buffer.
pub(in crate::runtime::render) fn bound_agent_diff_display_lines(
    lines: Vec<RichTextLine>,
) -> Vec<RichTextLine> {
    let mut bounded = Vec::new();
    let mut used_bytes = 0usize;
    for (index, mut line) in lines.into_iter().enumerate() {
        if index >= AGENT_ACTION_RESULT_DISPLAY_MAX_LINES {
            bounded.push(RichTextLine {
                display: "[mez: diff truncated for pane display]".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: RichTextLineKind::Normal,
            });
            break;
        }
        let remaining = AGENT_ACTION_RESULT_DISPLAY_MAX_BYTES.saturating_sub(used_bytes);
        if remaining == 0 {
            bounded.push(RichTextLine {
                display: "[mez: diff truncated for pane display]".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: RichTextLineKind::Normal,
            });
            break;
        }
        line.display = sanitized_agent_terminal_line(&line.display);
        if line.display.len() > remaining {
            line.display = truncate_to_utf8_boundary(&line.display, remaining);
            line.display.push_str("...");
            line.style_spans
                .retain(|span| span.start < agent_terminal_text_width(line.display.as_str()));
            bounded.push(line);
            bounded.push(RichTextLine {
                display: "[mez: diff truncated for pane display]".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: RichTextLineKind::Normal,
            });
            break;
        }
        used_bytes = used_bytes
            .saturating_add(line.display.len())
            .saturating_add(1);
        if !line.display.trim().is_empty() {
            bounded.push(line);
        }
    }
    bounded
}
