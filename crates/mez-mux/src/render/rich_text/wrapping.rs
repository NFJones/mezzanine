//! Source-aware wrapping over canonical rich-text rows.
//!
//! Physical rows retain original display-column ranges and shared copy markers.
//! Display-only continuation padding never becomes authored source, and style
//! spans are sliced using the same grapheme-aware coordinates as the text.

use super::*;

/// Applies the selected unbreakable-token policy to one rich-text line.
pub(super) fn wrap_rich_text_line_to_width_with_overflow_policy(
    line: RichTextLine,
    display_width: usize,
    hard_split_unbreakable: bool,
    continuation_indent_override: Option<&str>,
    first_prefix_width: usize,
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
    let continuation_indent = continuation_indent_override
        .map(str::to_string)
        .unwrap_or_else(|| rendered_line_continuation_indent(&line.display, display_width));
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
                .max(first_prefix_width)
                .max(if line.display.starts_with("agent: ") {
                    7
                } else if line.display.starts_with("thinking: ") {
                    10
                } else {
                    0
                })
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
        display_start =
            display_start.saturating_add(terminal_text_width(&remaining[..segment.bytes_consumed]));
        remaining = &remaining[segment.bytes_consumed..];
        // A hard boundary can land immediately before an authored word separator.
        // Keep labeled continuations at their fixed base indent while advancing
        // source coordinates past the separator for style and copy metadata.
        if line.display.starts_with("agent: ") || line.display.starts_with("thinking: ") {
            let skipped = remaining
                .len()
                .saturating_sub(remaining.trim_start_matches(' ').len());
            display_start = display_start.saturating_add(skipped);
            remaining = &remaining[skipped..];
        }
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

/// Takes one bounded segment with an explicit unbreakable-token policy.
pub(super) fn take_rich_text_display_segment_with_overflow_policy(
    text: &str,
    start_column: usize,
    display_width: usize,
    minimum_break_column: usize,
    hard_split_unbreakable: bool,
) -> Option<RichTextDisplaySegment> {
    if text.is_empty() {
        return None;
    }
    let mut width = 0usize;
    let mut boundary_consumed = 0usize;
    let mut boundary_width = 0usize;
    let mut last_space_break: Option<(usize, usize, usize)> = None;
    for (index, grapheme) in UnicodeSegmentation::grapheme_indices(text, true) {
        let grapheme_width = terminal_grapheme_width(grapheme);
        if boundary_consumed == 0 && grapheme_width > display_width {
            return Some(RichTextDisplaySegment {
                text: "…".to_string(),
                bytes_consumed: grapheme.len(),
                start_column,
                end_column: start_column.saturating_add(1),
            });
        }
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
    if boundary_consumed == text.len() {
        return Some(RichTextDisplaySegment {
            text: text.to_string(),
            bytes_consumed: text.len(),
            start_column,
            end_column: start_column.saturating_add(boundary_width),
        });
    }
    if last_space_break.is_none() && boundary_consumed < text.len() && !hard_split_unbreakable {
        let suffix_width = terminal_text_width(&text[boundary_consumed..]);
        return Some(RichTextDisplaySegment {
            text: text.to_string(),
            bytes_consumed: text.len(),
            start_column,
            end_column: start_column
                .saturating_add(boundary_width)
                .saturating_add(suffix_width),
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
