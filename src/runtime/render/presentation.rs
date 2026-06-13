//! Agent transcript and action-result presentation helpers.
//!
//! This module owns pure formatting for model-authored pane transcript content,
//! command previews, markdown rendering, diff previews, and bounded action
//! result display. Keeping these helpers outside the runtime service facade
//! makes visible output behavior easier to test without mixing it with pane
//! state transitions.

use super::super::{
    MezError, RenderedClientView, ShellClassification, runtime_mezzanine_error_code,
};
use super::geometry::overlay_text_cells;
use super::*;
use std::{str::FromStr, sync::LazyLock};

use crate::agent::{AgentAction, AgentActionPayload, apply_patch_touched_paths};
use crate::terminal::{
    AGENT_COPY_SKIP_LINE, AGENT_COPY_WRAP_CONTINUATION, GraphicRendition, TerminalColor,
    TerminalStyleSpan, TerminalStyledLine, UiColorPair, UiTheme, encode_agent_copy_source_line,
    overlay_fixed_column_style_spans, terminal_grapheme_width,
};
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SyntectColor, FontStyle, ScopeSelectors, Style as SyntectStyle, StyleModifier, Theme,
    ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Presentation-only rendering of one assistant output line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentRenderedLine {
    /// Text injected into the pane buffer after the Mezzanine gutter.
    pub(super) display: String,
    /// Style spans for the displayed text, excluding the gutter.
    pub(super) style_spans: Vec<TerminalStyleSpan>,
    /// Optional raw markdown text to use when copy mode selects this line.
    pub(super) copy_text: Option<String>,
    /// Structural presentation metadata that must not be inferred from glyphs.
    pub(super) kind: AgentRenderedLineKind,
}

/// Structural kind for one rendered presentation row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentRenderedLineKind {
    /// Ordinary rendered text with no special wrapping behavior.
    Normal,
    /// Synthetic frame row displayed above one rendered markdown block.
    MarkdownFrame,
    /// First physical row for one source markdown table row.
    MarkdownTableRow,
    /// Continuation physical row for a wrapped source markdown table row.
    MarkdownTableContinuation,
    /// Separator row generated from the markdown table delimiter line.
    MarkdownTableSeparator,
}

impl AgentRenderedLineKind {
    /// Returns whether this row is part of markdown table presentation.
    fn is_markdown_table(self) -> bool {
        matches!(
            self,
            Self::MarkdownTableRow | Self::MarkdownTableContinuation | Self::MarkdownTableSeparator
        )
    }

    /// Returns whether this row should consume one raw markdown source line.
    fn consumes_markdown_source_line(self) -> bool {
        !matches!(self, Self::MarkdownFrame | Self::MarkdownTableContinuation)
    }

    /// Returns the row kind to use for a generic wrapped continuation.
    fn continuation(self) -> Self {
        if self.is_markdown_table() {
            Self::MarkdownTableContinuation
        } else {
            Self::Normal
        }
    }
}

/// Maximum display width used for agent-rendered transcript presentation.
pub(super) const AGENT_TERMINAL_PRESENTATION_MAX_COLUMNS: usize = 120;
/// Light foreground-only color used for inline markdown on dark surfaces.
pub(super) const MARKDOWN_LIGHT_NEUTRAL_FOREGROUND: TerminalColor =
    TerminalColor::Rgb(0xe6, 0xe6, 0xe6);
/// Dark foreground-only color used for inline markdown on light surfaces.
pub(super) const MARKDOWN_DARK_NEUTRAL_FOREGROUND: TerminalColor =
    TerminalColor::Rgb(0x42, 0x42, 0x42);
/// Muted foreground-only color used for table alternation on light surfaces.
pub(super) const MARKDOWN_DARK_MUTED_FOREGROUND: TerminalColor =
    TerminalColor::Rgb(0x5a, 0x5a, 0x5a);
/// Built-in syntax set used for file-aware diff and shell command highlighting.
pub(super) static AGENT_DIFF_SYNTAX_SET: LazyLock<SyntaxSet> =
    LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Carries Agent Terminal Presentation Style state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTerminalPresentationStyle {
    /// Represents the User Prompt case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UserPrompt,
    /// Represents the Assistant case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Assistant,
    /// Represents the Status case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Status,
    /// Represents the Error case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Error,
    /// Represents the Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Command,
    /// Represents markdown or rich informational output produced by an agent shell command.
    ///
    /// Command display blocks are not model-authored assistant messages, so they
    /// render without an `agent>` label and use a neutral gutter.
    CommandDisplay,
    /// Represents diff metadata such as file labels and hunk headers.
    ///
    /// Mutating semantic actions use this style for bounded change previews
    /// rendered after hidden shell execution has completed.
    DiffHeader,
    /// Represents added lines in a mutating semantic action preview.
    ///
    /// The style intentionally reuses the user/accent color family so additions
    /// are visible without introducing another required theme slot.
    DiffAddition,
    /// Represents removed lines in a mutating semantic action preview.
    ///
    /// The style intentionally reuses the error color family so removals read as
    /// destructive or subtractive change without adding a new theme slot.
    DiffDeletion,
    /// Represents unchanged context lines in a mutating semantic action preview.
    ///
    /// Context stays visually quieter than additions and deletions while
    /// retaining enough contrast to make line numbers and nearby text readable.
    DiffContext,
}

impl AgentTerminalPresentationStyle {
    /// Returns the theme color pair for this agent transcript presentation.
    fn color_pair(self, ui_theme: &UiTheme) -> UiColorPair {
        match self {
            Self::UserPrompt => ui_theme.colors.agent_transcript_user,
            Self::Assistant => ui_theme.colors.agent_transcript_assistant,
            Self::Status => ui_theme.colors.agent_transcript_status,
            Self::Error => ui_theme.colors.agent_transcript_error,
            Self::Command => ui_theme.colors.agent_transcript_command,
            Self::CommandDisplay => ui_theme.colors.frame_fill,
            Self::DiffHeader => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_command.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
            Self::DiffAddition => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_user.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
            Self::DiffDeletion => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_error.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
            Self::DiffContext => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_status.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
        }
    }

    /// Returns the SGR prefix used before rendering a transcript gutter.
    fn sgr_prefix(self, ui_theme: &UiTheme) -> String {
        let mut rendition = agent_text_foreground_rendition(self.color_pair(ui_theme));
        match self {
            Self::Status | Self::DiffContext => rendition.dim = true,
            Self::UserPrompt
            | Self::Assistant
            | Self::Error
            | Self::Command
            | Self::DiffHeader
            | Self::DiffAddition
            | Self::DiffDeletion => {
                rendition.bold = true;
            }
            Self::CommandDisplay => {}
        }
        agent_terminal_sgr_sequence(rendition)
    }

    /// Returns the colored speaker label for transcript styles that should
    /// reset to default text after the name indicator.
    fn speaker_indicator(self) -> Option<&'static str> {
        match self {
            Self::UserPrompt => Some("user> "),
            Self::Assistant => Some("mez> "),
            Self::Status
            | Self::Error
            | Self::Command
            | Self::CommandDisplay
            | Self::DiffHeader
            | Self::DiffAddition
            | Self::DiffDeletion
            | Self::DiffContext => None,
        }
    }

    /// Returns the stable persistence name for this presentation style.
    pub(super) fn persistence_name(self) -> &'static str {
        match self {
            Self::UserPrompt => "user-prompt",
            Self::Assistant => "assistant",
            Self::Status => "status",
            Self::Error => "error",
            Self::Command => "command",
            Self::CommandDisplay => "command-display",
            Self::DiffHeader => "diff-header",
            Self::DiffAddition => "diff-addition",
            Self::DiffDeletion => "diff-deletion",
            Self::DiffContext => "diff-context",
        }
    }

    /// Restores one persisted presentation style name.
    pub(super) fn from_persistence_name(name: &str) -> Option<Self> {
        match name {
            "user-prompt" => Some(Self::UserPrompt),
            "assistant" => Some(Self::Assistant),
            "status" => Some(Self::Status),
            "error" => Some(Self::Error),
            "command" => Some(Self::Command),
            "command-display" => Some(Self::CommandDisplay),
            "diff-header" => Some(Self::DiffHeader),
            "diff-addition" => Some(Self::DiffAddition),
            "diff-deletion" => Some(Self::DiffDeletion),
            "diff-context" => Some(Self::DiffContext),
            _ => None,
        }
    }
}

/// Returns a foreground-only rendition for agent-authored transcript text.
///
/// Agent transcript content is injected into pane buffers as ordinary terminal
/// text. It should not paint a background over the user's terminal theme.
pub(super) fn agent_text_foreground_rendition(pair: UiColorPair) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(pair.foreground),
        ..GraphicRendition::default()
    }
}

/// Converts a graphic rendition to an SGR sequence for pane-buffer injection.
pub(super) fn agent_terminal_sgr_sequence(rendition: GraphicRendition) -> String {
    if rendition == GraphicRendition::default() {
        return "\x1b[0m".to_string();
    }
    let mut codes = vec!["0".to_string()];
    if rendition.bold {
        codes.push("1".to_string());
    }
    if rendition.dim {
        codes.push("2".to_string());
    }
    if rendition.italic {
        codes.push("3".to_string());
    }
    if rendition.underline {
        if rendition.double_underline {
            codes.push("21".to_string());
        } else {
            codes.push("4".to_string());
        }
    }
    if rendition.strikethrough {
        codes.push("9".to_string());
    }
    if rendition.inverse {
        codes.push("7".to_string());
    }
    if rendition.hidden {
        codes.push("8".to_string());
    }
    if let Some(color) = rendition.foreground {
        push_agent_terminal_sgr_color_codes(&mut codes, color, false);
    }
    if let Some(color) = rendition.background {
        push_agent_terminal_sgr_color_codes(&mut codes, color, true);
    }
    format!("\x1b[{}m", codes.join(";"))
}

/// Appends SGR foreground or background color parameters.
pub(super) fn push_agent_terminal_sgr_color_codes(
    codes: &mut Vec<String>,
    color: TerminalColor,
    background: bool,
) {
    match color {
        TerminalColor::Indexed(index) if index < 8 => {
            codes.push((u16::from(index) + if background { 40 } else { 30 }).to_string());
        }
        TerminalColor::Indexed(index) if index < 16 => {
            codes.push((u16::from(index - 8) + if background { 100 } else { 90 }).to_string());
        }
        TerminalColor::Indexed(index) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("5".to_string());
            codes.push(index.to_string());
        }
        TerminalColor::Rgb(red, green, blue) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("2".to_string());
            codes.push(red.to_string());
            codes.push(green.to_string());
            codes.push(blue.to_string());
        }
    }
}

/// Defines the AGENT TERMINAL MESSAGE PREFIX const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const AGENT_TERMINAL_MESSAGE_PREFIX: &str = "▐ ";
/// Editable prompt marker rendered after the agent terminal gutter.
pub(super) const AGENT_PROMPT_TEXT_PREFIX: &str = "mez> ";
/// Maximum action-result lines rendered directly into the pane buffer.
pub(super) const AGENT_ACTION_RESULT_DISPLAY_MAX_LINES: usize = 200;
/// Maximum action-result bytes rendered directly into the pane buffer.
pub(super) const AGENT_ACTION_RESULT_DISPLAY_MAX_BYTES: usize = 64 * 1024;

/// Runs the sanitized agent terminal line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn sanitized_agent_terminal_line(line: &str) -> String {
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
pub(super) fn prefixed_agent_terminal_lines(prefix: &str, text: &str) -> Vec<String> {
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
pub(super) fn wrapped_prefixed_agent_terminal_lines(
    prefix: &str,
    text: &str,
    display_width: usize,
) -> Vec<AgentRenderedLine> {
    let lines = prefixed_agent_terminal_lines(prefix, text)
        .into_iter()
        .map(|display| AgentRenderedLine {
            display,
            style_spans: Vec::new(),
            copy_text: None,
            kind: AgentRenderedLineKind::Normal,
        })
        .collect::<Vec<_>>();
    wrap_agent_rendered_lines_to_width(lines, display_width, display_width)
}

/// Returns true when a display-only `say` body is a raw Mezzanine patch example.
///
/// Markdown treats leading `***` as structural syntax in some contexts. Raw
/// patch examples should stay literal and copyable instead of being parsed as
/// markdown or an executable action.
pub(super) fn agent_say_text_is_displayed_patch_block(text: &str) -> bool {
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
pub(super) fn render_agent_markdown_body_lines(
    markdown: &str,
    ui_theme: &UiTheme,
    table_display_width: usize,
) -> Vec<AgentRenderedLine> {
    let trimmed = markdown.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return vec![AgentRenderedLine {
            display: "mez> ".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: AgentRenderedLineKind::Normal,
        }];
    }
    let table_body_display_width = table_display_width
        .saturating_sub(UnicodeWidthStr::width("mez> "))
        .max(1);
    prefix_agent_rendered_markdown_lines(render_markdown_preserving_source_blank_lines(
        trimmed,
        ui_theme,
        Some(table_body_display_width),
    ))
}

/// Renders runtime command markdown body lines without the surrounding frame.
pub(super) fn render_command_markdown_body_lines(
    markdown: &str,
    ui_theme: &UiTheme,
) -> Vec<AgentRenderedLine> {
    let trimmed = markdown.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Vec::new();
    }
    render_markdown_preserving_source_blank_lines(trimmed, ui_theme, None)
}

