//! Agent-independent rich-text parsing and terminal layout.
//!
//! The parser consumes CommonMark and emits semantic rows with terminal style
//! spans and source-aware copy metadata. Callers choose the colors and any
//! transcript prefixes; this module owns parsing, table layout, wrapping, and
//! width fitting without importing product or agent types.

use super::{char_count as terminal_text_width, push_or_extend_style_span};
use crate::copy::{COPY_SKIP_LINE, COPY_WRAP_CONTINUATION, encode_copy_source_line};
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan, terminal_emoji_width};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Replaces unsafe terminal controls while retaining tabs and printable text.
fn sanitized_terminal_line(line: &str) -> String {
    line.chars()
        .map(|character| {
            if character == '\t' || !character.is_control() {
                character
            } else {
                ' '
            }
        })
        .collect()
}

/// Measures one grapheme using the active terminal compatibility setting.
fn terminal_grapheme_width(grapheme: &str) -> usize {
    mez_terminal::terminal_grapheme_width(grapheme, terminal_emoji_width())
}

/// Caller-selected semantic colors used by rich-text rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RichTextTheme {
    /// Heading foreground.
    pub heading: TerminalColor,
    /// Structural foreground for markers, borders, and quotes.
    pub structural: TerminalColor,
    /// Link foreground.
    pub link: TerminalColor,
    /// Inline-code foreground.
    pub inline_code: TerminalColor,
    /// Foreground for alternating table rows.
    pub table_alternate_row: TerminalColor,
    /// Foreground used for added lines in fenced diff blocks.
    pub diff_addition: TerminalColor,
    /// Foreground used for removed lines in fenced diff blocks.
    pub diff_deletion: TerminalColor,
    /// Optional palette used to highlight recognized fenced programming languages.
    pub syntax: Option<super::SyntaxThemePalette>,
}

/// One complete fenced Markdown code block offered to a specialized renderer.
///
/// The request retains the literal fence information and body so product-owned
/// presentation transforms can decide whether to replace only this block.
#[derive(Debug, Clone, Copy)]
pub struct FencedCodeBlock<'a> {
    /// Complete source-authored fence info string.
    pub info: &'a str,
    /// Literal body without fence delimiters.
    pub body: &'a str,
}

/// Outcome from a specialized fenced-code renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FencedCodeBlockOutcome {
    /// Use the supplied presentation rows in place of the literal fence body.
    Rendered(Vec<RichTextLine>),
    /// Retain the literal body and bypass generic language highlighting.
    PreserveLiteral,
    /// Let the generic syntax renderer or literal fallback handle the fence.
    NotHandled,
}

/// Callback contract for specialized fenced-code presentation renderers.
pub type FencedCodeBlockRenderer = dyn for<'a> FnMut(FencedCodeBlock<'a>) -> FencedCodeBlockOutcome;

/// Presentation-only rendering of one assistant output line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTextLine {
    /// Text to place in a terminal presentation row.
    pub display: String,
    /// Style spans for the displayed text, excluding the gutter.
    pub style_spans: Vec<TerminalStyleSpan>,
    /// Optional raw markdown text to use when copy mode selects this line.
    pub copy_text: Option<String>,
    /// Structural presentation metadata that must not be inferred from glyphs.
    pub kind: RichTextLineKind,
}

/// Structural kind for one rendered presentation row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichTextLineKind {
    /// Ordinary rendered text with no special wrapping behavior.
    Normal,
    /// Synthetic frame row displayed above one rendered markdown block.
    MarkdownFrame,
    /// Markdown thematic-break row rendered as a full-width divider.
    MarkdownRule,
    /// First physical row for one source markdown table row.
    MarkdownTableRow,
    /// Continuation physical row for a wrapped source markdown table row.
    MarkdownTableContinuation,
    /// Separator row generated from the markdown table delimiter line.
    MarkdownTableSeparator,
    /// Presentation-only diagram row that must not be soft-wrapped.
    MarkdownDiagram,
}

impl RichTextLineKind {
    /// Returns whether this row is part of markdown table presentation.
    fn is_markdown_table(self) -> bool {
        matches!(
            self,
            Self::MarkdownTableRow
                | Self::MarkdownTableContinuation
                | Self::MarkdownTableSeparator
                | Self::MarkdownDiagram
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

/// Divider glyph used for markdown thematic breaks and framing.
pub const MARKDOWN_BLOCK_DIVIDER_GLYPH: char = '─';
/// Light foreground-only color used for inline markdown on dark surfaces.
pub const MARKDOWN_LIGHT_NEUTRAL_FOREGROUND: TerminalColor = TerminalColor::Rgb(0xe6, 0xe6, 0xe6);
/// Dark foreground-only color used for inline markdown on light surfaces.
pub const MARKDOWN_DARK_NEUTRAL_FOREGROUND: TerminalColor = TerminalColor::Rgb(0x42, 0x42, 0x42);
/// Muted foreground-only color used for table alternation on light surfaces.
pub const MARKDOWN_DARK_MUTED_FOREGROUND: TerminalColor = TerminalColor::Rgb(0x5a, 0x5a, 0x5a);
pub fn wrap_rich_text_lines_to_width(
    lines: Vec<RichTextLine>,
    display_width: usize,
    table_display_width: usize,
) -> Vec<RichTextLine> {
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
            wrap_rich_text_line_to_width(line, effective_width)
        })
        .collect()
}

/// One physical rich-text row together with its source display-column range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedRichTextLine {
    /// Wrapped presentation row.
    pub line: RichTextLine,
    /// Inclusive source display column where this row begins.
    pub source_start_column: usize,
    /// Exclusive source display column where this row ends.
    pub source_end_column: usize,
    /// Display cells prepended to this row for continuation indentation.
    pub display_prefix_width: usize,
}

