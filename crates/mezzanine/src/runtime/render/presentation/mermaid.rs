//! Bounded, agent-only Mermaid fence presentation.
//!
//! This module owns product policy for replacing a completed agent Markdown
//! `mermaid` fence with terminal-native Unicode diagram rows. The mux remains
//! renderer-neutral: malformed, unsupported, over-limit, or overwide diagrams
//! decline replacement so its literal fenced-code fallback retains the source.

use super::{
    GraphicRendition, RichTextLine, RichTextLineKind, TerminalColor, TerminalStyleSpan, UiTheme,
    UnicodeWidthStr,
};
use merman::ascii::{AsciiRenderOptions, HeadlessAsciiRenderer};
use merman_ascii::{AsciiColorMode, AsciiColorRole, AsciiColorTheme, AsciiRgb};
use mez_mux::copy::COPY_SKIP_LINE;
use mez_mux::render::{FencedCodeBlock, FencedCodeBlockOutcome, push_or_extend_style_span};

const MAX_MERMAID_SOURCE_BYTES: usize = 16 * 1024;
const MAX_MERMAID_FENCES: usize = 8;
const MAX_MERMAID_RENDERED_ROWS: usize = 96;
const MAX_MERMAID_RENDERED_CELLS: usize = 16 * 1024;
const MAX_MERMAID_GRID_CELLS: usize = 16 * 1024;