/// Wraps rendered markdown presentation lines to the pane-local display width.
///
/// # Parameters
/// - `lines`: Rendered markdown rows before pane-width wrapping.
/// - `display_width`: Capped display cells available after the transcript gutter.
/// - `table_display_width`: Terminal display cells available for table rows.
pub(super) fn wrap_agent_rendered_lines_to_width(
    lines: Vec<AgentRenderedLine>,
    display_width: usize,
    table_display_width: usize,
) -> Vec<AgentRenderedLine> {
    let display_width = display_width.max(1);
    let table_display_width = table_display_width.max(display_width).max(1);
    lines
        .into_iter()
        .flat_map(|line| {
            let effective_width = if markdown_rendered_line_is_table_row(&line) {
                table_display_width
            } else {
                display_width
            };
            wrap_agent_rendered_line_to_width(line, effective_width)
        })
        .collect()
}

/// Wraps one rendered markdown presentation line to a bounded display width.
///
/// # Parameters
/// - `line`: The rendered row to split.
/// - `display_width`: Maximum display cells available after the transcript gutter.
pub(super) fn wrap_agent_rendered_line_to_width(
    line: AgentRenderedLine,
    display_width: usize,
) -> Vec<AgentRenderedLine> {
    if agent_terminal_text_width(line.display.as_str()) <= display_width {
        return vec![line];
    }
    let continuation_indent = rendered_line_continuation_indent(&line.display, display_width);
    let continuation_width = agent_terminal_text_width(continuation_indent.as_str());
    let continuation_display_width = display_width.saturating_sub(continuation_width).max(1);
    let mut wrapped = Vec::new();
    let mut remaining = line.display.as_str();
    let mut display_start = 0usize;
    let mut first = true;
    while !remaining.is_empty() {
        let segment_width = if first {
            display_width
        } else {
            continuation_display_width
        };
        let minimum_break_column = if first {
            continuation_width
        } else {
            display_start
        };
        let Some(segment) = take_agent_rendered_display_segment(
            remaining,
            display_start,
            segment_width,
            minimum_break_column,
        ) else {
            break;
        };
        let display_prefix = if first {
            String::new()
        } else {
            continuation_indent.clone()
        };
        let display_prefix_width = agent_terminal_text_width(display_prefix.as_str());
        let segment_text = format!("{display_prefix}{}", segment.text);
        let style_spans = style_spans_for_agent_rendered_segment(
            &line.style_spans,
            segment.start_column,
            segment.end_column,
            display_prefix_width,
        );
        let copy_text = if first {
            line.copy_text.clone()
        } else if line
            .copy_text
            .as_deref()
            .is_some_and(|copy_text| copy_text != AGENT_COPY_SKIP_LINE)
        {
            Some(AGENT_COPY_WRAP_CONTINUATION.to_string())
        } else if line.copy_text.is_some() {
            Some(AGENT_COPY_SKIP_LINE.to_string())
        } else {
            None
        };
        wrapped.push(AgentRenderedLine {
            display: segment_text,
            style_spans,
            copy_text,
            kind: if first {
                line.kind
            } else {
                line.kind.continuation()
            },
        });
        remaining = &remaining[segment.bytes_consumed..];
        display_start = segment.end_column;
        first = false;
    }
    if wrapped.is_empty() {
        vec![line]
    } else {
        wrapped
    }
}

/// One display-cell-bounded segment from a rendered row.
pub(super) struct AgentRenderedDisplaySegment {
    /// Text included in the segment.
    text: String,
    /// Bytes consumed from the remaining source string.
    bytes_consumed: usize,
    /// Original display column where this segment begins.
    start_column: usize,
    /// Original display column one past the segment end.
    end_column: usize,
}

/// Takes one display-width-bounded segment from a rendered row.
///
/// # Parameters
/// - `text`: Remaining display text to split.
/// - `start_column`: Original display column of `text`.
/// - `display_width`: Maximum segment display width.
/// - `minimum_break_column`: Earliest original display column where whitespace
///   may be used as a wrap boundary.
pub(super) fn take_agent_rendered_display_segment(
    text: &str,
    start_column: usize,
    display_width: usize,
    minimum_break_column: usize,
) -> Option<AgentRenderedDisplaySegment> {
    if text.is_empty() {
        return None;
    }
    if agent_terminal_text_width(text) <= display_width {
        return Some(AgentRenderedDisplaySegment {
            text: text.to_string(),
            bytes_consumed: text.len(),
            start_column,
            end_column: start_column.saturating_add(agent_terminal_text_width(text)),
        });
    }
    if let Some((_, grapheme)) = UnicodeSegmentation::grapheme_indices(text, true).next()
        && agent_terminal_grapheme_width(grapheme) > display_width
    {
        return Some(AgentRenderedDisplaySegment {
            text: "…".to_string(),
            bytes_consumed: grapheme.len(),
            start_column,
            end_column: start_column.saturating_add(1),
        });
    }
    let mut width = 0usize;
    let mut boundary_consumed = 0usize;
    let mut boundary_width = 0usize;
    let mut last_space_break: Option<(usize, usize, usize)> = None;
    for (index, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
        let grapheme_width = agent_terminal_grapheme_width(grapheme);
        if width > 0 && width.saturating_add(grapheme_width) > display_width {
            break;
        }
        let next_consumed = index.saturating_add(grapheme.len());
        let next_width = width.saturating_add(grapheme_width);
        if grapheme.chars().all(char::is_whitespace) && width > 0 {
            let break_column = start_column.saturating_add(width);
            if break_column > minimum_break_column {
                last_space_break = Some((index, next_consumed, width));
            }
        }
        boundary_consumed = next_consumed;
        boundary_width = next_width;
        width = width.saturating_add(grapheme_width);
        if width >= display_width {
            break;
        }
    }
    if last_space_break.is_none() && boundary_consumed < text.len() {
        if let Some((_, grapheme)) = UnicodeSegmentation::grapheme_indices(text, true).next()
            && agent_terminal_grapheme_width(grapheme) > display_width
        {
            return Some(AgentRenderedDisplaySegment {
                text: "…".to_string(),
                bytes_consumed: grapheme.len(),
                start_column,
                end_column: start_column.saturating_add(1),
            });
        }
        return Some(AgentRenderedDisplaySegment {
            text: text.to_string(),
            bytes_consumed: text.len(),
            start_column,
            end_column: start_column.saturating_add(agent_terminal_text_width(text)),
        });
    }
    let (text_end, consumed, width) =
        if let Some((space_start, consumed_through_space, break_width)) = last_space_break {
            (space_start, consumed_through_space, break_width)
        } else {
            (boundary_consumed, boundary_consumed, boundary_width)
        };
    let output = text[..text_end].to_string();
    if output.is_empty() && boundary_consumed > 0 {
        return Some(AgentRenderedDisplaySegment {
            text: text[..boundary_consumed].to_string(),
            bytes_consumed: boundary_consumed,
            start_column,
            end_column: start_column.saturating_add(boundary_width),
        });
    }
    Some(AgentRenderedDisplaySegment {
        text: output,
        bytes_consumed: consumed,
        start_column,
        end_column: start_column.saturating_add(width),
    })
}

/// Produces style spans for a wrapped rendered-line segment.
///
/// # Parameters
/// - `spans`: Style spans from the unwrapped rendered row.
/// - `segment_start`: Original display column where the segment begins.
/// - `segment_end`: Original display column one past the segment end.
/// - `display_prefix_width`: Display cells inserted before this segment.
pub(super) fn style_spans_for_agent_rendered_segment(
    spans: &[TerminalStyleSpan],
    segment_start: usize,
    segment_end: usize,
    display_prefix_width: usize,
) -> Vec<TerminalStyleSpan> {
    spans
        .iter()
        .filter_map(|span| {
            let span_start = span.start;
            let span_end = span.start.saturating_add(span.length);
            let start = span_start.max(segment_start);
            let end = span_end.min(segment_end);
            if start >= end {
                return None;
            }
            Some(TerminalStyleSpan {
                start: start
                    .saturating_sub(segment_start)
                    .saturating_add(display_prefix_width),
                length: end.saturating_sub(start),
                rendition: span.rendition,
            })
        })
        .collect()
}

/// Returns the display-only indentation used after a markdown soft wrap.
///
/// # Parameters
/// - `display`: The unwrapped rendered line.
/// - `display_width`: Maximum available display cells.
pub(super) fn rendered_line_continuation_indent(display: &str, display_width: usize) -> String {
    if rendered_line_is_numbered_diff_row(display) {
        return " ".repeat(10.min(display_width.saturating_sub(1)));
    }
    if display.starts_with("user> ") {
        return " ".repeat(5.min(display_width.saturating_sub(1)));
    }
    let prompt = "mez> ";
    let indent_width = if let Some(rest) = display.strip_prefix(prompt) {
        agent_terminal_text_width(prompt) + markdown_local_continuation_indent_width(rest)
    } else {
        markdown_local_continuation_indent_width(display)
    };
    " ".repeat(indent_width.min(display_width.saturating_sub(1)))
}

/// Returns true when a rendered row uses the fixed diff hunk gutter.
///
/// # Parameters
/// - `display`: The rendered row to inspect.
pub(super) fn rendered_line_is_numbered_diff_row(display: &str) -> bool {
    let mut chars = display.chars();
    let gutter = chars.by_ref().take(8).collect::<String>();
    if !gutter.chars().all(|ch| ch == ' ' || ch.is_ascii_digit()) {
        return false;
    }
    matches!(
        (chars.next(), chars.next()),
        (Some(' '), Some('+' | '-' | ' '))
    )
}

/// Returns markdown-local continuation indentation for one rendered row.
///
/// # Parameters
/// - `display`: Rendered markdown text without any agent speaker prefix.
pub(super) fn markdown_local_continuation_indent_width(display: &str) -> usize {
    let mut width = 0usize;
    let mut byte_index = 0usize;
    for (index, grapheme) in UnicodeSegmentation::grapheme_indices(display, true) {
        if grapheme != " " && grapheme != "\t" {
            byte_index = index;
            break;
        }
        width = width.saturating_add(agent_terminal_grapheme_width(grapheme));
        byte_index = index.saturating_add(grapheme.len());
    }
    let mut rest = &display[byte_index..];
    while let Some(after_quote) = rest.strip_prefix("> ") {
        width = width.saturating_add(2);
        rest = after_quote;
    }
    if rest.starts_with("• ") {
        return width.saturating_add(2);
    }
    if rest.starts_with("[x] ") || rest.starts_with("[ ] ") {
        return width.saturating_add(4);
    }
    let ordered_marker_width = rest.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if ordered_marker_width > 0
        && rest
            .chars()
            .nth(ordered_marker_width)
            .is_some_and(|ch| ch == '.')
        && rest
            .chars()
            .nth(ordered_marker_width.saturating_add(1))
            .is_some_and(char::is_whitespace)
    {
        return width.saturating_add(ordered_marker_width).saturating_add(2);
    }
    width
}

/// Returns whether one rendered markdown row is part of a table.
///
/// # Parameters
/// - `line`: Rendered markdown row with structural presentation metadata.
pub(super) fn markdown_rendered_line_is_table_row(line: &AgentRenderedLine) -> bool {
    line.kind.is_markdown_table()
}

/// Keeps rendered markdown rows in the ordinary assistant transcript flow.
///
/// Assistant `say` output should not synthesize an extra divider row before the
/// rendered body, even when the body uses markdown presentation styling.
pub(super) fn frame_agent_markdown_lines(
    lines: Vec<AgentRenderedLine>,
    _display_width: usize,
) -> Vec<AgentRenderedLine> {
    lines
}

/// Builds copy text lines for rendered markdown presentation.
pub(super) fn markdown_block_copy_lines(
    rendered_lines: &[AgentRenderedLine],
    _body_rendered_count: usize,
    raw_body_copy_lines: Vec<String>,
) -> Vec<String> {
    let mut raw_lines = raw_body_copy_lines.into_iter().enumerate();
    let mut current_source_line = None;
    rendered_lines
        .iter()
        .map(|line| {
            if line
                .copy_text
                .as_deref()
                .is_some_and(|copy_text| copy_text == AGENT_COPY_SKIP_LINE)
            {
                return AGENT_COPY_SKIP_LINE.to_string();
            }
            if line.kind == AgentRenderedLineKind::MarkdownFrame {
                return line
                    .copy_text
                    .clone()
                    .unwrap_or_else(|| markdown_rendered_line_copy_text(line));
            }
            if line
                .copy_text
                .as_deref()
                .is_some_and(|copy_text| copy_text == AGENT_COPY_WRAP_CONTINUATION)
            {
                return current_source_line
                    .as_ref()
                    .map(|(source_index, raw_line): &(usize, String)| {
                        encode_agent_copy_source_line(*source_index, raw_line.as_str())
                    })
                    .unwrap_or_else(|| AGENT_COPY_SKIP_LINE.to_string());
            }
            if line.kind.consumes_markdown_source_line()
                && let Some((source_index, raw_line)) = raw_lines.next()
            {
                current_source_line = Some((source_index, raw_line.clone()));
                return encode_agent_copy_source_line(source_index, raw_line.as_str());
            }
            AGENT_COPY_SKIP_LINE.to_string()
        })
        .collect()
}