/// Wraps one rendered markdown presentation line to a bounded display width.
///
/// # Parameters
/// - `line`: The rendered row to split.
/// - `display_width`: Maximum display cells available after the transcript gutter.
pub fn wrap_rich_text_line_to_width(line: RichTextLine, display_width: usize) -> Vec<RichTextLine> {
    wrap_rich_text_line_to_width_with_source_ranges(line, display_width)
        .into_iter()
        .map(|wrapped| wrapped.line)
        .collect()
}

/// Wraps one rich-text line and reports source columns for each physical row.
///
/// The source ranges let callers translate interactive ranges, such as links,
/// without reproducing the Unicode-aware wrapping algorithm.
pub fn wrap_rich_text_line_to_width_with_source_ranges(
    line: RichTextLine,
    display_width: usize,
) -> Vec<WrappedRichTextLine> {
    wrap_rich_text_line_to_width_with_overflow_policy(line, display_width, false)
}

/// Wraps one rich-text line and hard-splits unbreakable overflow.
///
/// Modal canvases cannot delegate an overwide token to terminal soft wrapping
/// because their final fixed-width compositor would clip the hidden suffix.
/// This variant therefore splits only that overflow at terminal grapheme
/// boundaries while retaining styles, source columns, and copy metadata.
pub fn wrap_rich_text_line_to_width_with_source_ranges_hard(
    line: RichTextLine,
    display_width: usize,
) -> Vec<WrappedRichTextLine> {
    wrap_rich_text_line_to_width_with_overflow_policy(line, display_width, true)
}

/// Applies the selected unbreakable-token policy to one rich-text line.
fn wrap_rich_text_line_to_width_with_overflow_policy(
    line: RichTextLine,
    display_width: usize,
    hard_split_unbreakable: bool,
) -> Vec<WrappedRichTextLine> {
    let line = if line.kind == RichTextLineKind::MarkdownRule
        && terminal_text_width(line.display.as_str()) <= display_width
    {
        expand_markdown_rule_line_to_width(line, display_width)
    } else {
        line
    };
    if terminal_text_width(line.display.as_str()) <= display_width {
        let source_end_column = terminal_text_width(line.display.as_str());
        return vec![WrappedRichTextLine {
            line,
            source_start_column: 0,
            source_end_column,
            display_prefix_width: 0,
        }];
    }
    let continuation_indent = rendered_line_continuation_indent(&line.display, display_width);
    let continuation_width = terminal_text_width(continuation_indent.as_str());
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
        let Some(segment) = take_rich_text_display_segment_with_overflow_policy(
            remaining,
            display_start,
            segment_width,
            minimum_break_column,
            hard_split_unbreakable,
        ) else {
            break;
        };
        let display_prefix = if first {
            String::new()
        } else {
            continuation_indent.clone()
        };
        let display_prefix_width = terminal_text_width(display_prefix.as_str());
        let segment_text = format!("{display_prefix}{}", segment.text);
        let style_spans = style_spans_for_rich_text_segment(
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
            .is_some_and(|copy_text| copy_text != COPY_SKIP_LINE)
        {
            Some(COPY_WRAP_CONTINUATION.to_string())
        } else if line.copy_text.is_some() {
            Some(COPY_SKIP_LINE.to_string())
        } else {
            None
        };
        wrapped.push(WrappedRichTextLine {
            line: RichTextLine {
                display: segment_text,
                style_spans,
                copy_text,
                kind: if first {
                    line.kind
                } else {
                    line.kind.continuation()
                },
            },
            source_start_column: segment.start_column,
            source_end_column: segment.end_column,
            display_prefix_width,
        });
        remaining = &remaining[segment.bytes_consumed..];
        display_start = segment.end_column;
        first = false;
    }
    if wrapped.is_empty() {
        let source_end_column = terminal_text_width(line.display.as_str());
        vec![WrappedRichTextLine {
            line,
            source_start_column: 0,
            source_end_column,
            display_prefix_width: 0,
        }]
    } else {
        wrapped
    }
}