/// Attempts a bounded terminal-native replacement for one agent Markdown fence.
pub(super) fn render_agent_mermaid_fence(
    fence: FencedCodeBlock<'_>,
    available_width: usize,
    ui_theme: &UiTheme,
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

    let options = compact_mermaid_options(ui_theme);
    let renderer = HeadlessAsciiRenderer::new()
        .with_strict_parsing()
        .with_ascii_options(options);
    let diagram_source = fence.body.trim_end_matches(['\r', '\n']);
    let Ok(Some(rendered)) = renderer.render_ascii_sync(diagram_source) else {
        return FencedCodeBlockOutcome::PreserveLiteral;
    };
    let Some(rows) = decode_styled_mermaid_output(&rendered) else {
        return FencedCodeBlockOutcome::PreserveLiteral;
    };
    let cells = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.display.as_str()))
        .sum::<usize>();
    if rows.is_empty()
        || rows.len() > MAX_MERMAID_RENDERED_ROWS
        || cells > MAX_MERMAID_RENDERED_CELLS
        || rows
            .iter()
            .any(|row| UnicodeWidthStr::width(row.display.as_str()) > available_width)
    {
        return FencedCodeBlockOutcome::PreserveLiteral;
    }

    let raw_fence = format!("```{}\n{}```", fence.info, fence.body);
    FencedCodeBlockOutcome::Rendered(
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| RichTextLine {
                display: row.display,
                style_spans: row.style_spans,
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

/// Builds the compact Unicode and active-theme rendering policy.
fn compact_mermaid_options(ui_theme: &UiTheme) -> AsciiRenderOptions {
    let mut options = AsciiRenderOptions::unicode()
        .with_color_mode(AsciiColorMode::TrueColor)
        .with_color_theme(mermaid_color_theme(ui_theme));
    options.box_border_padding = 0;
    options.graph_padding_x = 2;
    options.graph_padding_y = 1;
    options.sequence_participant_spacing = 3;
    options.sequence_message_spacing = 1;
    options.sequence_self_message_width = 2;
    options.sequence_mirror_actors = false;
    options.max_grid_cells = MAX_MERMAID_GRID_CELLS;
    options
}

/// Projects semantic Mez foreground colors onto Mermaid renderer roles.
fn mermaid_color_theme(ui_theme: &UiTheme) -> AsciiColorTheme {
    let text = terminal_color_to_ascii_rgb(ui_theme.colors.syntax_plain.foreground);
    let muted = terminal_color_to_ascii_rgb(ui_theme.colors.agent_transcript_status.foreground);
    let structural =
        terminal_color_to_ascii_rgb(super::text::markdown_structural_foreground(ui_theme));
    let accent = terminal_color_to_ascii_rgb(ui_theme.colors.agent_transcript_user.foreground);
    let series = [
        ui_theme.colors.syntax_keyword.foreground,
        ui_theme.colors.syntax_string.foreground,
        ui_theme.colors.syntax_number.foreground,
        ui_theme.colors.syntax_function.foreground,
        ui_theme.colors.syntax_type.foreground,
        ui_theme.colors.syntax_operator.foreground,
        ui_theme.colors.agent_transcript_user.foreground,
        ui_theme.colors.agent_transcript_error.foreground,
    ];
    let mut theme = AsciiColorTheme::default_dark()
        .with_role(AsciiColorRole::Text, text)
        .with_role(AsciiColorRole::MutedText, muted)
        .with_role(AsciiColorRole::NodeBorder, structural)
        .with_role(AsciiColorRole::GroupBorder, structural)
        .with_role(AsciiColorRole::EdgeLine, muted)
        .with_role(AsciiColorRole::EdgeArrow, accent)
        .with_role(AsciiColorRole::EdgeLabel, text)
        .with_role(AsciiColorRole::Junction, muted)
        .with_role(AsciiColorRole::SequenceLifeline, muted)
        .with_role(AsciiColorRole::SequenceActivation, accent)
        .with_role(AsciiColorRole::SequenceFrame, structural)
        .with_role(AsciiColorRole::ChartAxis, muted);
    for (index, color) in series.into_iter().enumerate() {
        theme = theme.with_role(
            AsciiColorRole::ChartSeries(index),
            terminal_color_to_ascii_rgb(color),
        );
    }
    theme
}

/// Converts terminal RGB or indexed palette colors into deterministic RGB.
fn terminal_color_to_ascii_rgb(color: TerminalColor) -> AsciiRgb {
    let (red, green, blue) = match color {
        TerminalColor::Rgb(red, green, blue) => (red, green, blue),
        TerminalColor::Indexed(index) => indexed_terminal_color_rgb(index),
    };
    AsciiRgb::new(red, green, blue)
}

/// Resolves one xterm indexed color into its canonical RGB approximation.
fn indexed_terminal_color_rgb(index: u8) -> (u8, u8, u8) {
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
    if index < 16 {
        return ANSI_16[usize::from(index)];
    }
    if index < 232 {
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let offset = index - 16;
        return (
            CUBE[usize::from(offset / 36)],
            CUBE[usize::from((offset % 36) / 6)],
            CUBE[usize::from(offset % 6)],
        );
    }
    let level = 8u8.saturating_add(index.saturating_sub(232).saturating_mul(10));
    (level, level, level)
}

/// Control-free diagram row plus native terminal style spans.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedMermaidRow {
    display: String,
    style_spans: Vec<TerminalStyleSpan>,
}

/// Strictly decodes renderer-owned RGB foreground SGR into native spans.
fn decode_styled_mermaid_output(rendered: &str) -> Option<Vec<DecodedMermaidRow>> {
    rendered
        .split_terminator('\n')
        .map(decode_styled_mermaid_row)
        .collect()
}

/// Decodes one row, rejecting unsupported controls or unterminated styles.
fn decode_styled_mermaid_row(row: &str) -> Option<DecodedMermaidRow> {
    let mut display = String::new();
    let mut style_spans = Vec::new();
    let mut active_color = None;
    let mut run = String::new();
    let mut offset = 0usize;
    while offset < row.len() {
        let remaining = &row[offset..];
        if remaining.starts_with('\u{1b}') {
            flush_mermaid_text_run(&mut display, &mut style_spans, &mut run, active_color);
            if let Some(rest) = remaining.strip_prefix("\u{1b}[0m") {
                active_color = None;
                offset = row.len().saturating_sub(rest.len());
                continue;
            }
            let rest = remaining.strip_prefix("\u{1b}[38;2;")?;
            let end = rest.find('m')?;
            let mut channels = rest[..end].split(';');
            let red = channels.next()?.parse::<u8>().ok()?;
            let green = channels.next()?.parse::<u8>().ok()?;
            let blue = channels.next()?.parse::<u8>().ok()?;
            if channels.next().is_some() {
                return None;
            }
            active_color = Some(TerminalColor::Rgb(red, green, blue));
            offset = row
                .len()
                .saturating_sub(rest[end.saturating_add(1)..].len());
            continue;
        }
        let character = remaining.chars().next()?;
        if character.is_control() {
            return None;
        }
        run.push(character);
        offset = offset.saturating_add(character.len_utf8());
    }
    flush_mermaid_text_run(&mut display, &mut style_spans, &mut run, active_color);
    if active_color.is_some() {
        return None;
    }
    Some(DecodedMermaidRow {
        display,
        style_spans,
    })
}

/// Appends one decoded text run and its foreground-only terminal span.
fn flush_mermaid_text_run(
    display: &mut String,
    style_spans: &mut Vec<TerminalStyleSpan>,
    run: &mut String,
    color: Option<TerminalColor>,
) {
    if run.is_empty() {
        return;
    }
    let start = UnicodeWidthStr::width(display.as_str());
    let length = UnicodeWidthStr::width(run.as_str());
    display.push_str(run);
    if let Some(foreground) = color {
        push_or_extend_style_span(
            style_spans,
            TerminalStyleSpan {
                start,
                length,
                rendition: GraphicRendition {
                    foreground: Some(foreground),
                    ..GraphicRendition::default()
                },
            },
        );
    }
    run.clear();
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
            &mez_mux::theme::deepforest_ui_theme(),
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
        assert!(
            lines.iter().any(|line| !line.style_spans.is_empty()),
            "{lines:?}"
        );
        assert!(lines.len() <= 3, "expected compact flowchart: {lines:?}");
        assert!(lines.iter().flat_map(|line| &line.style_spans).all(|span| {
            span.rendition.foreground.is_some()
                && span.rendition.background.is_none()
                && !span.rendition.bold
                && !span.rendition.inverse
        }));
    }

    /// Verifies the product-owned Mermaid preset stays at the compact geometry
    /// selected for terminal presentation while retaining resource bounds.
    #[test]
    fn compact_mermaid_options_match_product_policy() {
        let options = compact_mermaid_options(&mez_mux::theme::deepforest_ui_theme());

        assert_eq!(options.box_border_padding, 0);
        assert_eq!(options.graph_padding_x, 2);
        assert_eq!(options.graph_padding_y, 1);
        assert_eq!(options.sequence_participant_spacing, 3);
        assert_eq!(options.sequence_message_spacing, 1);
        assert_eq!(options.sequence_self_message_width, 2);
        assert!(!options.sequence_mirror_actors);
        assert_eq!(options.max_grid_cells, MAX_MERMAID_GRID_CELLS);
        assert_eq!(options.color_mode, AsciiColorMode::TrueColor);
    }

    /// Verifies active themes preserve diagram geometry while changing only
    /// native foreground spans projected from semantic Mez color slots.
    #[test]
    fn rendered_mermaid_rows_follow_active_theme_foregrounds() {
        let first_theme = mez_mux::theme::deepforest_ui_theme();
        let mut definition = mez_mux::theme::builtin_ui_theme_definition("deepforest").unwrap();
        for (slot, value) in [
            ("syntax_plain_fg", "#010203"),
            ("agent_transcript_status_fg", "#040506"),
            ("agent_transcript_user_fg", "#070809"),
        ] {
            definition
                .colors
                .insert(slot.to_string(), value.to_string());
        }
        let second_theme = mez_mux::theme::resolve_ui_theme("mermaid-theme-test", definition)
            .expect("custom Mermaid theme should resolve");
        let render = |theme: &UiTheme| {
            let mut count = 0;
            let FencedCodeBlockOutcome::Rendered(lines) = render_agent_mermaid_fence(
                FencedCodeBlock {
                    info: "mermaid",
                    body: "flowchart LR\nA[Start] --> B[Done]",
                },
                80,
                theme,
                &mut count,
            ) else {
                panic!("expected themed Mermaid rows");
            };
            lines
        };

        let first = render(&first_theme);
        let second = render(&second_theme);
        assert_eq!(
            first.iter().map(|line| &line.display).collect::<Vec<_>>(),
            second.iter().map(|line| &line.display).collect::<Vec<_>>()
        );
        assert_ne!(
            first
                .iter()
                .map(|line| &line.style_spans)
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|line| &line.style_spans)
                .collect::<Vec<_>>()
        );
        let foregrounds = second
            .iter()
            .flat_map(|line| &line.style_spans)
            .filter_map(|span| span.rendition.foreground)
            .collect::<Vec<_>>();
        assert!(foregrounds.contains(&TerminalColor::Rgb(1, 2, 3)));
        assert!(foregrounds.contains(&TerminalColor::Rgb(4, 5, 6)));
        assert!(foregrounds.contains(&TerminalColor::Rgb(7, 8, 9)));
        assert!(
            second
                .iter()
                .flat_map(|line| &line.style_spans)
                .all(|span| {
                    span.rendition.background.is_none()
                        && !span.rendition.bold
                        && !span.rendition.inverse
                })
        );
    }

    /// Verifies strict renderer-control decoding accepts only RGB foreground
    /// SGR/reset output and fails closed for malformed or unexpected controls.
    #[test]
    fn styled_mermaid_decoder_rejects_unexpected_controls() {
        let decoded = decode_styled_mermaid_row("\u{1b}[38;2;1;2;3mA界\u{1b}[0m!")
            .expect("known RGB renderer output should decode");
        assert_eq!(decoded.display, "A界!");
        assert_eq!(decoded.style_spans.len(), 1);
        assert_eq!(decoded.style_spans[0].start, 0);
        assert_eq!(decoded.style_spans[0].length, 3);
        assert_eq!(
            decoded.style_spans[0].rendition.foreground,
            Some(TerminalColor::Rgb(1, 2, 3))
        );

        for invalid in [
            "\u{1b}[31mA\u{1b}[0m",
            "\u{1b}[38;2;1;2mA\u{1b}[0m",
            "\u{1b}[38;2;1;2;3mA",
            "\u{1b}]8;;https://example.com\u{7}A",
            "A\tB",
        ] {
            assert!(decode_styled_mermaid_row(invalid).is_none(), "{invalid:?}");
        }
    }

    /// Verifies all Mermaid families supported by the dependency retain
    /// bounded, control-free, natively styled product presentation.
    #[test]
    fn supported_mermaid_families_render_with_native_styles() {
        for source in [
            "flowchart TD\nA[Start] -->|label| B[Done]",
            "sequenceDiagram\nparticipant A\nparticipant B\nA->>B: Hello\nA->>A: Retry",
            "classDiagram\nclass Animal",
            "erDiagram\nCUSTOMER",
            "xychart\ntitle \"Sales\"\nx-axis [Jan, Feb]\ny-axis 0 --> 10\nbar [2, 8]",
        ] {
            let mut count = 0;
            let outcome = render_agent_mermaid_fence(
                FencedCodeBlock {
                    info: "mermaid",
                    body: source,
                },
                200,
                &mez_mux::theme::deepforest_ui_theme(),
                &mut count,
            );
            let FencedCodeBlockOutcome::Rendered(lines) = outcome else {
                panic!("expected supported Mermaid family to render: {source:?} {outcome:?}");
            };
            assert!(!lines.is_empty(), "{source:?}");
            assert!(lines.iter().all(|line| {
                line.kind == RichTextLineKind::MarkdownDiagram
                    && !line.display.chars().any(char::is_control)
                    && UnicodeWidthStr::width(line.display.as_str()) <= 200
            }));
            assert!(
                lines.iter().any(|line| !line.style_spans.is_empty()),
                "{source:?} {lines:?}"
            );
        }
    }

    /// Verifies parser-captured Mermaid bodies retain diagram rendering at the
    /// agent transcript width after their terminal newline is normalized.
    #[test]
    fn parser_captured_flowchart_renders_at_agent_body_width() {
        let mut mermaid_fence_count = 0;
        let outcome = render_agent_mermaid_fence(
            FencedCodeBlock {
                info: "mermaid",
                body: "flowchart LR\nA[Start] --> B[Done]\n",
            },
            74,
            &mez_mux::theme::deepforest_ui_theme(),
            &mut mermaid_fence_count,
        );

        assert!(
            matches!(outcome, FencedCodeBlockOutcome::Rendered(_)),
            "{outcome:?}"
        );
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
                &mez_mux::theme::deepforest_ui_theme(),
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
            &mez_mux::theme::deepforest_ui_theme(),
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
            &mez_mux::theme::deepforest_ui_theme(),
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
            &mez_mux::theme::deepforest_ui_theme(),
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
                &mez_mux::theme::deepforest_ui_theme(),
                &mut fence_count,
            ),
            FencedCodeBlockOutcome::NotHandled
        );
        assert_eq!(fence_count, 0);
    }
}
