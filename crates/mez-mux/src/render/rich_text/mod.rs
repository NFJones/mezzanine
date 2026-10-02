//! Agent-independent rich-text parsing and terminal layout.
//!
//! The facade keeps one semantic row and renderer-state contract. CommonMark
//! event dispatch, complete fenced-block handling, table geometry, wrapping and
//! source-copy projection live in focused child owners. All use the same terminal
//! cell coordinates and copy markers; no owner invents a second source mapping.
//! Callers choose colors, transcript prefixes and specialized fence callbacks;
//! product policy remains outside this agent-independent module.

mod source_copy;
pub use source_copy::{markdown_block_copy_lines, markdown_rendered_line_copy_text};

mod wrapping;
use wrapping::{
    take_rich_text_display_segment_with_overflow_policy,
    wrap_rich_text_line_to_width_with_overflow_policy,
};

mod table_geometry;

mod fenced_blocks;

mod markdown_events;

use super::{char_count as terminal_text_width, push_or_extend_style_span};
use crate::copy::{COPY_SKIP_LINE, COPY_WRAP_CONTINUATION};
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
    /// Final rendered row of one top-level Markdown paragraph.
    MarkdownParagraph,
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
    /// Generic fenced-code row whose copy metadata owns the complete raw fence.
    MarkdownCodeBlock,
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

/// Wraps one rich-text line with an explicit display-only continuation indent.
///
/// The first physical row uses the full width. Every later row reserves the
/// requested indent within that same width, preserving source-copy metadata.
pub fn wrap_rich_text_line_to_width_with_continuation_indent(
    line: RichTextLine,
    display_width: usize,
    continuation_indent: &str,
) -> Vec<RichTextLine> {
    wrap_rich_text_line_to_width_with_overflow_policy(
        line,
        display_width,
        false,
        Some(continuation_indent),
        0,
    )
    .into_iter()
    .map(|wrapped| wrapped.line)
    .collect()
}

/// Wraps one rich-text line with an explicit continuation indent and hard-splits
/// unbreakable overflow that would otherwise exceed the fixed display width.
pub fn wrap_rich_text_line_to_width_with_continuation_indent_hard(
    line: RichTextLine,
    display_width: usize,
    continuation_indent: &str,
) -> Vec<RichTextLine> {
    wrap_rich_text_line_to_width_with_overflow_policy(
        line,
        display_width,
        true,
        Some(continuation_indent),
        0,
    )
    .into_iter()
    .map(|wrapped| wrapped.line)
    .collect()
}

