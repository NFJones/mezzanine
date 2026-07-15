//! Runtime tests for agent presentation markdown behavior.

use super::*;

/// Verifies plain text `say` output does not receive markdown block framing.
///
/// Plain `say` output is ordinary assistant transcript text, so it should keep
/// the `mez> ` speaker prefix while avoiding the synthetic markdown divider
/// row that is reserved for `text/markdown` presentation blocks.
#[test]
fn runtime_agent_plain_say_does_not_render_markdown_divider() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-plain","method":"agent/shell/command","params":{"idempotency_key":"agent-plain-say","input":"render plain text"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let plain = "Plain say output without markdown framing.";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "plain say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: plain.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(
        styled_lines.iter().any(|line| line
            .text
            .contains("mez> Plain say output without markdown framing.")),
        "{styled_lines:?}"
    );
    let expected_divider = expected_markdown_block_divider_line(80);
    assert!(
        styled_lines
            .iter()
            .all(|line| line.text != expected_divider),
        "{styled_lines:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies markdown `say` output is rendered as presentation-only styling.
///
/// The display path should remove visual markdown delimiters and add terminal
/// style spans for readability, while copy mode must still return the raw
/// markdown authored by the model. This protects markdown as the first
/// content-type renderer without hard-coding future media types into copy mode.
#[test]
fn runtime_agent_markdown_say_renders_styled_presentation_and_copies_raw_markdown() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-markdown","method":"agent/shell/command","params":{"idempotency_key":"agent-markdown-say","input":"render markdown"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let markdown = "**Important** and <u>underlined</u>\n- first";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "markdown say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: markdown.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let assistant_line = styled_lines
        .iter()
        .find(|line| line.text.contains("mez> Important and underlined"))
        .unwrap();
    let assistant_index = styled_lines
        .iter()
        .position(|line| line.text == assistant_line.text)
        .unwrap();
    let expected_divider = expected_markdown_block_divider_line(80);
    assert!(
        assistant_index == 0 || styled_lines[assistant_index - 1].text != expected_divider,
        "{styled_lines:?}"
    );
    assert!(
        !assistant_line.text.contains("**") && !assistant_line.text.contains("<u>"),
        "{assistant_line:?}"
    );
    assert!(
        assistant_line
            .style_spans
            .iter()
            .any(|span| span.rendition.bold && span.start >= "▐ mez> ".chars().count()),
        "{assistant_line:?}"
    );
    assert!(
        assistant_line
            .style_spans
            .iter()
            .any(|span| span.rendition.underline && span.start >= "▐ mez> ".chars().count()),
        "{assistant_line:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("• first")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines.iter().all(|line| {
            line.text != expected_divider
                && !line.text.contains("mez> ---------")
                && !line.text.contains("mez> ─")
        }),
        "{styled_lines:?}"
    );

    let copy_mode = service.ensure_active_copy_mode("%1").unwrap();
    let scroll_top = copy_mode.scroll_top();
    let visible_lines = copy_mode.visible_lines();
    assert!(
        visible_lines.iter().all(|line| {
            line != &expected_divider
                && !line.contains("mez> ---------")
                && !line.contains("mez> ─")
        }),
        "{visible_lines:?}"
    );
    let first_line = visible_lines
        .iter()
        .position(|line| line.contains("mez> Important and underlined"))
        .map(|line| line + scroll_top)
        .unwrap();
    let second_line = visible_lines
        .iter()
        .position(|line| line.contains("• first"))
        .map(|line| line + scroll_top)
        .unwrap();
    let second_column = visible_lines[second_line.saturating_sub(scroll_top)]
        .chars()
        .count();
    copy_mode
        .select_range(
            CopyPosition {
                line: first_line,
                column: 0,
            },
            CopyPosition {
                line: second_line,
                column: second_column,
            },
        )
        .unwrap();

    assert_eq!(copy_mode.copy_selection().unwrap(), markdown);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies wrapped markdown presentation rows still copy as raw markdown.