/// Returns one pane-buffer copy line for a rendered markdown presentation row.
pub(super) fn markdown_rendered_line_copy_text(line: &AgentRenderedLine) -> String {
    if line
        .copy_text
        .as_deref()
        .is_some_and(|copy_text| copy_text == AGENT_COPY_SKIP_LINE)
    {
        return AGENT_COPY_SKIP_LINE.to_string();
    }
    format!(
        "{AGENT_TERMINAL_MESSAGE_PREFIX}{}",
        line.copy_text.as_ref().unwrap_or(&line.display)
    )
}

/// Restores source-authored blank lines when the rendered body preserves line count.
pub(super) fn render_markdown_preserving_source_blank_lines(
    markdown: &str,
    ui_theme: &UiTheme,
    table_display_width: Option<usize>,
) -> Vec<AgentRenderedLine> {
    let rendered_lines = AgentMarkdownRenderer::render(markdown, ui_theme, table_display_width);
    let source_lines = markdown.lines().collect::<Vec<_>>();
    let nonblank_source_lines = source_lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .count();
    let rendered_source_line_count = rendered_lines
        .iter()
        .filter(|line| line.kind.consumes_markdown_source_line())
        .count();
    if nonblank_source_lines != rendered_source_line_count {
        return insert_blank_lines_above_markdown_headings(rendered_lines);
    }

    let mut rendered = rendered_lines.into_iter();
    let mut source_aligned_lines = Vec::new();
    for source_line in source_lines {
        if source_line.trim().is_empty() {
            source_aligned_lines.push(AgentRenderedLine {
                display: String::new(),
                style_spans: Vec::new(),
                copy_text: Some(String::new()),
                kind: AgentRenderedLineKind::Normal,
            });
            continue;
        }
        for mut rendered_line in rendered.by_ref() {
            if rendered_line.kind.consumes_markdown_source_line() {
                rendered_line.copy_text = Some(source_line.to_string());
                source_aligned_lines.push(rendered_line);
                break;
            }
            if rendered_line.copy_text.is_none() {
                rendered_line.copy_text = Some(AGENT_COPY_SKIP_LINE.to_string());
            }
            source_aligned_lines.push(rendered_line);
        }
    }
    source_aligned_lines.extend(rendered.map(|mut rendered_line| {
        if !rendered_line.kind.consumes_markdown_source_line() && rendered_line.copy_text.is_none()
        {
            rendered_line.copy_text = Some(AGENT_COPY_SKIP_LINE.to_string());
        }
        rendered_line
    }));
    insert_blank_lines_above_markdown_headings(source_aligned_lines)
}

/// Ensures every rendered markdown heading has a presentation blank line above it.
pub(super) fn insert_blank_lines_above_markdown_headings(
    lines: Vec<AgentRenderedLine>,
) -> Vec<AgentRenderedLine> {
    let mut spaced = Vec::with_capacity(lines.len().saturating_mul(2));
    for line in lines {
        if markdown_rendered_line_is_heading(&line)
            && spaced
                .last()
                .is_none_or(|previous: &AgentRenderedLine| !previous.display.trim().is_empty())
        {
            spaced.push(markdown_blank_line());
        }
        spaced.push(line);
    }
    spaced
}

/// Returns whether a rendered line came from an ATX markdown heading.
pub(super) fn markdown_rendered_line_is_heading(line: &AgentRenderedLine) -> bool {
    let Some(copy_text) = line.copy_text.as_deref() else {
        return false;
    };
    let trimmed = copy_text.trim_start();
    let marker_count = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&marker_count) {
        return false;
    }
    trimmed
        .chars()
        .nth(marker_count)
        .is_some_and(char::is_whitespace)
}

/// Builds one presentation-only blank markdown row.
pub(super) fn markdown_blank_line() -> AgentRenderedLine {
    AgentRenderedLine {
        display: String::new(),
        style_spans: Vec::new(),
        copy_text: Some(AGENT_COPY_SKIP_LINE.to_string()),
        kind: AgentRenderedLineKind::Normal,
    }
}

/// Returns the foreground used for inline markdown code on the active theme.
pub(super) fn markdown_inline_code_foreground(ui_theme: &UiTheme) -> TerminalColor {
    if markdown_surface_is_light(ui_theme) {
        MARKDOWN_DARK_NEUTRAL_FOREGROUND
    } else {
        MARKDOWN_LIGHT_NEUTRAL_FOREGROUND
    }
}

/// Returns the foreground used to distinguish alternating markdown table rows.
pub(super) fn markdown_table_alternate_row_foreground(ui_theme: &UiTheme) -> TerminalColor {
    if markdown_surface_is_light(ui_theme) {
        MARKDOWN_DARK_MUTED_FOREGROUND
    } else {
        MARKDOWN_LIGHT_NEUTRAL_FOREGROUND
    }
}

/// Returns whether markdown should use dark neutral text accents.
pub(super) fn markdown_surface_is_light(ui_theme: &UiTheme) -> bool {
    terminal_color_luminance(ui_theme.colors.agent_transcript_assistant.background)
        .or_else(|| terminal_color_luminance(ui_theme.colors.frame_fill.background))
        .is_some_and(|luminance| luminance >= 140)
}

