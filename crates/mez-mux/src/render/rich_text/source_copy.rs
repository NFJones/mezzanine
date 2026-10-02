//! Canonical source association for rich-text copy projections.
//!
//! Synthetic rows never consume authored source. Wrapped rows reuse the exact
//! source group, while fenced blocks and specialized diagrams retain their own
//! raw-source metadata. Presentation owners must not invent alternate copy rules.

use super::{RichTextLine, RichTextLineKind};
use crate::copy::{COPY_SKIP_LINE, COPY_WRAP_CONTINUATION, encode_copy_source_line};

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
            if matches!(
                line.kind,
                RichTextLineKind::MarkdownDiagram | RichTextLineKind::MarkdownCodeBlock
            ) && let Some(copy_text) = line.copy_text.as_deref()
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