///
/// Wide Markdown tables can render across several terminal rows in a narrow
/// pane. Copy mode should treat those extra rows as presentation-only so the
/// copied text remains a valid pipe table rather than including display wraps
/// or Unicode table borders.
#[test]
fn runtime_agent_markdown_copy_preserves_raw_table_when_rendered_rows_wrap() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(34, 12).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(34, 12).unwrap(), 120).unwrap(),
    );
    let markdown = "| Name | Description |\n| --- | --- |\n| alpha | this description is intentionally long enough to wrap in a narrow pane |";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("│"),
        "table should be rendered as terminal presentation: {pane_text}"
    );
    let pane_lines = pane_text.lines().collect::<Vec<_>>();
    let table_rows = pane_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains('│'))
        .collect::<Vec<_>>();
    assert!(
        table_rows.len() > 3,
        "table cells should wrap as rows: {pane_text}"
    );
    let separator_index = pane_lines
        .iter()
        .position(|line| line.contains('├'))
        .expect("table separator should be rendered");
    let last_table_row_index = table_rows
        .last()
        .map(|(index, _)| *index)
        .unwrap_or(separator_index);
    assert!(
        pane_lines[separator_index.saturating_add(1)..=last_table_row_index]
            .iter()
            .all(|line| line.contains('│')),
        "wrapped final table row should remain contiguous without blank gaps: {pane_text}"
    );
    assert!(
        table_rows
            .iter()
            .all(|(_, line)| line.matches('│').count() >= 3),
        "wrapped table rows should preserve column borders: {table_rows:?}"
    );
    let copy_mode = service.ensure_active_copy_mode("%1").unwrap();
    let visible_lines = copy_mode.visible_lines();
    let last_visible_index = visible_lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or_else(|| visible_lines.len().saturating_sub(1));
    let last_line = copy_mode.scroll_top().saturating_add(last_visible_index);
    let last_column = visible_lines
        .get(last_visible_index)
        .map(|line| line.chars().count())
        .unwrap_or_default();
    copy_mode
        .select_range(
            CopyPosition { line: 0, column: 0 },
            CopyPosition {
                line: last_line,
                column: last_column,
            },
        )
        .unwrap();

    let copied = copy_mode.copy_selection().unwrap();
    assert_eq!(copied, markdown);
    assert!(!copied.contains('│'), "{copied}");
}

/// Verifies rendered markdown blocks no longer copy a synthetic frame row.
///
/// Markdown `say` output now stays in the ordinary assistant transcript flow
/// without an extra divider line, so copy mode should preserve only the raw
/// markdown source instead of inventing a thematic-break row.
#[test]
fn runtime_agent_markdown_copy_omits_synthetic_frame_row() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let markdown = "# Heading";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains('─'), "{pane_text}");
    let copy_mode = service.ensure_active_copy_mode("%1").unwrap();
    copy_mode.scroll_to_top();
    let visible_lines = copy_mode.visible_lines();
    let heading_line_index = visible_lines
        .iter()
        .position(|line| line.contains("Heading"))
        .unwrap();
    let heading_line_width = visible_lines[heading_line_index].chars().count();
    let scroll_top = copy_mode.scroll_top();
    copy_mode
        .select_range(
            CopyPosition {
                line: scroll_top.saturating_add(heading_line_index),
                column: 0,
            },
            CopyPosition {
                line: scroll_top.saturating_add(heading_line_index),
                column: heading_line_width,
            },
        )
        .unwrap();

    let copied = copy_mode.copy_selection().unwrap();
    assert_eq!(copied, "# Heading");
}

/// Verifies partial markdown selections copy the rendered display slice.
///
/// Copy mode should only substitute raw markdown when the selection covers the
/// full rendered extent of a source line. Partial or continuation-only
/// selections must therefore match the visible wrapped text instead of
/// expanding back into the authored markdown source line.
#[test]
fn runtime_agent_markdown_partial_and_continuation_copy_matches_rendered_selection() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(24, 12).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 12).unwrap(), 120).unwrap(),
    );
    let markdown = "# heading text that wraps";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let copy_mode = service.ensure_active_copy_mode("%1").unwrap();
    copy_mode.scroll_to_top();
    let visible_lines = copy_mode.visible_lines();
    let heading_line_index = visible_lines
        .iter()
        .position(|line| line.contains("heading"))
        .unwrap();
    let continuation_line_index = visible_lines
        .iter()
        .enumerate()
        .skip(heading_line_index.saturating_add(1))
        .find(|(_, line)| !line.trim().is_empty())
        .map(|(index, _)| index)
        .unwrap();
    let heading_display_line = visible_lines[heading_line_index].clone();
    let heading_column_start = heading_display_line
        .split_once("heading")
        .map(|(prefix, _)| prefix.chars().count())
        .unwrap();
    let continuation_line_width = visible_lines[continuation_line_index].chars().count();
    let expected_heading_slice = heading_display_line
        .chars()
        .skip(heading_column_start)
        .collect::<String>();
    let expected_continuation_slice = "that wraps";
    let scroll_top = copy_mode.scroll_top();

    copy_mode
        .select_range(
            CopyPosition {
                line: scroll_top.saturating_add(heading_line_index),
                column: heading_column_start,
            },
            CopyPosition {
                line: scroll_top.saturating_add(continuation_line_index),
                column: continuation_line_width,
            },
        )
        .unwrap();
    assert_eq!(
        copy_mode.copy_selection().unwrap(),
        format!("{expected_heading_slice}\n{expected_continuation_slice}")
    );

    copy_mode
        .select_range(
            CopyPosition {
                line: scroll_top.saturating_add(continuation_line_index),
                column: 0,
            },
            CopyPosition {
                line: scroll_top.saturating_add(continuation_line_index),
                column: continuation_line_width,
            },
        )
        .unwrap();
    assert_eq!(
        copy_mode.copy_selection().unwrap(),
        expected_continuation_slice
    );
}

