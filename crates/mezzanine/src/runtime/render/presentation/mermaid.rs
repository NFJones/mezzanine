//! Bounded, agent-only Mermaid fence presentation.
//!
//! This module owns product policy for replacing a completed agent Markdown
//! `mermaid` fence with terminal-native Unicode diagram rows. The mux remains
//! renderer-neutral: malformed, unsupported, over-limit, or overwide diagrams
//! decline replacement so its literal fenced-code fallback retains the source.

use super::{RichTextLine, RichTextLineKind, UnicodeWidthStr};
use merman::ascii::{AsciiRenderOptions, HeadlessAsciiRenderer};
use mez_mux::copy::COPY_SKIP_LINE;
use mez_mux::render::{FencedCodeBlock, FencedCodeBlockOutcome};

const MAX_MERMAID_SOURCE_BYTES: usize = 16 * 1024;
const MAX_MERMAID_FENCES: usize = 8;
const MAX_MERMAID_RENDERED_ROWS: usize = 96;
const MAX_MERMAID_RENDERED_CELLS: usize = 16 * 1024;
const MAX_MERMAID_GRID_CELLS: usize = 16 * 1024;

/// Attempts a bounded terminal-native replacement for one agent Markdown fence.
pub(super) fn render_agent_mermaid_fence(
    fence: FencedCodeBlock<'_>,
    available_width: usize,
    mermaid_fence_count: &mut usize,
) -> FencedCodeBlockOutcome {
    if !fence
        .info
        .split_whitespace()
        .next()
        .is_some_and(|language| language.eq_ignore_ascii_case("mermaid"))
    {
        return FencedCodeBlockOutcome::NotHandled;
    }
    *mermaid_fence_count = mermaid_fence_count.saturating_add(1);
    if *mermaid_fence_count > MAX_MERMAID_FENCES {
        return FencedCodeBlockOutcome::PreserveLiteral;
    }
    if fence.body.len() > MAX_MERMAID_SOURCE_BYTES || available_width == 0 {
        return FencedCodeBlockOutcome::PreserveLiteral;
    }

    let mut options = AsciiRenderOptions::unicode();
    options.max_grid_cells = MAX_MERMAID_GRID_CELLS;
    let renderer = HeadlessAsciiRenderer::new()
        .with_strict_parsing()
        .with_ascii_options(options);
    let Ok(Some(rendered)) = renderer.render_ascii_sync(fence.body) else {
        return FencedCodeBlockOutcome::PreserveLiteral;
    };
    let rows = rendered
        .lines()
        .map(sanitize_mermaid_row)
        .collect::<Vec<_>>();
    let cells = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.as_str()))
        .sum::<usize>();
    if rows.is_empty()
        || rows.len() > MAX_MERMAID_RENDERED_ROWS
        || cells > MAX_MERMAID_RENDERED_CELLS
        || rows
            .iter()
            .any(|row| UnicodeWidthStr::width(row.as_str()) > available_width)
    {
        return FencedCodeBlockOutcome::PreserveLiteral;
    }

    let raw_fence = format!("```{}\n{}```", fence.info, fence.body);
    FencedCodeBlockOutcome::Rendered(
        rows.into_iter()
            .enumerate()
            .map(|(index, display)| RichTextLine {
                display,
                style_spans: Vec::new(),
                copy_text: Some(if index == 0 {
                    raw_fence.clone()
                } else {
                    COPY_SKIP_LINE.to_string()
                }),
                kind: RichTextLineKind::MarkdownDiagram,
            })
            .collect(),
    )
}

/// Removes terminal controls from renderer output while retaining printable text.
fn sanitize_mermaid_row(row: &str) -> String {
    row.chars()
        .map(|character| {
            if character == '\t' || !character.is_control() {
                character
            } else {
                ' '
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies a fitting flowchart becomes plain terminal rows without control
    /// sequences, leaving terminal encoding to the owning presentation layer.
    #[test]
    fn fitting_flowchart_renders_as_terminal_rows() {
        let mut mermaid_fence_count = 0;
        let outcome = render_agent_mermaid_fence(
            FencedCodeBlock {
                info: "mermaid",
                body: "flowchart LR\nA[Start] --> B[Done]",
            },
            80,
            &mut mermaid_fence_count,
        );

        let FencedCodeBlockOutcome::Rendered(lines) = outcome else {
            panic!("expected rendered Mermaid rows");
        };
        assert!(!lines.is_empty(), "{lines:?}");
        assert!(lines.iter().all(|line| {
            line.kind == RichTextLineKind::MarkdownDiagram
                && !line.display.contains('\u{1b}')
                && UnicodeWidthStr::width(line.display.as_str()) <= 80
        }));
    }

    /// Verifies malformed Mermaid preserves literal Markdown rather than
    /// replacing only part of the fenced source with a renderer failure.
    #[test]
    fn malformed_mermaid_preserves_literal_fence() {
        let mut mermaid_fence_count = 0;
        assert_eq!(
            render_agent_mermaid_fence(
                FencedCodeBlock {
                    info: "mermaid",
                    body: "flowchart ???",
                },
                80,
                &mut mermaid_fence_count,
            ),
            FencedCodeBlockOutcome::PreserveLiteral
        );
    }

    /// Verifies source, width, and per-response fence limits retain literal
    /// Markdown instead of emitting incomplete or terminal-wrapped diagrams.
    #[test]
    fn bounded_mermaid_inputs_preserve_literal_fences() {
        let mut fence_count = 0;
        let overwide = render_agent_mermaid_fence(
            FencedCodeBlock {
                info: "mermaid",
                body: "flowchart LR\nA[Start] --> B[Done]",
            },
            4,
            &mut fence_count,
        );
        assert_eq!(overwide, FencedCodeBlockOutcome::PreserveLiteral);

        let oversized_source = "x".repeat(MAX_MERMAID_SOURCE_BYTES + 1);
        let oversized = render_agent_mermaid_fence(
            FencedCodeBlock {
                info: "mermaid",
                body: oversized_source.as_str(),
            },
            80,
            &mut fence_count,
        );
        assert_eq!(oversized, FencedCodeBlockOutcome::PreserveLiteral);

        let mut exhausted_count = MAX_MERMAID_FENCES;
        let exhausted = render_agent_mermaid_fence(
            FencedCodeBlock {
                info: "mermaid",
                body: "flowchart LR\nA --> B",
            },
            80,
            &mut exhausted_count,
        );
        assert_eq!(exhausted, FencedCodeBlockOutcome::PreserveLiteral);
    }

    /// Verifies non-Mermaid fences decline handling without consuming the
    /// bounded Mermaid-fence allowance for the response.
    #[test]
    fn ordinary_code_fences_do_not_consume_mermaid_limit() {
        let mut fence_count = 0;
        assert_eq!(
            render_agent_mermaid_fence(
                FencedCodeBlock {
                    info: "rust",
                    body: "fn main() {}",
                },
                80,
                &mut fence_count,
            ),
            FencedCodeBlockOutcome::NotHandled
        );
        assert_eq!(fence_count, 0);
    }
}