/// Expands one markdown thematic break to the target display width.
fn expand_markdown_rule_line_to_width(
    mut line: RichTextLine,
    display_width: usize,
) -> RichTextLine {
    let current_width = terminal_text_width(line.display.as_str());
    if current_width >= display_width {
        return line;
    }
    let addition_width = display_width.saturating_sub(current_width);
    let glyphs = MARKDOWN_BLOCK_DIVIDER_GLYPH
        .to_string()
        .repeat(addition_width);
    let rendition = line
        .style_spans
        .last()
        .map(|span| span.rendition)
        .unwrap_or_default();
    line.display.push_str(&glyphs);
    if let Some(last_span) = line.style_spans.last_mut()
        && last_span.start.saturating_add(last_span.length) == current_width
        && last_span.rendition == rendition
    {
        last_span.length = last_span.length.saturating_add(addition_width);
        return line;
    }
    line.style_spans.push(TerminalStyleSpan {
        start: current_width,
        length: addition_width,
        rendition,
    });
    line
}

/// One display-cell-bounded segment from a rendered row.
pub struct RichTextDisplaySegment {
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
pub fn take_rich_text_display_segment(
    text: &str,
    start_column: usize,
    display_width: usize,
    minimum_break_column: usize,
) -> Option<RichTextDisplaySegment> {
    take_rich_text_display_segment_with_overflow_policy(
        text,
        start_column,
        display_width,
        minimum_break_column,
        false,
    )
}

/// Takes one bounded segment with an explicit unbreakable-token policy.
fn take_rich_text_display_segment_with_overflow_policy(
    text: &str,
    start_column: usize,
    display_width: usize,
    minimum_break_column: usize,
    hard_split_unbreakable: bool,
) -> Option<RichTextDisplaySegment> {
    if text.is_empty() {
        return None;
    }
    if terminal_text_width(text) <= display_width {
        return Some(RichTextDisplaySegment {
            text: text.to_string(),
            bytes_consumed: text.len(),
            start_column,
            end_column: start_column.saturating_add(terminal_text_width(text)),
        });
    }
    if let Some((_, grapheme)) = UnicodeSegmentation::grapheme_indices(text, true).next()
        && terminal_grapheme_width(grapheme) > display_width
    {
        return Some(RichTextDisplaySegment {
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
        let grapheme_width = terminal_grapheme_width(grapheme);
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
    if last_space_break.is_none() && boundary_consumed < text.len() && !hard_split_unbreakable {
        if let Some((_, grapheme)) = UnicodeSegmentation::grapheme_indices(text, true).next()
            && terminal_grapheme_width(grapheme) > display_width
        {
            return Some(RichTextDisplaySegment {
                text: "…".to_string(),
                bytes_consumed: grapheme.len(),
                start_column,
                end_column: start_column.saturating_add(1),
            });
        }
        return Some(RichTextDisplaySegment {
            text: text.to_string(),
            bytes_consumed: text.len(),
            start_column,
            end_column: start_column.saturating_add(terminal_text_width(text)),
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
        return Some(RichTextDisplaySegment {
            text: text[..boundary_consumed].to_string(),
            bytes_consumed: boundary_consumed,
            start_column,
            end_column: start_column.saturating_add(boundary_width),
        });
    }
    Some(RichTextDisplaySegment {
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
pub fn style_spans_for_rich_text_segment(
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
pub fn rendered_line_continuation_indent(display: &str, display_width: usize) -> String {
    if rendered_line_is_numbered_diff_row(display) {
        return " ".repeat(10.min(display_width.saturating_sub(1)));
    }
    if display.starts_with("user> ") {
        return " ".repeat(5.min(display_width.saturating_sub(1)));
    }
    let prompt = "mez> ";
    let indent_width = if let Some(rest) = display.strip_prefix(prompt) {
        terminal_text_width(prompt) + markdown_local_continuation_indent_width(rest)
    } else {
        markdown_local_continuation_indent_width(display)
    };
    " ".repeat(indent_width.min(display_width.saturating_sub(1)))
}

/// Returns true when a rendered row uses the fixed diff hunk gutter.
///
/// # Parameters
/// - `display`: The rendered row to inspect.
pub fn rendered_line_is_numbered_diff_row(display: &str) -> bool {
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
pub fn markdown_local_continuation_indent_width(display: &str) -> usize {
    let mut width = 0usize;
    let mut byte_index = 0usize;
    for (index, grapheme) in UnicodeSegmentation::grapheme_indices(display, true) {
        if grapheme != " " && grapheme != "\t" {
            byte_index = index;
            break;
        }
        width = width.saturating_add(terminal_grapheme_width(grapheme));
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
pub fn markdown_rendered_line_is_table_row(line: &RichTextLine) -> bool {
    line.kind.is_markdown_table()
}

/// Keeps rendered markdown rows in the ordinary assistant transcript flow.
///
/// Assistant `say` output should not synthesize an extra divider row before the
/// rendered body, even when the body uses markdown presentation styling.
pub fn frame_markdown_lines(lines: Vec<RichTextLine>, _display_width: usize) -> Vec<RichTextLine> {
    lines
}

/// Builds copy text lines for rendered markdown presentation.
pub fn markdown_block_copy_lines(
    rendered_lines: &[RichTextLine],
    _body_rendered_count: usize,
    raw_body_copy_lines: Vec<String>,
    display_prefix: &str,
) -> Vec<String> {
    let mut raw_lines = raw_body_copy_lines.into_iter().enumerate();
    let mut current_source_line = None;
    rendered_lines
        .iter()
        .map(|line| {
            if line
                .copy_text
                .as_deref()
                .is_some_and(|copy_text| copy_text == COPY_SKIP_LINE)
            {
                return COPY_SKIP_LINE.to_string();
            }
            if line.kind == RichTextLineKind::MarkdownFrame {
                return line
                    .copy_text
                    .clone()
                    .unwrap_or_else(|| markdown_rendered_line_copy_text(line, display_prefix));
            }
            if line.kind == RichTextLineKind::MarkdownDiagram
                && let Some(copy_text) = line.copy_text.as_deref()
            {
                return copy_text.to_string();
            }
            if line
                .copy_text
                .as_deref()
                .is_some_and(|copy_text| copy_text == COPY_WRAP_CONTINUATION)
            {
                return current_source_line
                    .as_ref()
                    .map(|(source_index, raw_line): &(usize, String)| {
                        encode_copy_source_line(*source_index, raw_line.as_str())
                    })
                    .unwrap_or_else(|| COPY_SKIP_LINE.to_string());
            }
            if line.kind.consumes_markdown_source_line()
                && let Some((source_index, raw_line)) = raw_lines.next()
            {
                current_source_line = Some((source_index, raw_line.clone()));
                return encode_copy_source_line(source_index, raw_line.as_str());
            }
            COPY_SKIP_LINE.to_string()
        })
        .collect()
}

/// Returns one pane-buffer copy line for a rendered markdown presentation row.
pub fn markdown_rendered_line_copy_text(line: &RichTextLine, display_prefix: &str) -> String {
    if line
        .copy_text
        .as_deref()
        .is_some_and(|copy_text| copy_text == COPY_SKIP_LINE)
    {
        return COPY_SKIP_LINE.to_string();
    }
    format!(
        "{display_prefix}{}",
        line.copy_text.as_ref().unwrap_or(&line.display)
    )
}

/// Restores source-authored blank lines when the rendered body preserves line count.
pub fn render_markdown(
    markdown: &str,
    theme: &RichTextTheme,
    table_display_width: Option<usize>,
) -> Vec<RichTextLine> {
    render_markdown_internal(markdown, theme, table_display_width, None)
}

/// Renders Markdown while allowing a specialized fenced-block renderer to run
/// before generic syntax highlighting and literal fallback.
pub fn render_markdown_with_fenced_block_renderer(
    markdown: &str,
    theme: &RichTextTheme,
    table_display_width: Option<usize>,
    fenced_block_renderer: &mut FencedCodeBlockRenderer,
) -> Vec<RichTextLine> {
    render_markdown_internal(
        markdown,
        theme,
        table_display_width,
        Some(fenced_block_renderer),
    )
}

/// Applies source-line copy metadata after rendering one Markdown document.
fn render_markdown_internal(
    markdown: &str,
    theme: &RichTextTheme,
    table_display_width: Option<usize>,
    fenced_block_renderer: Option<&mut FencedCodeBlockRenderer>,
) -> Vec<RichTextLine> {
    let rendered_lines =
        MarkdownRenderer::render(markdown, theme, table_display_width, fenced_block_renderer);
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
            source_aligned_lines.push(RichTextLine {
                display: String::new(),
                style_spans: Vec::new(),
                copy_text: Some(String::new()),
                kind: RichTextLineKind::Normal,
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
                rendered_line.copy_text = Some(COPY_SKIP_LINE.to_string());
            }
            source_aligned_lines.push(rendered_line);
        }
    }
    source_aligned_lines.extend(rendered.map(|mut rendered_line| {
        if !rendered_line.kind.consumes_markdown_source_line() && rendered_line.copy_text.is_none()
        {
            rendered_line.copy_text = Some(COPY_SKIP_LINE.to_string());
        }
        rendered_line
    }));
    insert_blank_lines_above_markdown_headings(source_aligned_lines)
}

/// Ensures every rendered markdown heading has a presentation blank line above it.
pub fn insert_blank_lines_above_markdown_headings(lines: Vec<RichTextLine>) -> Vec<RichTextLine> {
    let mut spaced = Vec::with_capacity(lines.len().saturating_mul(2));
    for line in lines {
        if markdown_rendered_line_is_heading(&line)
            && spaced
                .last()
                .is_none_or(|previous: &RichTextLine| !previous.display.trim().is_empty())
        {
            spaced.push(markdown_blank_line());
        }
        spaced.push(line);
    }
    spaced
}

/// Returns whether a rendered line came from an ATX markdown heading.
pub fn markdown_rendered_line_is_heading(line: &RichTextLine) -> bool {
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
pub fn markdown_blank_line() -> RichTextLine {
    RichTextLine {
        display: String::new(),
        style_spans: Vec::new(),
        copy_text: Some(COPY_SKIP_LINE.to_string()),
        kind: RichTextLineKind::Normal,
    }
}

/// Prefixes rich-text rows with caller-selected first and continuation labels.
pub fn prefix_rich_text_lines(
    lines: Vec<RichTextLine>,
    first_prefix: &str,
    continuation_prefix: &str,
) -> Vec<RichTextLine> {
    let body_lines = if lines.is_empty() {
        vec![RichTextLine {
            display: String::new(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        }]
    } else {
        lines
    };
    let mut first_nonblank = true;
    body_lines
        .into_iter()
        .map(|mut line| {
            if line.display.is_empty() {
                if line.copy_text.as_deref() == Some(COPY_SKIP_LINE) {
                    return line;
                }
                if line.copy_text.is_some() {
                    line.copy_text = Some(String::new());
                }
                return line;
            }
            let prefix = if first_nonblank {
                first_nonblank = false;
                first_prefix.to_string()
            } else {
                continuation_prefix.to_string()
            };
            let prefix_width = UnicodeWidthStr::width(prefix.as_str());
            for span in &mut line.style_spans {
                span.start = span.start.saturating_add(prefix_width);
            }
            line.display = format!("{prefix}{}", line.display);
            if let Some(copy_text) = line.copy_text.take() {
                if copy_text == COPY_SKIP_LINE || line.kind == RichTextLineKind::MarkdownDiagram {
                    line.copy_text = Some(copy_text);
                } else {
                    line.copy_text = Some(format!("{prefix}{copy_text}"));
                }
            }
            line
        })
        .collect()
}

/// Resolves display-column ranges for Markdown links accepted by a caller.
///
/// The callback translates destinations into caller-owned actions. This keeps
/// CommonMark source/display alignment in the rich-text owner while product
/// schemes and command decoding remain outside the mux crate.
pub fn markdown_link_display_ranges<Action>(
    source_line: &str,
    display: &str,
    resolve: impl Fn(&str) -> Option<Action>,
) -> Vec<(usize, usize, Action)> {
    let mut links = Vec::new();
    let mut source_cursor = 0usize;
    let mut display_cursor = 0usize;
    let mut active_link: Option<(String, Option<usize>)> = None;
    for event in Parser::new_ext(source_line, Options::all()) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) if resolve(&dest_url).is_some() => {
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
                    && let Some(action) = resolve(&destination)
                {
                    let start_column = UnicodeWidthStr::width(&display[..display_start]);
                    let width = UnicodeWidthStr::width(&display[display_start..display_cursor]);
                    links.push((start_column, width, action));
                }
            }
            _ => {}
        }
    }
    links
}

/// Parser-backed CommonMark renderer for pane-buffer markdown presentation.
///
/// The renderer intentionally keeps the output terminal-native rather than
/// attempting HTML layout. It consumes the CommonMark event stream, applies
/// available terminal styles for inline semantics, and emits readable plain
/// text for block structures that have no direct terminal equivalent.
pub struct MarkdownRenderer<'a> {
    lines: Vec<RichTextLine>,
    current: RichTextLine,
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
    heading_foreground: TerminalColor,
    structural_foreground: TerminalColor,
    link_foreground: TerminalColor,
    inline_code_foreground: TerminalColor,
    table_alternate_row_foreground: TerminalColor,
    diff_addition_foreground: TerminalColor,
    diff_deletion_foreground: TerminalColor,
    syntax_palette: Option<super::SyntaxThemePalette>,
    code_block: Option<MarkdownCodeBlockState>,
    fenced_block_renderer: Option<&'a mut FencedCodeBlockRenderer>,
    current_prefix_only: bool,
}

impl<'a> MarkdownRenderer<'a> {
    /// Renders markdown using CommonMark plus the common GitHub-style extensions.
    fn render(
        markdown: &str,
        theme: &RichTextTheme,
        table_display_width: Option<usize>,
        fenced_block_renderer: Option<&'a mut FencedCodeBlockRenderer>,
    ) -> Vec<RichTextLine> {
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

        let mut renderer = Self::new(theme, table_display_width, fenced_block_renderer);
        for event in Parser::new_ext(markdown, options) {
            renderer.handle_event(event);
        }
        renderer.finish_current_line();
        renderer.trim_trailing_blank_lines();
        renderer.lines
    }

    /// Handles one parser event, delegating table internals to table capture.
    fn handle_event(&mut self, event: Event<'_>) {
        if self.code_block.is_some() {
            self.handle_code_block_event(event);
            return;
        }
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
                self.append_thematic_break();
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
                let foreground = self.heading_foreground;
                self.push_style(|style| {
                    style.foreground = Some(foreground);
                    style.background = None;
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
            Tag::CodeBlock(kind) => {
                self.start_block();
                let (info, fenced) = match kind {
                    CodeBlockKind::Fenced(info) => (info.into_string(), true),
                    CodeBlockKind::Indented => (String::new(), false),
                };
                self.code_block = Some(MarkdownCodeBlockState {
                    info,
                    fenced,
                    body: String::new(),
                });
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
                    self.heading_foreground,
                    self.structural_foreground,
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
            TagEnd::CodeBlock => self.render_code_block(),
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

    /// Captures one complete code block so fenced renderers can inspect it.
    fn handle_code_block_event(&mut self, event: Event<'_>) {
        match event {
            Event::End(TagEnd::CodeBlock) => self.render_code_block(),
            Event::Text(text) => {
                if let Some(block) = self.code_block.as_mut() {
                    block.body.push_str(text.as_ref());
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(block) = self.code_block.as_mut() {
                    block.body.push('\n');
                }
            }
            _ => {}
        }
    }

    /// Renders a captured code block through specialized, generic, or literal paths.
    fn render_code_block(&mut self) {
        let Some(block) = self.code_block.take() else {
            return;
        };
        if block.fenced {
            if let Some(renderer) = self.fenced_block_renderer.as_mut() {
                match renderer(FencedCodeBlock {
                    info: block.info.as_str(),
                    body: block.body.as_str(),
                }) {
                    FencedCodeBlockOutcome::Rendered(lines) => {
                        self.lines.extend(lines);
                        return;
                    }
                    FencedCodeBlockOutcome::PreserveLiteral => {
                        self.append_fenced_literal_code_block(&block);
                        return;
                    }
                    FencedCodeBlockOutcome::NotHandled => {}
                }
            }
            if let Some(palette) = self.syntax_palette {
                let theme = super::syntax_theme("markdown-fence", palette);
                if let Some(mut highlighter) =
                    super::syntax_highlighter_for_fence(&block.info, &theme)
                {
                    self.append_fenced_code_delimiter(block.info.as_str());
                    for source_line in block.body.split_terminator('\n') {
                        let mut line = self.literal_code_line(source_line);
                        let display = line.display.clone();
                        super::append_syntax_spans(&mut line, 0, &display, &mut highlighter);
                        self.lines.push(line);
                    }
                    self.append_fenced_code_delimiter("");
                    return;
                }
            }
        }
        if block.fenced {
            self.append_fenced_literal_code_block(&block);
        } else {
            self.append_literal_code_block(block.body.as_str());
        }
    }

    /// Emits a fenced block with literal source delimiters for raw-copy alignment.
    fn append_fenced_literal_code_block(&mut self, block: &MarkdownCodeBlockState) {
        self.append_fenced_code_delimiter(block.info.as_str());
        self.append_literal_code_block(block.body.as_str());
        self.append_fenced_code_delimiter("");
    }

    /// Emits one visible fenced-code delimiter with the neutral code foreground.
    fn append_fenced_code_delimiter(&mut self, info: &str) {
        self.lines
            .push(self.literal_code_line(&format!("```{info}")));
    }

    /// Emits literal code rows with the existing neutral code foreground.
    fn append_literal_code_block(&mut self, body: &str) {
        for source_line in body.split_terminator('\n') {
            self.lines.push(self.literal_code_line(source_line));
        }
    }

    /// Builds one sanitized literal code row.
    fn literal_code_line(&self, source_line: &str) -> RichTextLine {
        let display = sanitized_terminal_line(source_line);
        let width = terminal_text_width(display.as_str());
        RichTextLine {
            display,
            style_spans: (width > 0)
                .then_some(TerminalStyleSpan {
                    start: 0,
                    length: width,
                    rendition: GraphicRendition {
                        foreground: Some(self.inline_code_foreground),
                        background: None,
                        inverse: false,
                        ..GraphicRendition::default()
                    },
                })
                .into_iter()
                .collect(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
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
                self.append_styled_text(&sanitized_terminal_line(part), self.active);
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
        self.append_styled_text(&sanitized_terminal_line(code), style);
    }

    /// Appends inline math with a lightweight math marker and italic style.
    fn append_inline_math(&mut self, math: &str) {
        self.ensure_line_prefix();
        self.current_prefix_only = false;
        let mut style = self.active;
        style.italic = true;
        self.append_styled_text(&format!("${}$", sanitized_terminal_line(math)), style);
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
            self.append_styled_text(&sanitized_terminal_line(line), style);
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

    /// Appends one markdown thematic break using subdued structural styling.
    fn append_thematic_break(&mut self) {
        self.ensure_line_prefix();
        self.current_prefix_only = false;
        self.current.kind = RichTextLineKind::MarkdownRule;
        self.append_styled_text(
            &MARKDOWN_BLOCK_DIVIDER_GLYPH.to_string(),
            GraphicRendition {
                foreground: Some(self.structural_foreground),
                background: None,
                dim: true,
                ..GraphicRendition::default()
            },
        );
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
        let rendition = if prefix.contains('>') {
            GraphicRendition {
                foreground: Some(self.structural_foreground),
                background: None,
                dim: true,
                ..GraphicRendition::default()
            }
        } else {
            GraphicRendition::default()
        };
        self.append_styled_text(prefix, rendition);
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
            let width = terminal_grapheme_width(grapheme);
            let start = terminal_text_width(self.current.display.as_str());
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
            RichTextLine {
                display: String::new(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: RichTextLineKind::Normal,
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

impl<'a> MarkdownRenderer<'a> {
    /// Builds an empty markdown renderer for one active UI theme.
    fn new(
        theme: &RichTextTheme,
        table_display_width: Option<usize>,
        fenced_block_renderer: Option<&'a mut FencedCodeBlockRenderer>,
    ) -> Self {
        Self {
            lines: Vec::new(),
            current: RichTextLine {
                display: String::new(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: RichTextLineKind::Normal,
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
            heading_foreground: theme.heading,
            structural_foreground: theme.structural,
            link_foreground: theme.link,
            inline_code_foreground: theme.inline_code,
            table_alternate_row_foreground: theme.table_alternate_row,
            diff_addition_foreground: theme.diff_addition,
            diff_deletion_foreground: theme.diff_deletion,
            syntax_palette: theme.syntax,
            code_block: None,
            fenced_block_renderer,
            current_prefix_only: false,
        }
    }
}

/// Captured source needed to select a fenced-code presentation path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownCodeBlockState {
    info: String,
    fenced: bool,
    body: String,
}

/// Tracks list numbering while rendering nested markdown lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownListState {
    /// Next ordered-list number to display.
    next_number: u64,
    /// Whether the list is ordered.
    ordered: bool,
}

/// Captures a CommonMark table before emitting aligned terminal rows.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownTableState {
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
    /// Foreground used for table header rows.
    header_foreground: TerminalColor,
    /// Foreground used for table borders and separators.
    border_foreground: TerminalColor,
    /// Foreground used for alternating body rows.
    alternate_row_foreground: TerminalColor,
}

impl MarkdownTableState {
    /// Builds a table capture state for parser-provided alignments.
    fn new(
        alignments: Vec<Alignment>,
        display_width: Option<usize>,
        header_foreground: TerminalColor,
        border_foreground: TerminalColor,
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
            header_foreground,
            border_foreground,
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
            .push_str(&sanitized_terminal_line(text).replace('\n', " "));
    }

    /// Renders the captured table as aligned box-drawing terminal rows.
    fn render_lines(self) -> Vec<RichTextLine> {
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
                let mut line = RichTextLine {
                    display: rendered.clone(),
                    style_spans: Vec::new(),
                    copy_text: Some(if physical_row == 0 {
                        rendered
                    } else {
                        COPY_SKIP_LINE.to_string()
                    }),
                    kind: if physical_row == 0 {
                        RichTextLineKind::MarkdownTableRow
                    } else {
                        RichTextLineKind::MarkdownTableContinuation
                    },
                };
                self.apply_row_style(&mut line, row_index);
                lines.push(line);
            }
            if row_index + 1 == self.header_rows {
                lines.push(RichTextLine {
                    display: self.render_separator(&widths),
                    style_spans: vec![TerminalStyleSpan {
                        start: 0,
                        length: terminal_text_width(self.render_separator(&widths).as_str()),
                        rendition: GraphicRendition {
                            foreground: Some(self.border_foreground),
                            background: None,
                            dim: true,
                            ..GraphicRendition::default()
                        },
                    }],
                    copy_text: None,
                    kind: RichTextLineKind::MarkdownTableSeparator,
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
                    .map(|cell| terminal_text_width(cell.as_str()))
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
        if terminal_text_width(text) <= width {
            return (text.to_string(), text.len());
        }
        if let Some((_, grapheme)) = UnicodeSegmentation::grapheme_indices(text, true).next()
            && terminal_grapheme_width(grapheme) > width
        {
            return ("…".to_string(), grapheme.len());
        }
        let mut used_width = 0usize;
        let mut boundary_consumed = 0usize;
        let mut last_space_break: Option<(usize, usize)> = None;
        for (index, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
            let grapheme_width = terminal_grapheme_width(grapheme);
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
    fn apply_row_style(&self, line: &mut RichTextLine, row_index: usize) {
        let length = terminal_text_width(line.display.as_str());
        if length == 0 {
            return;
        }
        let rendition = if row_index < self.header_rows {
            GraphicRendition {
                foreground: Some(self.header_foreground),
                background: None,
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
        self.apply_border_style(line);
    }

    /// Applies subdued foreground styling to visible box-drawing table borders.
    fn apply_border_style(&self, line: &mut RichTextLine) {
        for (start, grapheme) in UnicodeSegmentation::grapheme_indices(line.display.as_str(), true)
        {
            if matches!(grapheme, "│" | "├" | "┤" | "┼" | "─") {
                push_or_extend_style_span(
                    &mut line.style_spans,
                    TerminalStyleSpan {
                        start: terminal_text_width(&line.display[..start]),
                        length: terminal_grapheme_width(grapheme),
                        rendition: GraphicRendition {
                            foreground: Some(self.border_foreground),
                            background: None,
                            dim: true,
                            ..GraphicRendition::default()
                        },
                    },
                );
            }
        }
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
        let cell_width = terminal_text_width(cell);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> RichTextTheme {
        RichTextTheme {
            heading: TerminalColor::Rgb(1, 2, 3),
            structural: TerminalColor::Rgb(4, 5, 6),
            link: TerminalColor::Rgb(7, 8, 9),
            inline_code: TerminalColor::Rgb(10, 11, 12),
            table_alternate_row: TerminalColor::Rgb(13, 14, 15),
            diff_addition: TerminalColor::Rgb(16, 17, 18),
            diff_deletion: TerminalColor::Rgb(19, 20, 21),
            syntax: Some(crate::render::SyntaxThemePalette {
                plain: TerminalColor::Rgb(20, 21, 22),
                background: None,
                comment: TerminalColor::Rgb(23, 24, 25),
                string: TerminalColor::Rgb(26, 27, 28),
                number: TerminalColor::Rgb(29, 30, 31),
                keyword: TerminalColor::Rgb(32, 33, 34),
                r#type: TerminalColor::Rgb(35, 36, 37),
                function: TerminalColor::Rgb(38, 39, 40),
                operator: TerminalColor::Rgb(41, 42, 43),
            }),
        }
    }

    /// Verifies CommonMark tables become structural rows and retain source
    /// metadata without depending on product transcript types.
    #[test]
    fn markdown_tables_render_as_structural_rich_text_rows() {
        let lines = render_markdown("| A | B |\n| - | - |\n| one | two |", &theme(), Some(30));
        assert!(lines.iter().any(markdown_rendered_line_is_table_row));
        assert!(
            lines
                .iter()
                .any(|line| line.kind == RichTextLineKind::MarkdownTableSeparator)
        );
        assert!(lines.iter().any(|line| line.copy_text.is_some()));
    }

    /// Verifies fenced Rust blocks use the active syntax palette without
    /// adding a trailing presentation row for the closing fence newline.
    #[test]
    fn fenced_rust_blocks_use_theme_syntax_spans() {
        let lines = render_markdown("```RUST title\nfn main() {}\n```", &theme(), None);

        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0].display, "```RUST title");
        assert_eq!(lines[1].display, "fn main() {}");
        assert_eq!(lines[2].display, "```");
        assert_eq!(
            lines
                .iter()
                .map(|line| line.copy_text.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("```RUST title"), Some("fn main() {}"), Some("```")]
        );
        assert!(
            lines[1].style_spans.iter().any(|span| {
                matches!(
                    span.rendition.foreground,
                    Some(
                        TerminalColor::Rgb(32, 33, 34)
                            | TerminalColor::Rgb(35, 36, 37)
                            | TerminalColor::Rgb(38, 39, 40)
                            | TerminalColor::Rgb(41, 42, 43)
                    )
                )
            }),
            "{lines:?}"
        );
    }

    /// Verifies specialized fenced renderers run before generic highlighting
    /// and can preserve a literal body when their own presentation fails.
    #[test]
    fn specialized_fenced_renderer_precedes_generic_highlighting() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed_calls = std::rc::Rc::clone(&calls);
        let mut renderer = move |block: FencedCodeBlock<'_>| {
            observed_calls
                .borrow_mut()
                .push((block.info.to_string(), block.body.to_string()));
            FencedCodeBlockOutcome::PreserveLiteral
        };

        let lines = render_markdown_with_fenced_block_renderer(
            "```rust\nfn main() {}\n```",
            &theme(),
            None,
            &mut renderer,
        );

        assert_eq!(
            calls.borrow().as_slice(),
            [("rust".to_string(), "fn main() {}\n".to_string())]
        );
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[1].display, "fn main() {}");
        assert_eq!(
            lines[1].style_spans[0].rendition.foreground,
            Some(TerminalColor::Rgb(10, 11, 12))
        );
    }

    /// Verifies width wrapping preserves the first source identity and marks
    /// continuation rows for source-aware copy selection.
    #[test]
    fn rich_text_wrapping_marks_copy_continuations() {
        let line = RichTextLine {
            display: "prefix alpha beta gamma".to_string(),
            style_spans: Vec::new(),
            copy_text: Some("raw source".to_string()),
            kind: RichTextLineKind::Normal,
        };
        let wrapped = wrap_rich_text_line_to_width(line, 12);
        assert!(wrapped.len() > 1);
        assert_eq!(wrapped[0].copy_text.as_deref(), Some("raw source"));
        assert_eq!(
            wrapped[1].copy_text.as_deref(),
            Some(COPY_WRAP_CONTINUATION)
        );
    }

    /// Verifies fixed-width modal wrapping preserves an unbreakable token by
    /// splitting it at grapheme boundaries instead of exposing it to the
    /// compositor's defensive clipping path.
    #[test]
    fn rich_text_hard_wrapping_bounds_unbreakable_tokens() {
        let line = RichTextLine {
            display: "averyveryverylongtoken".to_string(),
            style_spans: Vec::new(),
            copy_text: Some("averyveryverylongtoken".to_string()),
            kind: RichTextLineKind::Normal,
        };

        let wrapped = wrap_rich_text_line_to_width_with_source_ranges_hard(line, 8);

        assert!(wrapped.len() > 1, "{wrapped:?}");
        assert!(
            wrapped
                .iter()
                .all(|line| terminal_text_width(line.line.display.as_str()) <= 8),
            "{wrapped:?}"
        );
    }
}