/// Verifies CommonMark block and inline constructs are rendered from a parser
/// instead of the older delimiter scanner.
///
/// This covers features that were not supported by the line-oriented renderer:
/// ordered lists, task markers, block quotes, links, tables, fenced code blocks,
/// emphasis, and strikethrough. The test checks display text and terminal
/// styles so regressions point at both parsing and presentation failures.
#[test]
fn runtime_agent_commonmark_say_renders_rich_markdown_features() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(96, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(96, 40).unwrap(), 120).unwrap(),
    );
    let markdown = "# Heading\n\n> quoted **bold** text\n\n1. first\n2. second\n\n- [x] done\n\n`code` and *em*\n\n[link](https://example.com)\n\n| Name | Count |\n|:--|--:|\n| alpha | 2 |\n\n```rust\nfn main() {}\n```\n\n~~gone~~\n\nparagraph\n## Later";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let heading = styled_lines
        .iter()
        .find(|line| line.text.trim_end().ends_with("Heading"))
        .unwrap();
    let heading_index = styled_lines
        .iter()
        .position(|line| line.text == heading.text)
        .unwrap();
    assert!(
        styled_lines
            .iter()
            .all(|line| line.text != expected_markdown_block_divider_line(96)),
        "{styled_lines:?}"
    );
    assert!(!heading.text.contains('#'), "{heading:?}");
    assert!(heading.style_spans.iter().any(|span| {
        span.rendition.bold
            && span.rendition.underline
            && span.rendition.foreground
                == Some(service.ui_theme.colors.agent_transcript_user.foreground)
            && span.rendition.background.is_none()
            && span.start >= "▐ mez> ".chars().count()
    }));

    let quote = styled_lines
        .iter()
        .find(|line| line.text.contains("> quoted bold text"))
        .unwrap();
    assert!(quote.style_spans.iter().any(|span| {
        span.rendition.dim
            && span.rendition.foreground
                == Some(service.ui_theme.colors.agent_transcript_status.foreground)
    }));
    assert!(quote.style_spans.iter().any(|span| span.rendition.bold));
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("1. first")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("2. second")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("[x] done")),
        "{styled_lines:?}"
    );

    let inline = styled_lines
        .iter()
        .find(|line| line.text.contains("code and em"))
        .unwrap();
    assert!(
        inline.style_spans.iter().any(|span| {
            !span.rendition.inverse
                && span.rendition.background.is_none()
                && span.rendition.foreground == Some(EXPECTED_MARKDOWN_INLINE_CODE_FOREGROUND)
        }),
        "{inline:?}"
    );
    assert!(inline.style_spans.iter().any(|span| span.rendition.italic));

    let link = styled_lines
        .iter()
        .find(|line| line.text.contains("link (https://example.com)"))
        .unwrap();
    assert!(link.style_spans.iter().any(|span| span.rendition.bold));
    assert!(link.style_spans.iter().any(|span| span.rendition.underline));
    assert!(link.style_spans.iter().any(|span| {
        !span.rendition.inverse
            && span.rendition.background.is_none()
            && span.rendition.foreground
                == Some(service.ui_theme.colors.agent_transcript_command.foreground)
    }));
    assert!(link.style_spans.iter().any(|span| span.rendition.dim));

    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("│ Name") && line.text.contains("Count │")),
        "{styled_lines:?}"
    );
    let table_header = styled_lines
        .iter()
        .find(|line| line.text.contains("│ Name") && line.text.contains("Count │"))
        .unwrap();
    assert!(table_header.style_spans.iter().any(|span| {
        span.rendition.bold
            && span.rendition.foreground
                == Some(service.ui_theme.colors.agent_transcript_user.foreground)
            && span.rendition.background.is_none()
    }));
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("├") && line.text.contains("┼")),
        "{styled_lines:?}"
    );
    let table_separator = styled_lines
        .iter()
        .find(|line| line.text.contains("├") && line.text.contains("┼"))
        .unwrap();
    assert!(table_separator.style_spans.iter().any(|span| {
        span.rendition.dim
            && span.rendition.foreground
                == Some(service.ui_theme.colors.agent_transcript_status.foreground)
    }));
    let table_row = styled_lines
        .iter()
        .find(|line| line.text.contains("│ alpha") && line.text.contains("2 │"))
        .unwrap();
    assert!(
        table_row.style_spans.iter().any(|span| {
            span.rendition.foreground == Some(EXPECTED_MARKDOWN_TABLE_ALTERNATE_ROW_FOREGROUND)
                && span.rendition.background.is_none()
        }),
        "{table_row:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("fn main() {}")
                && line.style_spans.iter().any(|span| {
                    !span.rendition.inverse
                        && !span.rendition.dim
                        && span.rendition.background.is_none()
                        && span.rendition.foreground
                            == Some(EXPECTED_MARKDOWN_INLINE_CODE_FOREGROUND)
                })),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines.iter().any(|line| line.text.contains("gone")
            && line
                .style_spans
                .iter()
                .any(|span| span.rendition.strikethrough)),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .skip(heading_index + 1)
            .all(|line| line.text != expected_markdown_block_divider_line(96)),
        "{styled_lines:?}"
    );
    let later_heading_index = styled_lines
        .iter()
        .position(|line| line.text.contains("Later"))
        .unwrap();
    assert!(
        later_heading_index > 0 && styled_lines[later_heading_index - 1].text.trim_end() == "▐",
        "{styled_lines:?}"
    );
}