/// Hard-wraps a labeled row without treating whitespace inside its first-row
/// prefix as a break, even when later rows use a shorter display-only indent.
pub fn wrap_rich_text_line_to_width_with_prefix_and_continuation_indent_hard(
    line: RichTextLine,
    display_width: usize,
    first_prefix_width: usize,
    continuation_indent: &str,
) -> Vec<RichTextLine> {
    wrap_rich_text_line_to_width_with_overflow_policy(
        line,
        display_width,
        true,
        Some(continuation_indent),
        first_prefix_width,
    )
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
    wrap_rich_text_line_to_width_with_overflow_policy(line, display_width, false, None, 0)
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
    wrap_rich_text_line_to_width_with_overflow_policy(line, display_width, true, None, 0)
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
    if display.starts_with("agent: ") {
        return " ".repeat(5.min(display_width.saturating_sub(1)));
    }
    if display.starts_with("thinking: ") {
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
        return insert_blank_lines_after_markdown_paragraphs(
            insert_blank_lines_around_markdown_headings(rendered_lines),
        );
    }

    let mut rendered = rendered_lines.into_iter().peekable();
    let mut source_aligned_lines = Vec::new();
    for source_line in source_lines {
        if source_line.trim().is_empty() {
            while let Some(rendered_line) =
                rendered.next_if(|line| line.kind == RichTextLineKind::MarkdownTableContinuation)
            {
                source_aligned_lines.push(rendered_line);
            }
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
    insert_blank_lines_after_markdown_paragraphs(insert_blank_lines_after_markdown_tables(
        insert_blank_lines_around_markdown_headings(source_aligned_lines),
    ))
}

/// Inserts one presentation-only blank row between a top-level paragraph and
/// following visible Markdown content.
fn insert_blank_lines_after_markdown_paragraphs(lines: Vec<RichTextLine>) -> Vec<RichTextLine> {
    let mut spaced = Vec::with_capacity(lines.len().saturating_add(1));
    let mut lines = lines.into_iter().peekable();
    while let Some(line) = lines.next() {
        let is_paragraph_end = line.kind == RichTextLineKind::MarkdownParagraph;
        spaced.push(line);
        if is_paragraph_end
            && lines
                .peek()
                .is_some_and(|following| !following.display.trim().is_empty())
        {
            spaced.push(markdown_blank_line());
        }
    }
    spaced
}

/// Ensures every rendered markdown heading has presentation blank lines around it.
pub fn insert_blank_lines_around_markdown_headings(lines: Vec<RichTextLine>) -> Vec<RichTextLine> {
    let mut spaced = Vec::with_capacity(lines.len().saturating_mul(2));
    let mut lines = lines.into_iter().peekable();
    while let Some(line) = lines.next() {
        let is_heading = markdown_rendered_line_is_heading(&line);
        if is_heading
            && spaced
                .last()
                .is_none_or(|previous: &RichTextLine| !previous.display.trim().is_empty())
        {
            spaced.push(markdown_blank_line());
        }
        spaced.push(line);
        if is_heading
            && lines
                .peek()
                .is_some_and(|following| !following.display.trim().is_empty())
        {
            spaced.push(markdown_blank_line());
        }
    }
    spaced
}

/// Inserts one presentation-only blank row between a rendered table and
/// following visible Markdown content.
fn insert_blank_lines_after_markdown_tables(lines: Vec<RichTextLine>) -> Vec<RichTextLine> {
    let mut spaced = Vec::with_capacity(lines.len().saturating_add(1));
    let mut lines = lines.into_iter().peekable();
    while let Some(line) = lines.next() {
        let is_table = line.kind.is_markdown_table();
        spaced.push(line);
        if is_table
            && lines.peek().is_some_and(|following| {
                !following.kind.is_markdown_table() && !following.display.trim().is_empty()
            })
        {
            spaced.push(markdown_blank_line());
        }
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
                if copy_text == COPY_SKIP_LINE
                    || matches!(
                        line.kind,
                        RichTextLineKind::MarkdownDiagram | RichTextLineKind::MarkdownCodeBlock
                    )
                {
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

    /// Finishes a paragraph and marks its final top-level row for spacing.
    fn finish_paragraph(&mut self) {
        if self.list_stack.is_empty() && !self.current.display.is_empty() {
            self.current.kind = RichTextLineKind::MarkdownParagraph;
        }
        self.finish_current_line();
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
#[derive(Debug, Clone, Default, PartialEq)]
struct MarkdownTableCell {
    text: String,
    style_spans: Vec<TerminalStyleSpan>,
}

impl MarkdownTableCell {
    /// Reports whether this cell has no visible text.
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Returns a display-trimmed cell with style coordinates shifted to match.
    fn trimmed(self) -> Self {
        let trimmed = self.text.trim();
        let Some(byte_start) = self.text.find(trimmed) else {
            return Self::default();
        };
        let start = terminal_text_width(&self.text[..byte_start]);
        let end = start.saturating_add(terminal_text_width(trimmed));
        Self {
            text: trimmed.to_string(),
            style_spans: style_spans_for_rich_text_segment(&self.style_spans, start, end, 0),
        }
    }
}

/// Captures a CommonMark table before emitting aligned terminal rows.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownTableState {
    /// Column alignments reported by the parser.
    alignments: Vec<Alignment>,
    /// Completed rows.
    rows: Vec<Vec<MarkdownTableCell>>,
    /// Row currently being captured.
    current_row: Vec<MarkdownTableCell>,
    /// Cell currently being captured.
    current_cell: MarkdownTableCell,
    /// Active inline rendition while capturing a cell.
    active: GraphicRendition,
    /// Nested inline renditions inside the current cell.
    style_stack: Vec<GraphicRendition>,
    /// Number of active Markdown links in the current cell.
    link_depth: usize,
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
            current_cell: MarkdownTableCell::default(),
            active: GraphicRendition::default(),
            style_stack: Vec::new(),
            link_depth: 0,
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
        self.current_cell = MarkdownTableCell::default();
        self.active = GraphicRendition::default();
        self.style_stack.clear();
        self.link_depth = 0;
    }

    /// Finishes the current table cell.
    fn finish_cell(&mut self) {
        self.current_row
            .push(std::mem::take(&mut self.current_cell).trimmed());
        self.active = GraphicRendition::default();
        self.style_stack.clear();
        self.link_depth = 0;
    }

    /// Appends text into the current table cell.
    fn append_cell_text(&mut self, text: &str) {
        self.append_cell_styled_text(text, self.active);
    }

    /// Appends styled text to the current cell using display-cell coordinates.
    fn append_cell_styled_text(&mut self, text: &str, rendition: GraphicRendition) {
        let text = if !self.current_cell.text.is_empty() && text.starts_with(char::is_whitespace) {
            " ".to_string()
        } else {
            sanitized_terminal_line(text).replace('\n', " ")
        };
        for grapheme in UnicodeSegmentation::graphemes(text.as_str(), true) {
            let start = terminal_text_width(self.current_cell.text.as_str());
            let width = terminal_grapheme_width(grapheme);
            self.current_cell.text.push_str(grapheme);
            if width > 0 && rendition != GraphicRendition::default() {
                push_or_extend_style_span(
                    &mut self.current_cell.style_spans,
                    TerminalStyleSpan {
                        start,
                        length: width,
                        rendition,
                    },
                );
            }
        }
    }

    /// Pushes one nested inline style while capturing a cell.
    fn push_style(&mut self, apply: impl FnOnce(&mut GraphicRendition)) {
        self.style_stack.push(self.active);
        apply(&mut self.active);
    }

    /// Restores the previous inline style while capturing a cell.
    fn pop_style(&mut self) {
        if let Some(style) = self.style_stack.pop() {
            self.active = style;
        }
    }
}

#[cfg(test)]
mod tests;