/// Returns a simple perceptual luminance approximation for true-color values.
pub(super) fn terminal_color_luminance(color: TerminalColor) -> Option<u32> {
    let (red, green, blue) = terminal_color_rgb(color)?;
    Some((u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000)
}

/// Returns RGB components for true-color values.
pub(super) fn terminal_color_rgb(color: TerminalColor) -> Option<(u8, u8, u8)> {
    match color {
        TerminalColor::Rgb(red, green, blue) => Some((red, green, blue)),
        TerminalColor::Indexed(_) => None,
    }
}

/// Prefixes markdown body lines with the standard agent transcript label.
pub(super) fn prefix_agent_rendered_markdown_lines(
    lines: Vec<AgentRenderedLine>,
) -> Vec<AgentRenderedLine> {
    let body_lines = if lines.is_empty() {
        vec![AgentRenderedLine {
            display: String::new(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: AgentRenderedLineKind::Normal,
        }]
    } else {
        lines
    };
    let continuation = " ".repeat("mez> ".chars().count());
    let mut first_nonblank = true;
    body_lines
        .into_iter()
        .map(|mut line| {
            if line.display.is_empty() {
                if line.copy_text.as_deref() == Some(AGENT_COPY_SKIP_LINE) {
                    return line;
                }
                if line.copy_text.is_some() {
                    line.copy_text = Some(String::new());
                }
                return line;
            }
            let prefix = if first_nonblank {
                first_nonblank = false;
                "mez> ".to_string()
            } else {
                continuation.clone()
            };
            let prefix_width = UnicodeWidthStr::width(prefix.as_str());
            for span in &mut line.style_spans {
                span.start = span.start.saturating_add(prefix_width);
            }
            line.display = format!("{prefix}{}", line.display);
            if let Some(copy_text) = line.copy_text.take() {
                if copy_text == AGENT_COPY_SKIP_LINE {
                    line.copy_text = Some(copy_text);
                } else {
                    line.copy_text = Some(format!("{prefix}{copy_text}"));
                }
            }
            line
        })
        .collect()
}

/// Parser-backed CommonMark renderer for pane-buffer markdown presentation.
///
/// The renderer intentionally keeps the output terminal-native rather than
/// attempting HTML layout. It consumes the CommonMark event stream, applies
/// available terminal styles for inline semantics, and emits readable plain
/// text for block structures that have no direct terminal equivalent.
#[derive(Debug)]
pub(super) struct AgentMarkdownRenderer {
    lines: Vec<AgentRenderedLine>,
    current: AgentRenderedLine,
    table_display_width: Option<usize>,
    active: GraphicRendition,
    style_stack: Vec<GraphicRendition>,
    quote_depth: usize,
    list_stack: Vec<MarkdownListState>,
    continuation_prefix: Option<String>,
    link_stack: Vec<String>,
    image_stack: Vec<String>,
    table: Option<MarkdownTableState>,
    line_copy_prefix: Option<String>,
    link_foreground: TerminalColor,
    inline_code_foreground: TerminalColor,
    table_alternate_row_foreground: TerminalColor,
    diff_addition_foreground: TerminalColor,
    diff_deletion_foreground: TerminalColor,
    current_prefix_only: bool,
}

impl AgentMarkdownRenderer {
    /// Renders markdown using CommonMark plus the common GitHub-style extensions.
    fn render(
        markdown: &str,
        ui_theme: &UiTheme,
        table_display_width: Option<usize>,
    ) -> Vec<AgentRenderedLine> {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_FOOTNOTES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_SMART_PUNCTUATION);
        options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
        options.insert(Options::ENABLE_MATH);
        options.insert(Options::ENABLE_GFM);
        options.insert(Options::ENABLE_DEFINITION_LIST);
        options.insert(Options::ENABLE_SUPERSCRIPT);
        options.insert(Options::ENABLE_SUBSCRIPT);
        options.insert(Options::ENABLE_WIKILINKS);

        let mut renderer = Self::new(ui_theme, table_display_width);
        for event in Parser::new_ext(markdown, options) {
            renderer.handle_event(event);
        }
        renderer.finish_current_line();
        renderer.trim_trailing_blank_lines();
        renderer.lines
    }

    /// Handles one parser event, delegating table internals to table capture.
    fn handle_event(&mut self, event: Event<'_>) {
        if self.table.is_some() {
            self.handle_table_event(event);
            return;
        }
        match event {
            Event::Start(tag) => self.handle_start_tag(tag),
            Event::End(tag) => self.handle_end_tag(tag),
            Event::Text(text) => self.append_text(text.as_ref()),
            Event::Code(code) => self.append_code(code.as_ref()),
            Event::InlineMath(math) => self.append_inline_math(math.as_ref()),
            Event::DisplayMath(math) => self.append_display_math(math.as_ref()),
            Event::Html(html) => self.append_text(html.as_ref()),
            Event::InlineHtml(html) => self.handle_inline_html(html.as_ref()),
            Event::FootnoteReference(label) => self.append_text(&format!("[^{label}]")),
            Event::SoftBreak | Event::HardBreak => self.finish_current_line(),
            Event::Rule => {
                self.start_block();
                self.append_text("────────");
                self.finish_current_line();
            }
            Event::TaskListMarker(checked) => self.replace_current_task_marker(checked),
        }
    }

    /// Handles the start of one markdown tag.
    fn handle_start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if !self.current_prefix_only {
                    self.start_block();
                }
            }
            Tag::Heading { level, .. } => {
                self.start_block();
                self.line_copy_prefix = Some(format!("{} ", "#".repeat(level as usize)));
                self.push_style(|style| {
                    style.bold = true;
                    style.underline = true;
                });
            }
            Tag::BlockQuote(kind) => {
                self.start_block();
                self.quote_depth = self.quote_depth.saturating_add(1);
                if let Some(kind) = kind {
                    self.append_text(&format!("[{kind:?}] "));
                }
            }
            Tag::CodeBlock(_kind) => {
                self.start_block();
                self.push_style(|_style| {});
            }
            Tag::HtmlBlock => self.start_block(),
            Tag::List(start) => self.list_stack.push(MarkdownListState {
                next_number: start.unwrap_or(1),
                ordered: start.is_some(),
            }),
            Tag::Item => self.start_list_item(),
            Tag::FootnoteDefinition(label) => {
                self.start_block();
                self.append_text(&format!("[^{label}]: "));
            }
            Tag::DefinitionList => self.start_block(),
            Tag::DefinitionListTitle => {
                self.start_block();
                self.push_style(|style| {
                    style.bold = true;
                });
            }
            Tag::DefinitionListDefinition => {
                self.start_block();
                self.append_text(": ");
            }
            Tag::Table(alignments) => {
                self.start_block();
                self.table = Some(MarkdownTableState::new(
                    alignments,
                    self.table_display_width,
                    self.table_alternate_row_foreground,
                ));
            }
            Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self.push_style(|style| {
                style.italic = true;
            }),
            Tag::Strong => self.push_style(|style| {
                style.bold = true;
            }),
            Tag::Strikethrough => self.push_style(|style| {
                style.strikethrough = true;
            }),
            Tag::Superscript => self.push_style(|style| {
                style.bold = true;
            }),
            Tag::Subscript => self.push_style(|style| {
                style.dim = true;
            }),
            Tag::Link { dest_url, .. } => {
                self.link_stack.push(dest_url.to_string());
                let link_style = self.markdown_link_rendition();
                self.push_style(|style| *style = link_style);
            }
            Tag::Image { dest_url, .. } => {
                self.image_stack.push(dest_url.to_string());
                self.append_text("image: ");
                self.push_style(|style| {
                    style.italic = true;
                    style.underline = true;
                });
            }
            Tag::MetadataBlock(_) => self.start_block(),
        }
    }

    /// Handles the end of one markdown tag.
    fn handle_end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.finish_current_line(),
            TagEnd::Heading(_) => {
                self.pop_style();
                self.finish_current_line();
            }
            TagEnd::BlockQuote(_) => {
                self.finish_current_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.pop_style();
                self.finish_current_line();
            }
            TagEnd::HtmlBlock => self.finish_current_line(),
            TagEnd::List(_) => {
                self.finish_current_line();
                self.list_stack.pop();
            }
            TagEnd::Item => {
                self.finish_current_line();
                self.continuation_prefix = None;
            }
            TagEnd::FootnoteDefinition => self.finish_current_line(),
            TagEnd::DefinitionList => self.finish_current_line(),
            TagEnd::DefinitionListTitle => {
                self.pop_style();
                self.finish_current_line();
            }
            TagEnd::DefinitionListDefinition => self.finish_current_line(),
            TagEnd::Table => {}
            TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some(dest_url) = self.link_stack.pop()
                    && !dest_url.is_empty()
                    && !dest_url.starts_with("mez-agent:")
                {
                    self.append_dim_text(&format!(" ({dest_url})"));
                }
            }
            TagEnd::Image => {
                self.pop_style();
                if let Some(dest_url) = self.image_stack.pop()
                    && !dest_url.is_empty()
                {
                    self.append_dim_text(&format!(" ({dest_url})"));
                }
            }
            TagEnd::MetadataBlock(_) => self.finish_current_line(),
        }
    }

    /// Handles parser events while a table is being captured.
    fn handle_table_event(&mut self, event: Event<'_>) {
        let mut render_table = false;
        if let Some(table) = self.table.as_mut() {
            match event {
                Event::Start(Tag::Table(_)) => {}
                Event::End(TagEnd::Table) => render_table = true,
                Event::Start(Tag::TableHead) => table.in_head = true,
                Event::End(TagEnd::TableHead) => {
                    if !table.current_cell.is_empty() {
                        table.finish_cell();
                    }
                    if !table.current_row.is_empty() {
                        table.finish_row();
                    }
                    table.header_rows = table.rows.len();
                    table.in_head = false;
                }
                Event::Start(Tag::TableRow) => table.start_row(),
                Event::End(TagEnd::TableRow) => table.finish_row(),
                Event::Start(Tag::TableCell) => table.start_cell(),
                Event::End(TagEnd::TableCell) => table.finish_cell(),
                Event::Text(text)
                | Event::Code(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::Html(text)
                | Event::InlineHtml(text)
                | Event::FootnoteReference(text) => table.append_cell_text(text.as_ref()),
                Event::SoftBreak | Event::HardBreak => table.append_cell_text(" "),
                Event::Rule => table.append_cell_text("────────"),
                Event::TaskListMarker(checked) => {
                    table.append_cell_text(if checked { "[x] " } else { "[ ] " });
                }
                Event::Start(_) | Event::End(_) => {}
            }
        }
        if render_table && let Some(table) = self.table.take() {
            self.lines.extend(table.render_lines());
        }
    }

    /// Starts a new markdown block if the current line already has content.
    fn start_block(&mut self) {
        if !self.current.display.is_empty() {
            self.finish_current_line();
        }
    }

    /// Starts a list item with its ordered, unordered, or task marker prefix.
    fn start_list_item(&mut self) {
        self.start_block();
        let depth = self.list_stack.len().saturating_sub(1);
        let marker = if let Some(list) = self.list_stack.last_mut() {
            if list.ordered {
                let number = list.next_number;
                list.next_number = list.next_number.saturating_add(1);
                format!("{number}. ")
            } else {
                "• ".to_string()
            }
        } else {
            "• ".to_string()
        };
        let prefix = format!("{}{}{}", self.quote_prefix(), "  ".repeat(depth), marker);
        let continuation = format!(
            "{}{}{}",
            self.quote_prefix(),
            "  ".repeat(depth),
            " ".repeat(UnicodeWidthStr::width(marker.as_str()))
        );
        self.continuation_prefix = Some(continuation);
        self.append_prefix(&prefix);
    }

    /// Appends plain text using the currently active markdown style.
    fn append_text(&mut self, text: &str) {
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.finish_current_line();
            }
            if !part.is_empty() {
                self.ensure_line_prefix();
                self.current_prefix_only = false;
                self.append_styled_text(&sanitized_agent_terminal_line(part), self.active);
            }
        }
    }

    /// Appends inline code with a terminal-native code style.
    fn append_code(&mut self, code: &str) {
        self.ensure_line_prefix();
        self.current_prefix_only = false;
        let mut style = self.active;
        style.inverse = false;
        style.foreground = Some(if self.link_stack.is_empty() {
            self.inline_code_foreground
        } else {
            self.link_foreground
        });
        style.background = None;
        self.append_styled_text(&sanitized_agent_terminal_line(code), style);
    }

    /// Appends inline math with a lightweight math marker and italic style.
    fn append_inline_math(&mut self, math: &str) {
        self.ensure_line_prefix();
        self.current_prefix_only = false;
        let mut style = self.active;
        style.italic = true;
        self.append_styled_text(&format!("${}$", sanitized_agent_terminal_line(math)), style);
    }
    /// Returns the terminal rendition used for visible markdown link labels.
    fn markdown_link_rendition(&self) -> GraphicRendition {
        let mut style = self.active;
        style.foreground = Some(self.link_foreground);
        style.background = None;
        style.inverse = false;
        style.bold = true;
        style.underline = true;
        style
    }

    /// Appends display math as a block.
    fn append_display_math(&mut self, math: &str) {
        self.start_block();
        let mut style = self.active;
        style.italic = true;
        self.append_styled_text("$$", style);
        self.finish_current_line();
        for line in math.lines() {
            self.append_styled_text(&sanitized_agent_terminal_line(line), style);
            self.finish_current_line();
        }
        self.append_styled_text("$$", style);
        self.finish_current_line();
    }

    /// Handles inline HTML, preserving raw HTML except supported presentation tags.
    fn handle_inline_html(&mut self, html: &str) {
        match html.trim().to_ascii_lowercase().as_str() {
            "<u>" => self.push_style(|style| {
                style.underline = true;
            }),
            "</u>" => self.pop_style(),
            "<span class=\"mez-diff-addition\">" => {
                let foreground = self.diff_addition_foreground;
                self.push_style(|style| {
                    style.foreground = Some(foreground);
                    style.background = None;
                    style.inverse = false;
                    style.bold = true;
                });
            }
            "<span class=\"mez-diff-deletion\">" => {
                let foreground = self.diff_deletion_foreground;
                self.push_style(|style| {
                    style.foreground = Some(foreground);
                    style.background = None;
                    style.inverse = false;
                    style.bold = true;
                });
            }
            "</span>" => self.pop_style(),
            "<br>" | "<br/>" | "<br />" => self.finish_current_line(),
            _ => self.append_text(html),
        }
    }

    /// Appends lower-emphasis terminal text without changing the current style.
    fn append_dim_text(&mut self, text: &str) {
        self.ensure_line_prefix();
        self.current_prefix_only = false;
        let mut style = self.active;
        style.dim = true;
        self.append_styled_text(text, style);
    }

    /// Replaces the leading unordered marker in a GitHub task list item.
    fn replace_current_task_marker(&mut self, checked: bool) {
        let marker = if checked { "[x] " } else { "[ ] " };
        if let Some(position) = self.current.display.rfind("• ") {
            self.current.display.replace_range(position.., marker);
            return;
        }
        self.append_text(marker);
    }

    /// Ensures the current display line starts with quote/list continuation.
    fn ensure_line_prefix(&mut self) {
        if self.current.display.is_empty() {
            let prefix = self
                .continuation_prefix
                .clone()
                .unwrap_or_else(|| self.quote_prefix());
            self.append_prefix(&prefix);
        }
    }

    /// Appends an unstyled structural prefix.
    fn append_prefix(&mut self, prefix: &str) {
        self.append_styled_text(prefix, GraphicRendition::default());
        self.current_prefix_only = true;
    }

    /// Returns the visible prefix for the current blockquote depth.
    fn quote_prefix(&self) -> String {
        "> ".repeat(self.quote_depth)
    }

    /// Pushes a style transform on top of the active markdown style.
    fn push_style(&mut self, apply: impl FnOnce(&mut GraphicRendition)) {
        self.style_stack.push(self.active);
        apply(&mut self.active);
    }

    /// Restores the previous active markdown style.
    fn pop_style(&mut self) {
        if let Some(style) = self.style_stack.pop() {
            self.active = style;
        }
    }

    /// Appends styled terminal text and records display-cell spans.
    fn append_styled_text(&mut self, text: &str, rendition: GraphicRendition) {
        for grapheme in UnicodeSegmentation::graphemes(text, true) {
            let width = agent_terminal_grapheme_width(grapheme);
            let start = agent_terminal_text_width(self.current.display.as_str());
            self.current.display.push_str(grapheme);
            if width == 0 || rendition == GraphicRendition::default() {
                continue;
            }
            push_or_extend_style_span(
                &mut self.current.style_spans,
                TerminalStyleSpan {
                    start,
                    length: width,
                    rendition,
                },
            );
        }
    }

    /// Finishes the current line and resets line-local state.
    fn finish_current_line(&mut self) {
        if self.current.display.is_empty() {
            self.line_copy_prefix = None;
            self.current_prefix_only = false;
            return;
        }
        if let Some(prefix) = self.line_copy_prefix.take() {
            self.current.copy_text = Some(format!("{prefix}{}", self.current.display));
        }
        let line = std::mem::replace(
            &mut self.current,
            AgentRenderedLine {
                display: String::new(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
        );
        self.current_prefix_only = false;
        self.lines.push(line);
    }

    /// Removes trailing blank presentation lines after parsing completes.
    fn trim_trailing_blank_lines(&mut self) {
        while self
            .lines
            .last()
            .is_some_and(|line| line.display.trim().is_empty())
        {
            self.lines.pop();
        }
    }
}

impl AgentMarkdownRenderer {
    /// Builds an empty markdown renderer for one active UI theme.
    fn new(ui_theme: &UiTheme, table_display_width: Option<usize>) -> Self {
        Self {
            lines: Vec::new(),
            current: AgentRenderedLine {
                display: String::new(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
            table_display_width,
            active: GraphicRendition::default(),
            style_stack: Vec::new(),
            quote_depth: 0,
            list_stack: Vec::new(),
            continuation_prefix: None,
            link_stack: Vec::new(),
            image_stack: Vec::new(),
            table: None,
            line_copy_prefix: None,
            link_foreground: ui_theme.colors.agent_transcript_command.foreground,
            inline_code_foreground: markdown_inline_code_foreground(ui_theme),
            table_alternate_row_foreground: markdown_table_alternate_row_foreground(ui_theme),
            diff_addition_foreground: ui_theme.colors.agent_transcript_user.foreground,
            diff_deletion_foreground: ui_theme.colors.agent_transcript_error.foreground,
            current_prefix_only: false,
        }
    }
}

/// Tracks list numbering while rendering nested markdown lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkdownListState {
    /// Next ordered-list number to display.
    next_number: u64,
    /// Whether the list is ordered.
    ordered: bool,
}

/// Captures a CommonMark table before emitting aligned terminal rows.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MarkdownTableState {
    /// Column alignments reported by the parser.
    alignments: Vec<Alignment>,
    /// Completed rows.
    rows: Vec<Vec<String>>,
    /// Row currently being captured.
    current_row: Vec<String>,
    /// Cell currently being captured.
    current_cell: String,
    /// Number of rows that belong to the table header.
    header_rows: usize,
    /// Whether the parser is currently inside the table head.
    in_head: bool,
    /// Optional maximum terminal width available for rendered table rows.
    display_width: Option<usize>,
    /// Foreground used for alternating body rows.
    alternate_row_foreground: TerminalColor,
}

impl MarkdownTableState {
    /// Builds a table capture state for parser-provided alignments.
    fn new(
        alignments: Vec<Alignment>,
        display_width: Option<usize>,
        alternate_row_foreground: TerminalColor,
    ) -> Self {
        Self {
            alignments,
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            header_rows: 0,
            in_head: false,
            display_width,
            alternate_row_foreground,
        }
    }

    /// Starts a new table row.
    fn start_row(&mut self) {
        self.current_row.clear();
    }

    /// Finishes the current table row.
    fn finish_row(&mut self) {
        if !self.current_cell.is_empty() {
            self.finish_cell();
        }
        self.rows.push(std::mem::take(&mut self.current_row));
    }

    /// Starts a new table cell.
    fn start_cell(&mut self) {
        self.current_cell.clear();
    }

    /// Finishes the current table cell.
    fn finish_cell(&mut self) {
        self.current_row.push(self.current_cell.trim().to_string());
        self.current_cell.clear();
    }

    /// Appends text into the current table cell.
    fn append_cell_text(&mut self, text: &str) {
        if !self.current_cell.is_empty() && text.starts_with(char::is_whitespace) {
            self.current_cell.push(' ');
        }
        self.current_cell
            .push_str(&sanitized_agent_terminal_line(text).replace('\n', " "));
    }

    /// Renders the captured table as aligned box-drawing terminal rows.
    fn render_lines(self) -> Vec<AgentRenderedLine> {
        let column_count = self.column_count();
        if column_count == 0 {
            return Vec::new();
        }
        let widths = self.column_widths(column_count);
        let mut lines = Vec::new();
        for (row_index, row) in self.rows.iter().enumerate() {
            let wrapped_cells = self.wrap_row_cells(row, &widths);
            let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for physical_row in 0..row_height {
                let rendered = self.render_wrapped_row(&wrapped_cells, &widths, physical_row);
                let mut line = AgentRenderedLine {
                    display: rendered.clone(),
                    style_spans: Vec::new(),
                    copy_text: Some(if physical_row == 0 {
                        rendered
                    } else {
                        AGENT_COPY_SKIP_LINE.to_string()
                    }),
                    kind: if physical_row == 0 {
                        AgentRenderedLineKind::MarkdownTableRow
                    } else {
                        AgentRenderedLineKind::MarkdownTableContinuation
                    },
                };
                self.apply_row_style(&mut line, row_index);
                lines.push(line);
            }
            if row_index + 1 == self.header_rows {
                lines.push(AgentRenderedLine {
                    display: self.render_separator(&widths),
                    style_spans: Vec::new(),
                    copy_text: None,
                    kind: AgentRenderedLineKind::MarkdownTableSeparator,
                });
            }
        }
        lines
    }

    /// Returns the number of table columns.
    fn column_count(&self) -> usize {
        self.alignments
            .len()
            .max(self.rows.iter().map(Vec::len).max().unwrap_or_default())
    }

    /// Computes display widths for each column.
    fn column_widths(&self, column_count: usize) -> Vec<usize> {
        let natural_widths = (0..column_count)
            .map(|column| {
                self.rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| agent_terminal_text_width(cell.as_str()))
                    .max()
                    .unwrap_or(0)
                    .max(3)
            })
            .collect::<Vec<_>>();
        let Some(display_width) = self.display_width else {
            return natural_widths;
        };
        if Self::table_total_width(&natural_widths) <= display_width {
            return natural_widths;
        }
        Self::bounded_column_widths(&natural_widths, display_width)
    }

    /// Returns total display width for a rendered table with these content widths.
    fn table_total_width(widths: &[usize]) -> usize {
        widths
            .iter()
            .sum::<usize>()
            .saturating_add(widths.len().saturating_mul(3))
            .saturating_add(1)
    }

    /// Allocates bounded content widths after box and padding overhead.
    fn bounded_column_widths(natural_widths: &[usize], display_width: usize) -> Vec<usize> {
        let column_count = natural_widths.len();
        if column_count == 0 {
            return Vec::new();
        }
        let structural_width = column_count.saturating_mul(3).saturating_add(1);
        let available = display_width
            .saturating_sub(structural_width)
            .max(column_count);
        let minimum_width: usize = if available >= column_count.saturating_mul(3) {
            3
        } else {
            1
        };
        let mut widths = vec![minimum_width; column_count];
        let mut remaining = available.saturating_sub(minimum_width.saturating_mul(column_count));
        while remaining > 0 {
            let mut advanced = false;
            for (width, natural_width) in widths.iter_mut().zip(natural_widths.iter()) {
                if remaining == 0 {
                    break;
                }
                if *width < *natural_width {
                    *width = width.saturating_add(1);
                    remaining = remaining.saturating_sub(1);
                    advanced = true;
                }
            }
            if !advanced {
                break;
            }
        }
        widths
    }

    /// Wraps every cell in one markdown source row to its allocated content width.
    fn wrap_row_cells(&self, row: &[String], widths: &[usize]) -> Vec<Vec<String>> {
        widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let cell = row.get(column).map(String::as_str).unwrap_or_default();
                Self::wrap_cell(cell, *width)
            })
            .collect()
    }

    /// Wraps one cell into physical table-row fragments.
    fn wrap_cell(cell: &str, width: usize) -> Vec<String> {
        let width = width.max(1);
        let mut remaining = cell.trim();
        if remaining.is_empty() {
            return vec![String::new()];
        }
        let mut lines = Vec::new();
        while !remaining.is_empty() {
            let (segment, consumed) = Self::take_cell_segment(remaining, width);
            lines.push(segment);
            remaining = remaining[consumed..].trim_start();
        }
        lines
    }

    /// Takes one table-cell segment, hard-splitting only when needed for layout.
    fn take_cell_segment(text: &str, width: usize) -> (String, usize) {
        if agent_terminal_text_width(text) <= width {
            return (text.to_string(), text.len());
        }
        if let Some((_, grapheme)) = UnicodeSegmentation::grapheme_indices(text, true).next()
            && agent_terminal_grapheme_width(grapheme) > width
        {
            return ("…".to_string(), grapheme.len());
        }
        let mut used_width = 0usize;
        let mut boundary_consumed = 0usize;
        let mut last_space_break: Option<(usize, usize)> = None;
        for (index, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
            let grapheme_width = agent_terminal_grapheme_width(grapheme);
            if used_width > 0 && used_width.saturating_add(grapheme_width) > width {
                break;
            }
            let next_consumed = index.saturating_add(grapheme.len());
            if grapheme.chars().all(char::is_whitespace) && used_width > 0 {
                last_space_break = Some((index, next_consumed));
            }
            boundary_consumed = next_consumed;
            used_width = used_width.saturating_add(grapheme_width);
            if used_width >= width {
                break;
            }
        }
        if let Some((space_start, consumed_through_space)) = last_space_break {
            let segment = text[..space_start].trim_end().to_string();
            if !segment.is_empty() {
                return (segment, consumed_through_space);
            }
        }
        (text[..boundary_consumed].to_string(), boundary_consumed)
    }

    /// Renders one physical table row from already wrapped cells.
    fn render_wrapped_row(
        &self,
        cells: &[Vec<String>],
        widths: &[usize],
        row_index: usize,
    ) -> String {
        let row = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let cell = cells
                    .get(column)
                    .and_then(|lines| lines.get(row_index))
                    .map(String::as_str)
                    .unwrap_or_default();
                self.render_cell(cell, *width, self.alignment(column))
            })
            .collect::<Vec<_>>();
        format!("│{}│", row.join("│"))
    }

    /// Applies header or alternating-row table styling to one physical row.
    fn apply_row_style(&self, line: &mut AgentRenderedLine, row_index: usize) {
        let length = agent_terminal_text_width(line.display.as_str());
        if length == 0 {
            return;
        }
        let rendition = if row_index < self.header_rows {
            GraphicRendition {
                bold: true,
                ..GraphicRendition::default()
            }
        } else if row_index.saturating_sub(self.header_rows).is_multiple_of(2) {
            GraphicRendition {
                foreground: Some(self.alternate_row_foreground),
                background: None,
                ..GraphicRendition::default()
            }
        } else {
            return;
        };
        line.style_spans.push(TerminalStyleSpan {
            start: 0,
            length,
            rendition,
        });
    }

    /// Renders one box-drawing table separator row.
    fn render_separator(&self, widths: &[usize]) -> String {
        let cells = widths
            .iter()
            .map(|width| "─".repeat(width.saturating_add(2)))
            .collect::<Vec<_>>();
        format!("├{}┤", cells.join("┼"))
    }

    /// Renders one padded table cell.
    fn render_cell(&self, cell: &str, width: usize, alignment: Alignment) -> String {
        let cell_width = agent_terminal_text_width(cell);
        let padding = width.saturating_sub(cell_width);
        let (left, right) = match alignment {
            Alignment::Right => (padding, 0),
            Alignment::Center => (padding / 2, padding.saturating_sub(padding / 2)),
            Alignment::None | Alignment::Left => (0, padding),
        };
        format!(" {}{}{} ", " ".repeat(left), cell, " ".repeat(right))
    }

    /// Returns the alignment for a column.
    fn alignment(&self, column: usize) -> Alignment {
        self.alignments
            .get(column)
            .copied()
            .unwrap_or(Alignment::None)
    }
}