/// Verifies markdown neutral accents switch to dark greys on light themes.
///
/// Inline code and table alternation are foreground-only presentation accents,
/// so they must derive their lightness from the active theme surface instead of
/// assuming a dark terminal background.
#[test]
fn runtime_agent_markdown_uses_dark_neutral_accents_on_light_theme() {
    let mut service = test_runtime_service();
    service.ui_theme = mez_mux::theme::resolve_ui_theme(
        "catppuccin_latte",
        mez_mux::theme::builtin_ui_theme_definition("catppuccin_latte").unwrap(),
    )
    .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 16).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "`code`\n\n| Name | Count |\n|:--|--:|\n| alpha | 2 |",
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let inline = styled_lines
        .iter()
        .find(|line| line.text.contains("code"))
        .unwrap();
    assert!(
        inline.style_spans.iter().any(|span| {
            span.rendition.foreground == Some(TerminalColor::Rgb(0x42, 0x42, 0x42))
                && span.rendition.background.is_none()
        }),
        "{inline:?}"
    );
    let table_row = styled_lines
        .iter()
        .find(|line| line.text.contains("│ alpha") && line.text.contains("2 │"))
        .unwrap();
    assert!(
        table_row.style_spans.iter().any(|span| {
            span.rendition.foreground == Some(TerminalColor::Rgb(0x5a, 0x5a, 0x5a))
                && span.rendition.background.is_none()
        }),
        "{table_row:?}"
    );
}

/// Verifies markdown presentation wraps at the smaller of pane width or 120
/// cells and indents continuation rows under the rendered list marker.
///
/// Wide panes should not produce unreadably long markdown transcript rows, and
/// continuation rows should retain enough structural indentation to make lists
/// readable after wrapping.
#[test]
fn runtime_agent_markdown_wraps_to_120_cells_and_indents_continuations() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 40).unwrap(), 120).unwrap(),
    );
    let markdown = format!("- {}", "alphabet ".repeat(40));

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            &markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(
        styled_lines
            .iter()
            .all(|line| line.text != expected_markdown_block_divider_line(120)),
        "{styled_lines:?}"
    );
    let continuation_prefix = format!("▐ {}", " ".repeat("mez> • ".chars().count()));
    let wrapped_lines = styled_lines
        .iter()
        .filter(|line| {
            line.text.contains("mez> • alphabet") || line.text.starts_with(&continuation_prefix)
        })
        .collect::<Vec<_>>();

    assert!(wrapped_lines.len() > 1, "{styled_lines:?}");
    assert!(
        wrapped_lines
            .iter()
            .all(|line| line.text.chars().count() <= 120),
        "{wrapped_lines:?}"
    );
    assert!(
        wrapped_lines
            .iter()
            .skip(1)
            .all(|line| line.text.starts_with(&continuation_prefix)),
        "{wrapped_lines:?}"
    );
}

/// Verifies markdown thematic breaks expand to the capped prose width.
///
/// A source `***` line should render as a subdued box-drawing divider that
/// fills the same width cap used for prose markdown rows instead of remaining a
/// short fixed run of glyphs.
#[test]
fn runtime_agent_markdown_thematic_break_expands_to_capped_divider_width() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 40).unwrap(), 120).unwrap(),
    );
    let markdown = "***";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let expected = format!(
        "▐ mez> {}",
        EXPECTED_MARKDOWN_BLOCK_DIVIDER_GLYPH
            .to_string()
            .repeat(120usize.saturating_sub("▐ mez> ".chars().count()))
    );

    assert!(
        styled_lines.iter().any(|line| line.text == expected),
        "{styled_lines:?}"
    );
}

/// Verifies markdown tables keep their row layout on wide terminals.
///
/// Prose markdown is capped at 120 cells for readability, but table rows need
/// to remain horizontally inspectable until they exceed the actual pane width.
#[test]
fn runtime_agent_markdown_tables_wrap_only_at_terminal_width() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 40).unwrap(), 120).unwrap(),
    );
    let first_cell = "alpha".repeat(18);
    let second_cell = "beta".repeat(8);
    let markdown = format!("| Long | Other |\n| --- | --- |\n| {first_cell} | {second_cell} |");

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            &markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let data_row = styled_lines
        .iter()
        .find(|line| line.text.contains(&first_cell) && line.text.contains(&second_cell))
        .unwrap();

    assert!(
        data_row.text.chars().count() > 120,
        "table row should exceed the prose cap: {data_row:?}"
    );
    assert!(
        data_row.text.chars().count() <= 200,
        "table row should still fit the terminal width: {data_row:?}"
    );
    assert!(
        data_row.text.contains("│") && data_row.text.contains(&second_cell),
        "{data_row:?}"
    );
}

/// Verifies box-drawing text alone does not opt into table-row wrapping.
///
/// Markdown table rows carry structural metadata from the parser. A paragraph
/// or code-like line that happens to begin with a Unicode table border glyph
/// must still use the prose presentation width instead of terminal-width table
/// behavior.
#[test]
fn runtime_agent_markdown_box_drawing_paragraph_uses_prose_width() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 40).unwrap(), 120).unwrap(),
    );
    let markdown = format!("│ {}", "not-a-table ".repeat(30));

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            &markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let paragraph_lines = styled_lines
        .iter()
        .filter(|line| line.text.contains("not-a-table"))
        .collect::<Vec<_>>();

    assert!(paragraph_lines.len() > 1, "{styled_lines:?}");
    assert!(
        paragraph_lines
            .iter()
            .all(|line| line.text.chars().count() <= 120),
        "{paragraph_lines:?}"
    );
}

/// Verifies markdown display bodies from agent slash commands use the shared
/// command-output pager instead of being appended as ordinary pane transcript.
///
/// `/status` emits a markdown table rather than model-authored prose. It should
/// open the same navigable display overlay used by `:` command output, with
/// markdown heading syntax stripped and tables rendered for terminal reading.
#[test]
fn runtime_agent_slash_markdown_display_opens_command_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 60).unwrap(), 120).unwrap(),
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/status\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.full_redraw_required);
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("/status should open the command display overlay");
    let heading_index = overlay
        .lines
        .iter()
        .position(|line| line.contains("Agent Status"))
        .unwrap();
    let heading_line = &overlay.lines[heading_index];
    assert!(!heading_line.contains("##"), "{heading_line:?}");
    assert!(!heading_line.contains("mez>"), "{heading_line:?}");
    assert_eq!(heading_line, "Agent Status");
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("│ Field") && line.contains("Value")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .lines
            .iter()
            .all(|line| !line.contains("Quota Usage")),
        "{overlay:?}"
    );
}