/// Pushes a style span, coalescing adjacent runs with the same rendition.
pub(super) fn push_or_extend_style_span(
    spans: &mut Vec<TerminalStyleSpan>,
    span: TerminalStyleSpan,
) {
    if span.length == 0 {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.start.saturating_add(last.length) == span.start
        && last.rendition == span.rendition
    {
        last.length = last.length.saturating_add(span.length);
        return;
    }
    spans.push(span);
}

/// Runs the command preview terminal lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn command_preview_terminal_lines(
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
pub(super) fn command_preview_terminal_rendered_lines(
    command: &str,
    columns: usize,
    max_lines: usize,
    classification: ShellClassification,
    ui_theme: &UiTheme,
) -> Vec<AgentRenderedLine> {
    let syntax_theme = agent_command_syntax_theme(ui_theme);
    let mut highlighter = agent_shell_command_highlighter(classification, &syntax_theme);
    let command_rendition =
        agent_terminal_label_rendition(AgentTerminalPresentationStyle::Command, ui_theme);
    command_preview_terminal_lines(command, columns, max_lines)
        .into_iter()
        .map(|display| {
            let mut rendered = AgentRenderedLine {
                display,
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
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
                append_agent_syntax_spans(&mut rendered, text_start, &syntax_text, highlighter);
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
pub(super) fn wrap_agent_terminal_text(text: &str, columns: usize) -> Vec<String> {
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
pub(super) fn take_agent_terminal_word_wrapped_segment(
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
pub(super) fn fit_agent_terminal_text_width(text: &str, columns: usize) -> String {
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
pub(super) fn bounded_agent_terminal_presentation_columns(columns: usize) -> usize {
    columns.clamp(1, AGENT_TERMINAL_PRESENTATION_MAX_COLUMNS)
}

/// Returns the display width of agent transcript text.
///
/// # Parameters
/// - `text`: The agent transcript text to measure.
pub(super) fn agent_terminal_text_width(text: &str) -> usize {
    UnicodeSegmentation::graphemes(text, true)
        .map(agent_terminal_grapheme_width)
        .sum()
}

/// Returns the display width of one agent transcript grapheme cluster.
///
/// # Parameters
/// - `grapheme`: The grapheme cluster to measure.
pub(super) fn agent_terminal_grapheme_width(grapheme: &str) -> usize {
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
pub(super) fn agent_display_lines_are_error(lines: &[String]) -> bool {
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
pub(super) fn agent_display_lines_are_low_level_status(lines: &[String]) -> bool {
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
pub(super) fn agent_display_line_is_low_level_status(line: &str) -> bool {
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
pub(super) fn agent_prompt_error_display_lines(error: &MezError) -> Vec<String> {
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
pub(super) fn overlay_styled_lines(
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
pub(super) fn append_styled_agent_terminal_line(
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
pub(super) fn append_styled_agent_terminal_rendered_line(
    bytes: &mut String,
    style: AgentTerminalPresentationStyle,
    line: &AgentRenderedLine,
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
pub(super) fn agent_terminal_label_rendition(
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
pub(super) fn rendered_line_rendition_at(
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
fn merge_agent_rendered_line_renditions(
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
fn rendered_line_rendition_at_merges_diff_base_with_syntax_foreground() {
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

/// Chooses the presentation style for one generated diff preview line.
///
/// Semantic action diff lines use standard unified diff markers, while older
/// path-only previews use fixed-width old/new line number columns followed by a
/// marker. Recognizing both forms lets the pane transcript color additions and
/// deletions without requiring raw ANSI from the hidden shell transaction to
/// reach the user view.
pub(super) fn agent_diff_line_style(line: &str) -> AgentTerminalPresentationStyle {
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
pub(super) fn agent_action_result_uses_diff_preview(action: &AgentAction) -> bool {
    matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// One parsed line from a unified diff hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentDiffDisplayLine {
    old_line: Option<usize>,
    new_line: Option<usize>,
    pub(super) marker: char,
    text: String,
}

/// One parsed file-level diff display section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentDiffDisplaySection {
    old_label: String,
    new_label: String,
    pub(super) lines: Vec<AgentDiffDisplayLine>,
    hunk_headers: Vec<(usize, String)>,
}

/// Builds readable styled diff display lines from raw shell diff output.
///
/// Semantic mutation helpers execute inside a PTY, so their captured output may
/// include prompt redraws, wrapper variables, and echoed command fragments
/// around the actual diff. The pane should present the semantic change, not the
/// mechanics used to collect it.
#[cfg(test)]
pub(super) fn readable_agent_diff_display_lines(
    text: &str,
    ui_theme: &UiTheme,
) -> Vec<AgentRenderedLine> {
    readable_agent_diff_display_lines_for_width(text, ui_theme, usize::MAX)
}

/// Builds readable styled diff display lines and wraps them to a display width.
///
/// # Parameters
/// - `text`: Raw hidden-shell diff output.
/// - `ui_theme`: The active UI theme.
/// - `display_width`: Cells available after the agent transcript gutter.
pub(super) fn readable_agent_diff_display_lines_for_width(
    text: &str,
    ui_theme: &UiTheme,
    display_width: usize,
) -> Vec<AgentRenderedLine> {
    let source_lines = cleaned_agent_diff_source_lines(text);
    let sections = parse_agent_unified_diff_sections(&source_lines);
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
        .flat_map(|line| wrap_agent_rendered_line_to_width(line, display_width.max(1)))
        .collect();
    bound_agent_diff_display_lines(wrapped)
}

/// Removes Mezzanine wrapper and prompt echo lines around a diff.
pub(super) fn cleaned_agent_diff_source_lines(text: &str) -> Vec<String> {
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
pub(super) fn strip_agent_diff_prompt_prefix(line: &str) -> &str {
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
pub(super) fn agent_diff_line_is_prompt_glyph(trimmed: &str) -> bool {
    trimmed
        .chars()
        .all(|ch| matches!(ch, '' | '∙' | ' ' | '\t'))
}

/// Returns true for shell wrapper echo that should never appear in diff output.
pub(super) fn agent_diff_line_is_wrapper_traffic(trimmed: &str) -> bool {
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

/// Parses unified diff sections from cleaned shell output.
pub(super) fn parse_agent_unified_diff_sections(lines: &[String]) -> Vec<AgentDiffDisplaySection> {
    let mut sections = Vec::new();
    let mut index = 0usize;
    while index + 1 < lines.len() {
        if !lines[index].starts_with("--- ") || !lines[index + 1].starts_with("+++ ") {
            index += 1;
            continue;
        }
        let old_label = clean_agent_diff_label(&lines[index][4..]);
        let new_label = clean_agent_diff_label(&lines[index + 1][4..]);
        index += 2;
        let mut section = AgentDiffDisplaySection {
            old_label,
            new_label,
            lines: Vec::new(),
            hunk_headers: Vec::new(),
        };
        while index < lines.len() {
            if index + 1 < lines.len()
                && lines[index].starts_with("--- ")
                && lines[index + 1].starts_with("+++ ")
            {
                break;
            }
            let Some((mut old_line, mut new_line)) = parse_agent_diff_hunk_header(&lines[index])
            else {
                index += 1;
                continue;
            };
            section
                .hunk_headers
                .push((section.lines.len(), lines[index].to_string()));
            index += 1;
            while index < lines.len() {
                let line = &lines[index];
                if line.starts_with("@@ ")
                    || (index + 1 < lines.len()
                        && line.starts_with("--- ")
                        && lines[index + 1].starts_with("+++ "))
                {
                    break;
                }
                if line.starts_with("\\ ") {
                    index += 1;
                    continue;
                }
                if let Some(text) = line.strip_prefix('+') {
                    section.lines.push(AgentDiffDisplayLine {
                        old_line: None,
                        new_line: Some(new_line),
                        marker: '+',
                        text: text.to_string(),
                    });
                    new_line = new_line.saturating_add(1);
                } else if let Some(text) = line.strip_prefix('-') {
                    section.lines.push(AgentDiffDisplayLine {
                        old_line: Some(old_line),
                        new_line: None,
                        marker: '-',
                        text: text.to_string(),
                    });
                    old_line = old_line.saturating_add(1);
                } else if let Some(text) = line.strip_prefix(' ') {
                    section.lines.push(AgentDiffDisplayLine {
                        old_line: Some(old_line),
                        new_line: Some(new_line),
                        marker: ' ',
                        text: text.to_string(),
                    });
                    old_line = old_line.saturating_add(1);
                    new_line = new_line.saturating_add(1);
                }
                index += 1;
            }
        }
        if !section.lines.is_empty() {
            sections.push(section);
        }
    }
    sections
}

/// Parses the old/new start line numbers from a unified diff hunk header.
pub(super) fn parse_agent_diff_hunk_header(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((
        parse_agent_diff_range_start(old)?,
        parse_agent_diff_range_start(new)?,
    ))
}

/// Parses the start line from a unified diff range.
pub(super) fn parse_agent_diff_range_start(value: &str) -> Option<usize> {
    value
        .split(',')
        .next()?
        .parse::<usize>()
        .ok()
        .map(|line| line.max(1))
}

/// Cleans a unified diff file label for display.
pub(super) fn clean_agent_diff_label(value: &str) -> String {
    let label = value.split('\t').next().unwrap_or(value).trim();
    label
        .strip_prefix("a/")
        .or_else(|| label.strip_prefix("b/"))
        .unwrap_or(label)
        .to_string()
}

/// Renders parsed unified diff sections into visible unified-diff previews.
pub(super) fn render_agent_unified_diff_sections(
    sections: &[AgentDiffDisplaySection],
    ui_theme: &UiTheme,
) -> Vec<AgentRenderedLine> {
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
        let mut highlighter =
            agent_diff_highlighter_for_path(agent_diff_section_path(section), &syntax_theme);
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

/// Returns the display path for a parsed file diff section.
pub(super) fn agent_diff_section_path(section: &AgentDiffDisplaySection) -> &str {
    if section.new_label == "/dev/null" {
        &section.old_label
    } else {
        &section.new_label
    }
}

/// Formats one parsed hunk line with a stable line-number gutter.
pub(super) fn format_agent_diff_display_line(line: &AgentDiffDisplayLine) -> String {
    let old_line = line
        .old_line
        .map(|line| line.to_string())
        .unwrap_or_default();
    let new_line = line
        .new_line
        .map(|line| line.to_string())
        .unwrap_or_default();
    format!("{old_line:>6} {new_line:>6} {}{}", line.marker, line.text)
}

/// Renders one parsed hunk line with a diff gutter and file-aware code spans.
pub(super) fn render_agent_diff_display_line(
    line: &AgentDiffDisplayLine,
    highlighter: Option<&mut HighlightLines<'_>>,
    ui_theme: &UiTheme,
) -> AgentRenderedLine {
    let display = format_agent_diff_display_line(line);
    let marker_style = agent_diff_display_line_style(line.marker);
    let marker_rendition = agent_terminal_label_rendition(marker_style, ui_theme);
    let mut rendered = AgentRenderedLine {
        display,
        style_spans: Vec::new(),
        copy_text: None,
        kind: AgentRenderedLineKind::Normal,
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
        append_agent_syntax_spans(&mut rendered, 15, &line.text, highlighter);
    }
    rendered
}

/// Returns the presentation style for one parsed diff hunk line.
pub(super) fn agent_diff_display_line_style(marker: char) -> AgentTerminalPresentationStyle {
    match marker {
        '+' => AgentTerminalPresentationStyle::DiffAddition,
        '-' => AgentTerminalPresentationStyle::DiffDeletion,
        _ => AgentTerminalPresentationStyle::DiffContext,
    }
}

/// Creates a syntax highlighter for the displayed file path when available.
pub(super) fn agent_diff_highlighter_for_path<'a>(
    path: &str,
    theme: &'a Theme,
) -> Option<HighlightLines<'a>> {
    let syntax = agent_diff_syntax_for_path(path)?;
    Some(HighlightLines::new(syntax, theme))
}

/// Resolves a syntax definition from a diff display path.
pub(super) fn agent_diff_syntax_for_path(path: &str) -> Option<&'static SyntaxReference> {
    if path == "/dev/null" {
        return None;
    }
    let syntax_set = &*AGENT_DIFF_SYNTAX_SET;
    syntax_set
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Creates a syntax highlighter for shell command previews.
pub(super) fn agent_shell_command_highlighter<'a>(
    classification: ShellClassification,
    theme: &'a Theme,
) -> Option<HighlightLines<'a>> {
    let syntax = agent_shell_command_syntax(classification)?;
    Some(HighlightLines::new(syntax, theme))
}

/// Resolves the syntax definition that best matches the pane shell.
pub(super) fn agent_shell_command_syntax(
    classification: ShellClassification,
) -> Option<&'static SyntaxReference> {
    let syntax_set = &*AGENT_DIFF_SYNTAX_SET;
    let extensions = match classification {
        ShellClassification::Fish => &["fish"][..],
        ShellClassification::Bash => &["bash", "sh"][..],
        ShellClassification::Zsh => &["zsh", "sh"][..],
        ShellClassification::PosixSh | ShellClassification::UnknownUnix => &["sh"][..],
    };
    extensions
        .iter()
        .find_map(|extension| syntax_set.find_syntax_by_extension(extension))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Builds the syntax theme used for shell command preview highlighting.
pub(super) fn agent_command_syntax_theme(ui_theme: &UiTheme) -> Theme {
    Theme {
        name: Some(format!("mezzanine-{}", ui_theme.name)),
        author: Some("Mezzanine".to_string()),
        settings: ThemeSettings {
            foreground: Some(syntect_color_from_terminal_color(
                ui_theme.colors.syntax_plain.foreground,
            )),
            background: Some(syntect_color_from_terminal_color(
                ui_theme.colors.syntax_plain.background,
            )),
            accent: Some(syntect_color_from_terminal_color(
                ui_theme.colors.syntax_keyword.foreground,
            )),
            ..ThemeSettings::default()
        },
        scopes: agent_command_syntax_theme_items(ui_theme),
    }
}

/// Builds TextMate scope rules from Mezzanine's active theme colors.
pub(super) fn agent_command_syntax_theme_items(ui_theme: &UiTheme) -> Vec<ThemeItem> {
    [
        ("source", ui_theme.colors.syntax_plain.foreground, None),
        (
            "comment",
            ui_theme.colors.syntax_comment.foreground,
            Some(FontStyle::ITALIC),
        ),
        ("string", ui_theme.colors.syntax_string.foreground, None),
        (
            "constant.numeric, constant.character, constant.language, constant.other",
            ui_theme.colors.syntax_number.foreground,
            None,
        ),
        (
            "keyword, storage, storage.modifier",
            ui_theme.colors.syntax_keyword.foreground,
            Some(FontStyle::BOLD),
        ),
        (
            "storage.type, support.type, entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.interface, meta.type",
            ui_theme.colors.syntax_type.foreground,
            None,
        ),
        (
            "entity.name.function, support.function, meta.function-call, variable.function",
            ui_theme.colors.syntax_function.foreground,
            None,
        ),
        (
            "keyword.operator, punctuation",
            ui_theme.colors.syntax_operator.foreground,
            None,
        ),
    ]
    .into_iter()
    .filter_map(|(selector, foreground, font_style)| {
        agent_diff_syntax_theme_item(selector, foreground, font_style)
    })
    .collect()
}

/// Builds the syntax theme used for terminal diff body highlighting.
pub(super) fn agent_diff_syntax_theme(ui_theme: &UiTheme) -> Theme {
    Theme {
        name: Some(format!("mezzanine-{}", ui_theme.name)),
        author: Some("Mezzanine".to_string()),
        settings: ThemeSettings {
            foreground: Some(syntect_color_from_terminal_color(
                ui_theme.colors.syntax_plain.foreground,
            )),
            background: None,
            accent: Some(syntect_color_from_terminal_color(
                ui_theme.colors.syntax_keyword.foreground,
            )),
            ..ThemeSettings::default()
        },
        scopes: agent_diff_syntax_theme_items(ui_theme),
    }
}

/// Builds TextMate scope rules from Mezzanine's active theme colors.
pub(super) fn agent_diff_syntax_theme_items(ui_theme: &UiTheme) -> Vec<ThemeItem> {
    [
        ("source", ui_theme.colors.syntax_plain.foreground, None),
        (
            "comment",
            ui_theme.colors.syntax_comment.foreground,
            Some(FontStyle::ITALIC),
        ),
        ("string", ui_theme.colors.syntax_string.foreground, None),
        (
            "constant.numeric, constant.character, constant.language, constant.other",
            ui_theme.colors.syntax_number.foreground,
            None,
        ),
        (
            "keyword, storage, storage.modifier",
            ui_theme.colors.syntax_keyword.foreground,
            Some(FontStyle::BOLD),
        ),
        (
            "storage.type, support.type, entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.interface, meta.type",
            ui_theme.colors.syntax_type.foreground,
            None,
        ),
        (
            "entity.name.function, support.function, meta.function-call, variable.function",
            ui_theme.colors.syntax_function.foreground,
            None,
        ),
        (
            "keyword.operator, punctuation",
            ui_theme.colors.syntax_operator.foreground,
            None,
        ),
    ]
    .into_iter()
    .filter_map(|(selector, foreground, font_style)| {
        agent_diff_syntax_theme_item(selector, foreground, font_style)
    })
    .collect()
}

/// Builds one safe syntect theme item from a constant scope selector.
pub(super) fn agent_diff_syntax_theme_item(
    selector: &str,
    foreground: TerminalColor,
    font_style: Option<FontStyle>,
) -> Option<ThemeItem> {
    ScopeSelectors::from_str(selector)
        .ok()
        .map(|scope| ThemeItem {
            scope,
            style: StyleModifier {
                foreground: Some(syntect_color_from_terminal_color(foreground)),
                background: None,
                font_style,
            },
        })
}

/// Converts a Mezzanine terminal color into a syntect RGB color.
pub(super) fn syntect_color_from_terminal_color(color: TerminalColor) -> SyntectColor {
    match color {
        TerminalColor::Rgb(red, green, blue) => SyntectColor {
            r: red,
            g: green,
            b: blue,
            a: 0xff,
        },
        TerminalColor::Indexed(index) => syntect_color_from_indexed_terminal_color(index),
    }
}

/// Converts an indexed terminal color into a conservative RGB approximation.
pub(super) fn syntect_color_from_indexed_terminal_color(index: u8) -> SyntectColor {
    const ANSI_16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0x80, 0x00, 0x00),
        (0x00, 0x80, 0x00),
        (0x80, 0x80, 0x00),
        (0x00, 0x00, 0x80),
        (0x80, 0x00, 0x80),
        (0x00, 0x80, 0x80),
        (0xc0, 0xc0, 0xc0),
        (0x80, 0x80, 0x80),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x00, 0x00, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    let (red, green, blue) = ANSI_16
        .get(usize::from(index))
        .copied()
        .unwrap_or((0xe4, 0xef, 0xe8));
    SyntectColor {
        r: red,
        g: green,
        b: blue,
        a: 0xff,
    }
}

/// Appends syntax color spans to a rendered line after its presentation gutter.
pub(super) fn append_agent_syntax_spans(
    rendered: &mut AgentRenderedLine,
    text_start: usize,
    text: &str,
    highlighter: &mut HighlightLines<'_>,
) {
    let Ok(highlighted) = highlighter.highlight_line(text, &AGENT_DIFF_SYNTAX_SET) else {
        return;
    };
    let mut column = text_start;
    for (style, segment) in highlighted {
        let rendition = agent_diff_syntect_rendition(style);
        let width = agent_terminal_text_width(segment);
        push_or_extend_style_span(
            &mut rendered.style_spans,
            TerminalStyleSpan {
                start: column,
                length: width,
                rendition,
            },
        );
        column = column.saturating_add(width);
    }
}

/// Converts syntect token style into Mezzanine's terminal rendition model.
pub(super) fn agent_diff_syntect_rendition(style: SyntectStyle) -> GraphicRendition {
    GraphicRendition {
        bold: style.font_style.contains(FontStyle::BOLD),
        italic: style.font_style.contains(FontStyle::ITALIC),
        underline: style.font_style.contains(FontStyle::UNDERLINE),
        foreground: Some(TerminalColor::Rgb(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        )),
        ..GraphicRendition::default()
    }
}

/// Parses and renders simple path-only delta output.
pub(super) fn parse_agent_path_delta_display_lines(
    lines: &[String],
    ui_theme: &UiTheme,
) -> Vec<AgentRenderedLine> {
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
pub(super) fn agent_path_delta_verb(title: &str) -> &'static str {
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
pub(super) fn agent_path_delta_header_path<'a>(
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
pub(super) fn rendered_agent_diff_plain_line(
    style: AgentTerminalPresentationStyle,
    line: &str,
    ui_theme: &UiTheme,
) -> AgentRenderedLine {
    let display = sanitized_agent_terminal_line(line);
    let length = agent_terminal_text_width(display.as_str());
    let mut rendered = AgentRenderedLine {
        display,
        style_spans: Vec::new(),
        copy_text: None,
        kind: AgentRenderedLineKind::Normal,
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
pub(super) fn bound_agent_diff_display_lines(
    lines: Vec<AgentRenderedLine>,
) -> Vec<AgentRenderedLine> {
    let mut bounded = Vec::new();
    let mut used_bytes = 0usize;
    for (index, mut line) in lines.into_iter().enumerate() {
        if index >= AGENT_ACTION_RESULT_DISPLAY_MAX_LINES {
            bounded.push(AgentRenderedLine {
                display: "[mez: diff truncated for pane display]".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            });
            break;
        }
        let remaining = AGENT_ACTION_RESULT_DISPLAY_MAX_BYTES.saturating_sub(used_bytes);
        if remaining == 0 {
            bounded.push(AgentRenderedLine {
                display: "[mez: diff truncated for pane display]".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
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
            bounded.push(AgentRenderedLine {
                display: "[mez: diff truncated for pane display]".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
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

/// Builds the compact header shown for action execution/result output.
pub(super) fn agent_action_execution_display_header(action: &AgentAction) -> Option<String> {
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
pub(super) fn agent_action_model_thinking_lines(action: &AgentAction) -> Vec<String> {
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
pub(super) fn agent_thinking_display_text(text: &str) -> String {
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
pub(super) fn agent_thinking_display_lines_for_width(text: &str, columns: usize) -> Vec<String> {
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

/// Builds the compact header shown above elevated action result output.
pub(super) fn agent_action_result_display_header(action: &AgentAction) -> Option<String> {
    agent_action_execution_display_header(action)
}

/// Builds the normal-mode action execution line with balanced visual weight.
///
/// The display text deliberately keeps the established `agent: action: target`
/// grammar while applying color only to the semantic pieces that need emphasis:
/// the prefix is quiet status text, the action phrase is command-accented, and
/// arguments fall back to the terminal foreground for readability.
pub(super) fn agent_action_execution_rendered_line(
    header: &str,
    ui_theme: &UiTheme,
) -> AgentRenderedLine {
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

    AgentRenderedLine {
        display,
        style_spans,
        copy_text: None,
        kind: AgentRenderedLineKind::Normal,
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
pub(super) fn push_agent_action_execution_style_span(
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
pub(super) fn push_agent_action_execution_secondary_spans(
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
pub(super) fn agent_action_display_preview(value: &str) -> String {
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
pub(super) fn agent_action_json_argument_preview(value: &str) -> String {
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
pub(super) fn agent_action_path_list_preview(paths: &[String]) -> String {
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
pub(super) fn bounded_agent_action_result_display_lines(text: &str) -> Vec<String> {
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
pub(super) fn truncate_to_utf8_boundary(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

impl RuntimeSessionService {
    /// Runs the append agent user prompt to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_user_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = wrapped_prefixed_agent_terminal_lines("user> ", prompt, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::UserPrompt,
            rendered_lines.as_slice(),
            &[],
        )
    }

    /// Appends the parent-supplied prompt at the top of a spawned subagent pane.
    ///
    /// Subagent pane logs should expose the exact parent instruction that
    /// started the child turn so follow-up inspection does not require looking
    /// back through the parent pane.
    pub(in crate::runtime) fn append_agent_parent_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let lines = prefixed_agent_terminal_lines("parent> ", prompt);
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::UserPrompt,
        )
    }

    /// Runs the append agent assistant text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_assistant_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        self.append_agent_assistant_content_to_terminal_buffer(
            pane_id,
            text,
            AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
    }

    /// Appends assistant output using its declared presentation media type.
    pub(in crate::runtime) fn append_agent_assistant_content_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
        content_type: &str,
    ) -> Result<()> {
        if agent_output_content_type_is_markdown(content_type)
            && !agent_say_text_is_displayed_patch_block(text)
        {
            return self.append_agent_assistant_markdown_to_terminal_buffer(pane_id, text);
        }
        if agent_output_content_type_is_diff(content_type) {
            return self.append_agent_diff_text_to_terminal_buffer(pane_id, text);
        }
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = wrapped_prefixed_agent_terminal_lines("mez> ", text, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Assistant,
            rendered_lines.as_slice(),
            &[],
        )
    }

    /// Returns the display cells available after the agent transcript gutter.
    fn agent_terminal_markdown_frame_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        Ok(bounded_agent_terminal_presentation_columns(columns)
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1))
    }

    /// Returns display cells available after the agent transcript gutter.
    fn agent_terminal_markdown_terminal_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        Ok(columns
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1))
    }

    /// Returns display cells available for editable pane-local prompt text.
    ///
    /// This width mirrors the terminal renderer, which draws the editable text
    /// after both the agent transcript gutter and the `agent>` prompt marker.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current presentation width bounds the prompt.
    pub(super) fn agent_prompt_editable_body_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        let prompt_prefix_width = UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX)
            .saturating_add(UnicodeWidthStr::width(AGENT_PROMPT_TEXT_PREFIX));
        Ok(columns.saturating_sub(prompt_prefix_width).max(1))
    }

    /// Returns the current pane presentation width in terminal display cells.
    fn agent_terminal_presentation_columns(&self, pane_id: &str) -> Result<usize> {
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if let Some(columns) = self.agent_terminal_render_region_columns(pane_id) {
            return Ok(columns);
        }
        let columns = self
            .pane_screens
            .get(pane_id)
            .map(|screen| screen.size().columns)
            .unwrap_or(descriptor.size.columns);
        Ok(usize::from(columns))
    }

    /// Returns the pane-local render width used by the terminal compositor.
    fn agent_terminal_render_region_columns(&self, pane_id: &str) -> Option<usize> {
        let window = self.session.active_window()?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let body_size = rendered_window_body_size(window.size, self.window_frames_enabled).ok()?;
        let geometries = if window.zoomed_pane_id() == Some(&pane.id) {
            vec![PaneGeometry {
                index: pane.index,
                column: 0,
                row: 0,
                columns: body_size.columns,
                rows: body_size.rows,
            }]
        } else {
            window.pane_geometries_for_size(body_size)
        };
        let geometry = geometries
            .iter()
            .find(|geometry| geometry.index == pane.index)?;
        pane_content_size_for_geometry(
            geometry,
            &geometries,
            self.pane_frames_enabled,
            self.pane_frame_position,
        )
        .ok()
        .map(|size| usize::from(size.columns))
    }

    /// Returns the pane width to persist with one agent presentation entry.
    fn agent_presentation_terminal_width(&self, pane_id: &str) -> Option<u16> {
        self.pane_screens
            .get(pane_id)
            .map(|screen| screen.size().columns)
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| descriptor.size.columns)
            })
    }

    /// Persists one durable user-visible agent presentation entry.
    fn persist_agent_presentation_entry(
        &self,
        pane_id: &str,
        style_names: Vec<String>,
        display_lines: Vec<String>,
        copy_lines: Vec<String>,
        ansi_text: String,
    ) {
        if self.agent_presentation_replay_panes.contains(pane_id)
            || display_lines.is_empty()
            || style_names.len() != display_lines.len()
        {
            return;
        }
        let Some(store) = self.agent_transcript_store.as_ref() else {
            return;
        };
        let Some(session) = self.agent_shell_store.get(pane_id) else {
            return;
        };
        let Some(terminal_width) = self.agent_presentation_terminal_width(pane_id) else {
            return;
        };
        let Ok(sequence) = store.next_presentation_sequence(&session.session_id) else {
            return;
        };
        let entry = AgentPresentationEntry {
            conversation_id: session.session_id.clone(),
            sequence,
            created_at_unix_seconds: current_unix_seconds().max(1),
            pane_id: pane_id.to_string(),
            turn_id: session.running_turn_id.clone(),
            terminal_width,
            style_names,
            display_lines,
            copy_lines,
            ansi_text: (!ansi_text.is_empty()).then_some(ansi_text),
        };
        let _ = store.append_presentation(&entry);
    }

    /// Replays persisted presentation entries into the pane terminal buffer.
    pub(in crate::runtime) fn replay_agent_presentation_entries_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        entries: &[AgentPresentationEntry],
    ) -> Result<bool> {
        if entries.is_empty() {
            return Ok(false);
        }
        self.agent_presentation_replay_panes
            .insert(pane_id.to_string());
        let result = (|| -> Result<bool> {
            let mut sorted_entries = entries.iter().collect::<Vec<_>>();
            sorted_entries.sort_by_key(|entry| entry.sequence);
            for entry in sorted_entries {
                if let Some(ansi_text) = entry.ansi_text.as_deref() {
                    let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
                        MezError::new(
                            crate::error::MezErrorKind::NotFound,
                            "agent terminal presentation target pane not found",
                        )
                    })?;
                    if !self.pane_screens.contains_key(pane_id) {
                        let screen = TerminalScreen::new_with_history_config(
                            descriptor.size,
                            self.terminal_history_limit,
                            self.terminal_history_rotate_lines,
                        )?;
                        self.pane_screens.insert(pane_id.to_string(), screen);
                    }
                    self.clear_agent_shell_output_status_line(pane_id)?;
                    let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
                        MezError::invalid_state(
                            "agent terminal presentation screen was not initialized",
                        )
                    })?;
                    Self::feed_agent_terminal_screen(
                        screen,
                        ansi_text.as_bytes(),
                        "replaying persisted agent presentation",
                    )?;
                    if !entry.copy_lines.is_empty() {
                        screen.set_recent_normal_copy_texts(&entry.copy_lines);
                    }
                    continue;
                }
                let styled_lines = entry
                    .display_lines
                    .iter()
                    .enumerate()
                    .map(|(index, line)| {
                        let style = entry
                            .style_names
                            .get(index)
                            .and_then(|name| {
                                AgentTerminalPresentationStyle::from_persistence_name(name)
                            })
                            .unwrap_or(AgentTerminalPresentationStyle::Status);
                        (style, line.clone())
                    })
                    .collect::<Vec<_>>();
                self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)?;
                if !entry.copy_lines.is_empty()
                    && let Some(screen) = self.pane_screens.get_mut(pane_id)
                {
                    screen.set_recent_normal_copy_texts(&entry.copy_lines);
                }
            }
            let state = self
                .agent_prompt_inputs
                .entry(pane_id.to_string())
                .or_insert_with(default_runtime_agent_prompt_input);
            state.display_lines.clear();
            Ok(true)
        })();
        self.agent_presentation_replay_panes.remove(pane_id);
        result
    }

    /// Appends markdown assistant output as styled presentation lines.
    fn append_agent_assistant_markdown_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        markdown: &str,
    ) -> Result<()> {
        let frame_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let table_width = self.agent_terminal_markdown_terminal_width(pane_id)?;
        let body_rendered_lines = wrap_agent_rendered_lines_to_width(
            render_agent_markdown_body_lines(markdown, &self.ui_theme, table_width),
            frame_width,
            table_width,
        );
        let body_rendered_count = body_rendered_lines.len();
        let rendered_lines = frame_agent_markdown_lines(body_rendered_lines, frame_width);
        let trimmed_markdown = markdown.trim_end_matches(['\r', '\n']);
        let raw_copy_lines = if trimmed_markdown.is_empty() {
            vec![String::new()]
        } else {
            trimmed_markdown
                .split('\n')
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let copy_lines = markdown_block_copy_lines(
            rendered_lines.as_slice(),
            body_rendered_count,
            raw_copy_lines,
        );
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Assistant,
            rendered_lines.as_slice(),
            &copy_lines,
        )
    }

    /// Runs the append agent status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Status,
        )
    }

    /// Runs the append agent verbose status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_verbose_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        if self.agent_verbose_enabled(pane_id) {
            self.append_agent_status_text_to_terminal_buffer(pane_id, text)?;
        }
        Ok(())
    }

    /// Runs the append agent thinking text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_thinking_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        if self.agent_thinking_enabled(pane_id) {
            let columns = self.agent_terminal_presentation_columns(pane_id)?;
            self.append_agent_terminal_lines_to_buffer(
                pane_id,
                &agent_thinking_display_lines_for_width(text, columns),
                AgentTerminalPresentationStyle::Status,
            )?;
        }
        Ok(())
    }

    /// Runs the append agent error text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_error_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Error,
        )
    }

    /// Runs the append agent command preview to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_command_preview_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        command: &str,
    ) -> Result<()> {
        /// Defines the MAX AGENT COMMAND PREVIEW LINES const used by this subsystem.
        ///
        /// Keeping this value documented makes the contract explicit at the module
        /// boundary and avoids relying on call-site inference.
        const MAX_AGENT_COMMAND_PREVIEW_LINES: usize = 10;
        let columns = self
            .pane_screens
            .get(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| usize::from(descriptor.size.columns))
            })
            .unwrap_or(80);
        let display_columns = bounded_agent_terminal_presentation_columns(columns);
        let prefix_width =
            UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX) + UnicodeWidthStr::width("$ ");
        let content_columns = display_columns.saturating_sub(prefix_width).max(1);
        let rendered_lines = command_preview_terminal_rendered_lines(
            command,
            content_columns,
            MAX_AGENT_COMMAND_PREVIEW_LINES,
            self.shell_classification_for_pane(pane_id),
            &self.ui_theme,
        );
        let copy_lines = rendered_lines
            .iter()
            .map(|line| line.display.clone())
            .collect::<Vec<_>>();
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Command,
            &rendered_lines,
            &copy_lines,
        )
    }

    /// Runs the append agent terminal lines to buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn append_agent_terminal_lines_to_buffer(
        &mut self,
        pane_id: &str,
        lines: &[String],
        style: AgentTerminalPresentationStyle,
    ) -> Result<()> {
        let styled_lines = lines
            .iter()
            .map(|line| (style, line.clone()))
            .collect::<Vec<_>>();
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Feeds agent-owned presentation bytes into a terminal screen.
    ///
    /// Agent presentation content is model-authored, so terminal rendering must
    /// contain parser defects to the presentation batch instead of allowing a
    /// panic to cross the runtime state boundary.
    ///
    /// # Parameters
    /// - `screen`: The pane screen receiving rendered bytes.
    /// - `bytes`: The already-sanitized terminal bytes to feed.
    /// - `context`: A short description of the presentation operation.
    fn feed_agent_terminal_screen(
        screen: &mut TerminalScreen,
        bytes: &[u8],
        _context: &str,
    ) -> Result<()> {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| screen.feed(bytes))).is_err() {
            return Ok(());
        }
        Ok(())
    }

    /// Appends agent terminal lines with per-line presentation styles.
    ///
    /// Diff previews need additions, deletions, headers, and context to carry
    /// different colors while still flowing through the same pane-buffer gutter
    /// logic as normal agent transcript entries.
    pub(in crate::runtime) fn append_agent_terminal_styled_lines_to_buffer(
        &mut self,
        pane_id: &str,
        styled_lines: &[(AgentTerminalPresentationStyle, String)],
    ) -> Result<()> {
        if styled_lines.is_empty() {
            return Ok(());
        }
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if !self.pane_screens.contains_key(pane_id) {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?;
            self.pane_screens.insert(pane_id.to_string(), screen);
        }
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ansi_text = {
            let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for (style, line) in styled_lines {
                append_styled_agent_terminal_line(&mut bytes, *style, line, &self.ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "appending styled agent lines",
            )?;
            bytes
        };
        self.persist_agent_presentation_entry(
            pane_id,
            styled_lines
                .iter()
                .map(|(style, _line)| style.persistence_name().to_string())
                .collect(),
            styled_lines
                .iter()
                .map(|(_style, line)| line.clone())
                .collect(),
            styled_lines
                .iter()
                .map(|(_style, line)| line.clone())
                .collect(),
            ansi_text,
        );
        Ok(())
    }

    /// Appends transformed assistant display lines while preserving raw copy text.
    fn append_agent_terminal_rendered_lines_to_buffer(
        &mut self,
        pane_id: &str,
        style: AgentTerminalPresentationStyle,
        rendered_lines: &[AgentRenderedLine],
        copy_lines: &[String],
    ) -> Result<()> {
        if rendered_lines.is_empty() {
            return Ok(());
        }
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if !self.pane_screens.contains_key(pane_id) {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?;
            self.pane_screens.insert(pane_id.to_string(), screen);
        }
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ansi_text = {
            let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for line in rendered_lines {
                append_styled_agent_terminal_rendered_line(&mut bytes, style, line, &self.ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "appending rendered agent lines",
            )?;
            screen.set_recent_normal_copy_texts(copy_lines);
            bytes
        };
        self.persist_agent_presentation_entry(
            pane_id,
            vec![style.persistence_name().to_string(); rendered_lines.len()],
            rendered_lines
                .iter()
                .map(|line| line.display.clone())
                .collect(),
            copy_lines.to_vec(),
            ansi_text,
        );
        Ok(())
    }

    /// Updates the transient status rows for a hidden running shell command.
    ///
    /// The preview intentionally has no trailing newline after its final row.
    /// Later output replaces it in place, while the next durable agent
    /// transcript append clears it before writing normal log content.
    pub(in crate::runtime) fn append_agent_shell_output_status_lines_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        lines: &[String],
    ) -> Result<()> {
        if self.agent_shell_view_enabled(pane_id) || lines.is_empty() {
            return Ok(());
        }
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if !self.pane_screens.contains_key(pane_id) {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?;
            self.pane_screens.insert(pane_id.to_string(), screen);
        }
        let columns = self
            .pane_screens
            .get(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .unwrap_or_else(|| usize::from(descriptor.size.columns));
        let content_columns = columns
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1);
        let lines = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                fit_agent_terminal_text_width(&sanitized_agent_terminal_line(line), content_columns)
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return Ok(());
        }
        let previous_line_count = self
            .agent_shell_output_status_lines
            .get(pane_id)
            .map(Vec::len)
            .unwrap_or(0);
        let screen = self.pane_screens.get_mut(pane_id).ok_or_else(|| {
            MezError::invalid_state("agent terminal presentation screen was not initialized")
        })?;
        let mut bytes = String::new();
        if previous_line_count > 0 {
            for index in 0..previous_line_count {
                if index > 0 {
                    bytes.push_str("\x1b[1A");
                }
                bytes.push_str("\r\x1b[2K");
            }
        } else {
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
        }
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                bytes.push_str("\r\n");
            }
            append_styled_agent_terminal_line(
                &mut bytes,
                AgentTerminalPresentationStyle::Status,
                line,
                &self.ui_theme,
            );
            bytes.push_str("\x1b[0m");
        }
        Self::feed_agent_terminal_screen(screen, bytes.as_bytes(), "updating shell output status")?;
        self.agent_shell_output_status_lines
            .insert(pane_id.to_string(), lines);
        Ok(())
    }

    /// Clears transient shell-output status rows if one is active for a pane.
    fn clear_agent_shell_output_status_line(&mut self, pane_id: &str) -> Result<()> {
        let Some(lines) = self.agent_shell_output_status_lines.remove(pane_id) else {
            return Ok(());
        };
        if let Some(screen) = self.pane_screens.get_mut(pane_id) {
            let mut bytes = String::new();
            for index in 0..lines.len() {
                if index > 0 {
                    bytes.push_str("\x1b[1A");
                }
                bytes.push_str("\r\x1b[2K");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "clearing shell output status",
            )?;
        }
        Ok(())
    }

    /// Appends model-authored action summary text as normal-mode thinking logs.
    pub(in crate::runtime) fn append_agent_action_model_thinking_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
    ) -> Result<bool> {
        let thinking_lines = agent_action_model_thinking_lines(action);
        if thinking_lines.is_empty() {
            return Ok(false);
        }
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &agent_thinking_display_lines_for_width(&thinking_lines.join("\n"), columns),
            AgentTerminalPresentationStyle::Status,
        )?;
        Ok(true)
    }

    /// Appends a sanitized mutating-action diff preview to the pane buffer.
    ///
    /// The source text is the cleaned shell observation captured from the hidden
    /// transaction, so this path never exposes shell prompts or Mezzanine wrapper
    /// traffic while still giving users a copyable summary of filesystem changes.
    pub(in crate::runtime) fn append_agent_diff_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines =
            readable_agent_diff_display_lines_for_width(text, &self.ui_theme, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::DiffContext,
            &rendered_lines,
            &[],
        )
    }

    /// Records successful patch diffs for `/list-modified-files`.
    ///
    /// The source text is the same cleaned shell observation used for the
    /// normal diff preview, so counts are derived from the semantic patch diff
    /// rather than from shell echo or wrapper traffic.
    pub(in crate::runtime) fn record_agent_modified_files_from_diff(
        &mut self,
        pane_id: &str,
        text: &str,
    ) {
        let source_lines = cleaned_agent_diff_source_lines(text);
        for section in parse_agent_unified_diff_sections(&source_lines) {
            let path = agent_diff_section_path(&section).to_string();
            if path.is_empty() || path == "/dev/null" {
                continue;
            }
            let added = section
                .lines
                .iter()
                .filter(|line| line.marker == '+')
                .count();
            let removed = section
                .lines
                .iter()
                .filter(|line| line.marker == '-')
                .count();
            let entry = self
                .agent_modified_files
                .entry(pane_id.to_string())
                .or_default()
                .entry(path.clone())
                .or_insert_with(|| RuntimeAgentModifiedFileSummary {
                    path,
                    added: 0,
                    removed: 0,
                });
            entry.added = entry.added.saturating_add(added);
            entry.removed = entry.removed.saturating_add(removed);
        }
    }

    /// Appends a single human-readable action execution line to the pane.
    ///
    /// Semantic file/search and runtime URL actions should be legible in normal
    /// mode without dumping generated commands or result payloads. The line
    /// uses span-level styling so the action remains salient without forcing
    /// arguments to inherit the same visual weight.
    pub(in crate::runtime) fn append_agent_action_execution_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
    ) -> Result<bool> {
        let Some(header) = agent_action_execution_display_header(action) else {
            return Ok(false);
        };
        let thinking_lines = agent_action_model_thinking_lines(action);
        if !thinking_lines.is_empty() && self.agent_thinking_enabled(pane_id) {
            let columns = self.agent_terminal_presentation_columns(pane_id)?;
            self.append_agent_terminal_lines_to_buffer(
                pane_id,
                &agent_thinking_display_lines_for_width(&thinking_lines.join("\n"), columns),
                AgentTerminalPresentationStyle::Status,
            )?;
        }
        let rendered_line = agent_action_execution_rendered_line(&header, &self.ui_theme);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Status,
            &[rendered_line],
            &[],
        )?;
        Ok(true)
    }

    /// Appends a bounded, human-readable action result preview to the pane.
    ///
    /// Normal mode uses this renderer for mutating semantic action diffs. Other
    /// result previews remain reserved for elevated log levels.
    pub(in crate::runtime) fn append_agent_action_result_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
        result: &ActionResult,
        text: &str,
    ) -> Result<()> {
        if result.is_error {
            return Ok(());
        }
        if agent_action_result_uses_diff_preview(action) {
            return self.append_agent_diff_text_to_terminal_buffer(pane_id, text);
        }
        let Some(header) = agent_action_result_display_header(action) else {
            return Ok(());
        };
        let mut styled_lines = vec![(AgentTerminalPresentationStyle::Command, header)];
        styled_lines.extend(
            bounded_agent_action_result_display_lines(text)
                .into_iter()
                .map(|line| (AgentTerminalPresentationStyle::Status, line)),
        );
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Returns whether a cleaned action result preview should render in normal
    /// logging mode.
    pub(in crate::runtime) fn agent_action_result_renders_in_normal_mode(
        &self,
        action: &AgentAction,
    ) -> bool {
        agent_action_result_uses_diff_preview(action)
    }

    /// Runs the agent verbose enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn agent_verbose_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_verbose_status())
    }

    /// Runs the agent thinking enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn agent_thinking_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_thinking())
    }

    /// Runs the agent debug enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn agent_debug_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_debug())
    }

    /// Runs the agent trace enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn agent_trace_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_trace())
    }

    /// Runs the agent shell view enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn agent_shell_view_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_shell_view())
    }

    /// Runs the agent diagnostic level name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn agent_diagnostic_level_name(
        &self,
        pane_id: &str,
    ) -> Option<&'static str> {
        if self.agent_trace_enabled(pane_id) {
            Some("trace")
        } else if self.agent_debug_enabled(pane_id) {
            Some("debug")
        } else {
            None
        }
    }
}
