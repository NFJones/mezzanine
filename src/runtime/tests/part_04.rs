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
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
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

/// Verifies plain `mez>` output wraps under the assistant indicator.
///
/// Markdown output already has element-aware continuation indentation. Plain
/// assistant text should use the same transcript geometry instead of relying
/// on terminal soft wrapping, whose continuation starts too far left.
#[test]
fn runtime_agent_plain_say_wraps_under_agent_indicator() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "alpha beta gamma delta epsilon",
            crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("▐ mez> alpha beta gamma"),
        "{pane_text}"
    );
    assert!(pane_text.contains("▐      delta epsilon"), "{pane_text}");
}

/// Verifies pasted provider diagnostics remain normal prompt text.
///
/// Users often paste the previous terminal failure back into the agent shell for
/// diagnosis. That text can contain JSON error payloads, wrapped words, and the
/// provider_error marker, but it is still user-authored prompt content. The
/// runtime should render it through the agent transcript presentation path
/// without surfacing a secondary terminal presentation failure.
#[test]
fn runtime_agent_user_prompt_renders_pasted_provider_error_without_terminal_failure() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
    );
    let prompt = "provider_error: InvalidState: OpenAI Responses-compatible provider `lmstudio` is not authenticated\nInvalidState: terminal step failed: {\"code\":-32004,\n\"data\":{\"mezzanine_code\":\"invalid_state\"},\"message\":\"agent terminal presentation feed panicked while appending styled agent\n lines\"}";

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", prompt)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("provider_error: InvalidState"), "{pane_text}");
    assert!(pane_text.contains("terminal step failed"), "{pane_text}");
}

/// Verifies model-authored diff output uses the diff content renderer.
///
/// Diffs are a structured text media type rather than prose. The runtime should
/// parse the unified diff, omit raw diff scaffolding from the visible pane log,
/// and apply file-aware token colors to changed source lines when the file path
/// identifies a supported syntax.
#[test]
fn runtime_agent_diff_say_renders_file_aware_syntax_spans() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-diff","method":"agent/shell/command","params":{"idempotency_key":"agent-diff-say","input":"show diff"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let diff = "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-fn old() {}\n+fn new() {}";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "diff say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-diff".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: diff.to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE.to_string(),
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
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        pane_text.contains("• Edited src/main.rs (+1 -1)"),
        "{pane_text}"
    );
    assert!(pane_text.contains("       1 +fn new() {}"), "{pane_text}");
    assert!(!pane_text.contains("diff -- update file"), "{pane_text}");
    let addition_line = styled_lines
        .iter()
        .find(|line| line.text.contains("       1 +fn new() {}"))
        .unwrap();
    let syntax_start = "▐ ".chars().count() + 10;
    assert!(
        addition_line
            .style_spans
            .iter()
            .any(|span| span.start >= syntax_start && span.rendition.foreground.is_some()),
        "{addition_line:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies display-only `say` actions can show raw Mezzanine patch examples.
///
/// When a user asks to see a patch, the patch text is ordinary assistant
/// output and must not be parsed as markdown structure, executed as a semantic
/// mutation, or collapsed into a no-output placeholder.
#[test]
fn runtime_agent_markdown_say_displays_raw_mez_patch_examples() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(96, 24).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(96, 24).unwrap(), 120).unwrap(),
    );
    let patch = "*** Begin Patch\n*** Update File: docs/example.md\n@@\n-old\n+new\n*** End Patch";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            patch,
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("mez> *** Begin Patch"), "{pane_text}");
    assert!(
        pane_text.contains("     *** Update File: docs/example.md"),
        "{pane_text}"
    );
    assert!(pane_text.contains("     +new"), "{pane_text}");
    assert!(!pane_text.contains("[mez: no output]"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
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
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
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
        span.rendition.bold && span.rendition.underline && span.start >= "▐ mez> ".chars().count()
    }));

    let quote = styled_lines
        .iter()
        .find(|line| line.text.contains("> quoted bold text"))
        .unwrap();
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
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("├") && line.text.contains("┼")),
        "{styled_lines:?}"
    );
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
                && line.style_spans.iter().all(|span| !span.rendition.dim)),
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

/// Verifies list items keep their marker and first content words on the same
/// rendered row instead of flushing a marker-only line before the paragraph
/// text arrives. CommonMark emits `Paragraph` inside list items, so the
/// renderer must not treat the freshly written list prefix as a completed
/// block.
#[test]
fn runtime_agent_markdown_lists_keep_content_on_marker_row() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(64, 20).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(64, 20).unwrap(), 120).unwrap(),
    );
    let markdown = "1. first numbered item\n2. second numbered item\n\n- bullet item";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let pane_text = pane_lines.join("\n");

    assert!(pane_text.contains("▐ mez> 1. first numbered item"), "{pane_text}");
    assert!(pane_text.contains("▐      2. second numbered item"), "{pane_text}");
    assert!(pane_text.contains("▐      • bullet item"), "{pane_text}");
    assert!(
        !pane_lines
            .iter()
            .any(|line| line.trim_end() == "▐ mez> 1." || line.trim_end() == "▐      2." || line.trim_end() == "▐      •"),
        "{pane_text}"
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
    service.ui_theme = crate::terminal::resolve_ui_theme(
        "catppuccin_latte",
        crate::terminal::builtin_ui_theme_definition("catppuccin_latte").unwrap(),
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
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
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
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
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
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
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
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
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

/// Verifies that a provider response containing only a final completion marker
/// still leaves an explicit pane-buffer status. This prevents the default
/// non-verbose view from looking silent when the model forgets to include a
/// user-facing `say` action.
#[test]
fn runtime_agent_complete_without_say_reports_visible_completion_status() {
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-complete","input":"finish silently"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap complete response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "the task is complete".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "complete-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Complete,
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: completed without a user-facing response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("thinking: the task is complete"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that model-authored thinking text is not rendered a second time
/// when another action in the same response already presents the same text as
/// a `say` action. Models commonly emit a short `say` plus a matching
/// batch-level `thinking:` rationale; the pane should show the user-visible
/// answer once rather than adding a grey duplicate.
#[test]
fn runtime_agent_suppresses_batch_rationale_that_duplicates_say_text() {
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-duplicate-thinking","input":"respond once"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let visible = "I will handle the next step.";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say and complete response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: format!("thinking: {visible}"),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "say-1".to_string(),
                        rationale: String::new(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: visible.to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    crate::agent::AgentAction {
                        id: "complete-1".to_string(),
                        rationale: String::new(),
                        payload: crate::agent::AgentActionPayload::Complete,
                    },
                ],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(pane_text.matches(visible).count(), 1, "{pane_text}");
    assert!(
        pane_text.contains(&format!("mez> {visible}")),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that normal mode renders shell commands selected by the agent into
/// the same pane terminal buffer before they are sent to the PTY. Users should
/// be able to monitor the exact command stream without enabling raw shell
/// output or wrapper diagnostics.
#[test]
fn runtime_agent_shell_command_is_presented_before_pty_dispatch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-command","input":"run a harmless command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "check shell access".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Check shell access".to_string(),
                        command: "if true; then echo \"ok\"; fi".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("mez> Check shell access"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent: Check shell access"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("thinking: check shell access"),
        "{pane_text}"
    );
    assert_eq!(
        pane_text.matches("$ if true; then echo \"ok\"; fi").count(),
        1
    );
    let command_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("$ if true; then echo \"ok\"; fi"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    assert!(command_line.style_spans.iter().any(|span| {
        span.start >= 2
            && span.rendition.foreground.is_some_and(|foreground| {
                foreground != theme.colors.agent_transcript_command.foreground
            })
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies hidden model shell commands expose a single live latest-output row.
///
/// Normal logging hides raw PTY output, but users still need lightweight
/// progress for long-running commands. The latest cleaned stdout/stderr line
/// should replace the previous transient row and disappear when the next durable
/// agent transcript line is written.
#[test]
fn runtime_hidden_model_shell_command_shows_transient_latest_output_line() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .start_agent_prompt_turn("%1", "run a command")
        .unwrap();
    assert_eq!(start.state, AgentTurnState::Running);
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "run a command".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Run a command".to_string(),
            command: "sleep 1".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions.insert(
        "turn-1".to_string(),
        crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                available_mcp_tools: Vec::new(),
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    content: "run a command".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell".to_string(),
                usage: Default::default(),
            latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: "turn-1".to_string(),
                    agent_id: "agent-%1".to_string(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![crate::agent::ActionResult::running(
                &turn,
                &action,
                vec!["shell command accepted for pane execution".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "sleep 1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "sleep 1".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    service.record_running_shell_transaction_output("%1", b"first output\n");
    let first_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(first_text.contains("first output"), "{first_text}");

    service.record_running_shell_transaction_output("%1", b"second output\n");
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let second_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!second_text.contains("first output"), "{second_text}");
    assert!(second_text.contains("second output"), "{second_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let output_line = styled_lines
        .iter()
        .find(|line| line.text.contains("second output"))
        .unwrap();
    assert!(
        output_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_status.foreground)
                && span.rendition.dim
        }),
        "transient shell output should use muted status/thinking style: {:?}",
        output_line.style_spans
    );

    let encoded_tail =
        base64::engine::general_purpose::STANDARD.encode(b"decoded transported output\n");
    let transported_tail = format!(
        "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n{encoded_tail}\n__MEZ_SHELL_OUTPUT_BASE64_END__\n"
    );
    service.record_running_shell_transaction_output("%1", transported_tail.as_bytes());
    let decoded_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        decoded_text.contains("decoded transported output"),
        "{decoded_text}"
    );
    assert!(
        !decoded_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{decoded_text}"
    );

    service.record_running_shell_transaction_output(
        "%1",
        b"final output\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\\r\n~/repo > ",
    );
    let final_output_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        final_output_text.contains("final output"),
        "{final_output_text}"
    );
    assert!(
        !final_output_text.contains("~/repo >"),
        "{final_output_text}"
    );
    assert!(
        !final_output_text
            .lines()
            .any(|line| line.trim_end().ends_with(">") && !line.contains("final output")),
        "{final_output_text}"
    );

    service.record_running_shell_transaction_output("%1", b"~/repo > ");
    let prompt_tail_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        prompt_tail_text.contains("final output"),
        "{prompt_tail_text}"
    );
    assert!(!prompt_tail_text.contains("~/repo >"), "{prompt_tail_text}");

    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: next stage")
        .unwrap();
    let final_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!final_text.contains("second output"), "{final_text}");
    assert!(final_text.contains("agent: next stage"), "{final_text}");
}

/// Verifies that planning-time shell action failures stay visible without
/// exposing the exact command in the default pane buffer. The user still sees
/// the policy failure, while command details remain reserved for verbose or
/// trace mode.
#[test]
fn runtime_agent_shell_planning_failure_hides_command_by_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().add_rule(
        crate::permissions::CommandRule::new(["ls"], RuleDecision::Forbid, RuleMatch::Prefix)
            .unwrap(),
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failed-command","input":"list files"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "list files".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "List files".to_string(),
                        command: "ls".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Denied);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: List files (shell command denied before execution"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("before execution: ls"), "{pane_text}");
    assert!(!pane_text.contains("$ ls"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level verbose` is an explicit opt-in for low-level agent lifecycle
/// chatter. Normal mode keeps the pane buffer focused on prompts, assistant
/// text, concise progress, and errors; verbose mode restores provider,
/// protocol, command, and command-output diagnostics for debugging without
/// enabling thinking.
#[test]
fn runtime_agent_verbose_mode_injects_low_level_status_lines() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let verbose = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"verbose","method":"agent/shell/command","params":{"idempotency_key":"agent-verbose","input":"/log-level verbose"}}"#,
        &primary,
    );
    assert!(
        verbose.contains("agent log level for pane %1 is now verbose."),
        "{verbose}"
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-verbose-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: thinking with runtime-batch model test"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: turn turn-1 completed"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level debug` exposes model introspection and action
/// rationales while still hiding the full shell view that verbose and trace show.
#[test]
fn runtime_agent_thinking_mode_injects_action_rationales() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let thinking = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"thinking","method":"agent/shell/command","params":{"idempotency_key":"agent-thinking","input":"/log-level debug"}}"#,
        &primary,
    );
    assert!(
        thinking.contains("agent log level for pane %1 is now debug."),
        "{thinking}"
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-thinking-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP action_results"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("mez> The pane is ready."),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that trace mode exposes the full MAAP exchange in the pane buffer:
/// the model request messages, raw provider response with the parsed action
/// batch, and action results. Summary-only tracing made auto-allow/full-access
/// hangs difficult to diagnose because the user could not copy the actual MAAP
/// messages that drove the state machine.
#[test]
fn runtime_agent_trace_mode_prints_maap_request_response_and_results() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(100, 16).unwrap(), 500).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-trace-maap","input":"trace maap please"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "trace-maap-raw-response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "show trace details".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "Trace visible.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP request"),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""role": "user""#), "{pane_text}");
    assert!(pane_text.contains("trace maap please"), "{pane_text}");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""raw_text": "trace-maap-raw-response""#),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""action_batch""#), "{pane_text}");
    assert!(pane_text.contains(r#""type": "say""#), "{pane_text}");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP action_results"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""status": "succeeded""#),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""structured_content""#), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that normal-mode panes retain a bounded hidden trace log that can
/// later be dumped to the pane, an internal paste buffer, or the clipboard.
///
/// This protects post-failure diagnostics: users should not have to predict in
/// advance that trace mode will be needed, but the retained trace remains
/// bounded and explicit to export.
#[test]
fn runtime_agent_copy_trace_log_retains_hidden_trace_and_writes_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-trace-log","input":"trace retention sentinel"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "trace raw sentinel".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "retain trace details".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "Trace retained.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text_before = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text_before.contains("agent trace: turn turn-1: MAAP request"),
        "{pane_text_before}"
    );

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log buffer retained-trace")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-trace-log""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers.get("retained-trace").unwrap();
    assert!(buffer.contains("trace raw sentinel"), "{buffer}");
    assert!(
        buffer.contains("agent trace: turn turn-1: MAAP response"),
        "{buffer}"
    );

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers.get("clipboard").unwrap();
    assert!(clipboard.contains("trace raw sentinel"), "{clipboard}");
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains("trace raw sentinel"))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("agent trace log for pane %1"),
        "{pane_text_after}"
    );
    assert!(
        pane_text_after.contains("trace raw sentinel"),
        "{pane_text_after}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/copy-context` exports the assembled provider request context
/// through the same pane, buffer, and clipboard targets as the other copy
/// commands.
///
/// The idle path is intentionally covered here because users invoke this
/// diagnostic command when they need to inspect the next prompt's context
/// before a turn is running.
#[test]
fn runtime_agent_copy_context_writes_idle_context_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-context buffer retained-context")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-context""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers.get("retained-context").unwrap();
    assert!(
        buffer.contains(r#""kind": "model_request_context_dump""#),
        "{buffer}"
    );
    assert!(buffer.contains("idle-context-preview-%1"), "{buffer}");

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-context clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers.get("clipboard").unwrap();
    assert!(
        clipboard.contains(r#""kind": "model_request_context_dump""#),
        "{clipboard}"
    );
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains("idle-context-preview-%1"))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-context pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("model_request_context_dump"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/copy-patches` exports exact retained patch payloads and statuses
/// through the same pane, buffer, and clipboard targets as `/copy-trace-log`.
///
/// Patch bodies are deliberately omitted from durable transcript summaries, so
/// this command must use the runtime's structured patch ledger rather than
/// scraping rendered pane text or compact transcript entries.
#[test]
fn runtime_agent_copy_patches_writes_retained_patches_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-copy-patches-export-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let patch = format!("*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n*** End Patch");

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-patches","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap patch response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "write a note".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: patch.clone(),
                        strip: None,
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    poll_until_turn_state(&mut service, "turn-1", AgentTurnState::Completed);

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer retained-patches")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-patches""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers.get("retained-patches").unwrap();
    assert!(buffer.contains("agent patches for pane %1"), "{buffer}");
    assert!(
        buffer.contains("patch 1: turn=turn-1 action=patch-1 status=succeeded"),
        "{buffer}"
    );
    assert!(buffer.contains("*** Begin Patch"), "{buffer}");
    assert!(buffer.contains(&target_rel), "{buffer}");
    assert!(buffer.contains("+alpha"), "{buffer}");

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-patches clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers.get("clipboard").unwrap();
    assert!(clipboard.contains("status=succeeded"), "{clipboard}");
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains(&patch))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-patches pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("agent patches for pane %1"),
        "{pane_text_after}"
    );
    assert!(
        pane_text_after.contains("status=succeeded"),
        "{pane_text_after}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/copy-patches` keeps every patch attempt for a session even when
/// recovery reuses the same turn id and model-authored action id.
///
/// Patch recovery often happens inside one agent turn, and models frequently
/// reuse simple action ids such as `patch`. The export ledger must therefore
/// treat a new running patch after a settled patch as a new attempt rather than
/// overwriting the earlier failed or successful attempt.
#[test]
fn runtime_agent_copy_patches_retains_reused_action_id_attempts() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");

    let first_patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
    let second_patch =
        "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-current\n+updated\n*** End Patch";
    let build_execution =
        |patch: &str, result: crate::agent::ActionResult| crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: format!("patch attempt for {}", result.action_id),
                usage: Default::default(),
            latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![crate::agent::AgentAction {
                        id: result.action_id.clone(),
                        rationale: "apply a source patch".to_string(),
                        payload: crate::agent::AgentActionPayload::ApplyPatch {
                            patch: patch.to_string(),
                            strip: None,
                        },
                    }],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![result],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
    let action_for_result = |patch: &str| crate::agent::AgentAction {
        id: "patch-retry".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };

    let first_action = action_for_result(first_patch);
    let first_running = crate::agent::ActionResult::running(
        &turn,
        &first_action,
        vec!["shell command accepted for pane execution".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(first_patch, first_running),
    );
    let first_failed = crate::agent::ActionResult::failed(
        &turn,
        &first_action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(first_patch, first_failed),
    );

    let second_action = action_for_result(second_patch);
    let second_running = crate::agent::ActionResult::running(
        &turn,
        &second_action,
        vec!["shell command accepted for pane execution".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(second_patch, second_running),
    );
    let second_succeeded = crate::agent::ActionResult::succeeded(
        &turn,
        &second_action,
        vec!["patch applied".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(second_patch, second_succeeded),
    );

    let copy_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer all-patches")
        .unwrap();
    assert!(
        copy_response.contains(r#""command":"copy-patches""#),
        "{copy_response}"
    );
    assert!(copy_response.contains("patches=2"), "{copy_response}");
    let retained = service.paste_buffers.get("all-patches").unwrap();
    assert!(
        retained.contains("patch 1: turn=turn-1 action=patch-retry status=failed"),
        "{retained}"
    );
    assert!(
        retained.contains("patch 2: turn=turn-1 action=patch-retry status=succeeded"),
        "{retained}"
    );
    assert!(retained.contains("-old"), "{retained}");
    assert!(retained.contains("+new"), "{retained}");
    assert!(retained.contains("-current"), "{retained}");
    assert!(retained.contains("+updated"), "{retained}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level debug` exposes MAAP and state-machine diagnostics
/// without exposing the raw shell view. Debug should show the same diagnostic
/// categories as trace and preserve command fields inside MAAP objects, while
/// raw provider text and output previews stay hidden until the pane is
/// explicitly moved to trace.
#[test]
fn runtime_agent_debug_mode_prints_maap_without_shell_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-debug-maap","input":"debug maap please"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "debug-maap-raw-response with debug-secret-command".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a command for debug redaction".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a debug redaction command".to_string(),
                        command: "printf 'debug-secret-command\\n'".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP request"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("hidden at debug log level; use /log-level trace"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""command": "printf 'debug-secret-command\\n'""#),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("debug-maap-raw-response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("$ printf 'debug-secret-command"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a shell command selected by the model is monitorable when
/// verbose mode is enabled: the command line is injected before dispatch and
/// transaction output can settle without exposing wrapper internals.
#[test]
fn runtime_agent_shell_command_output_is_visible_in_verbose_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-output","input":"print a marker"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "print a marker".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Print a marker".to_string(),
                        command: "printf 'agent-visible-%s\\n' output".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions.is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        service.running_shell_transactions.is_empty(),
        "agent shell command should settle before checking verbose presentation"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("$ printf 'agent-visible-%s"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_STATUS"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    assert!(!pane_text.contains("unset MEZ_MARKER_TOKEN"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that default agent command execution keeps one bounded command
/// preview while routing decoded command output into provider context. Raw
/// shell output may be base64-transported in the pane, but the model-facing
/// action result must still receive the decoded child-command output.
#[test]
fn runtime_agent_shell_command_output_keeps_decoded_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-hidden-output","input":"print a hidden marker"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "print a hidden marker".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Print a hidden marker".to_string(),
                        command: "printf 'agent-hidden-%s\\n' output".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..50 {
        let _ = service.poll_pane_outputs(4096).unwrap();
        if service.pending_agent_provider_tasks.contains("turn-1") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(service.pending_agent_provider_tasks.contains("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("mez> Print a hidden marker"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("$ printf 'agent-hidden-%s"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent-hidden-output"),
        "decoded command output should be visible as the transient tail line: {pane_text}"
    );
    assert!(
        !pane_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("unset MEZ_MARKER_TOKEN"), "{pane_text}");
    let context_text = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        context_text.contains("agent-hidden-output"),
        "{context_text}"
    );
    assert!(context_text.contains("output:\n"), "{context_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a bash-backed pane shell survives the first agent shell
/// transaction after the command is displayed. The user-visible failure mode
/// was the primary pane exiting immediately after an agent command preview, so
/// this test waits through transaction settlement and repeated process polls.
#[test]
fn runtime_bash_agent_shell_transaction_keeps_parent_shell_alive() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash parent-shell regression because bash is unavailable");
        return;
    };
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "exercise bash shell survival".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep the parent shell available"
                            .to_string(),
                        command: "printf 'agent-bash-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);

    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions.is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        service.running_shell_transactions.is_empty(),
        "agent transaction should have completed before checking parent shell liveness"
    );
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    for _ in 0..10 {
        let exits = service.poll_pane_processes().unwrap();
        assert!(exits.is_empty(), "{exits:?}");
        assert!(service.pane_processes().contains_pane("%1"));
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_HISTORY_"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the bash-backed pane shell also survives an agent shell
/// transaction when strict interactive options are already enabled. Some users
/// set `errexit` and `nounset` in shell startup files, so the transaction
/// prologue must temporarily disable and later restore both without letting a
/// failed agent command close the pane or the enclosing Mez session.
#[test]
fn runtime_bash_agent_shell_transaction_preserves_strict_parent_shell_options() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash strict-option regression because bash is unavailable");
        return;
    };
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .write_input_to_pane(&primary, Some("%1"), b"set -eu\n")
        .unwrap();
    for _ in 0..20 {
        let _ = service.poll_pane_outputs(4096).unwrap();
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-strict-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "exercise bash strict shell survival".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep strict shell options intact"
                            .to_string(),
                        command: "printf 'agent-bash-strict-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);

    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions.is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(service.running_shell_transactions.is_empty());
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    if !service.pending_agent_provider_tasks().is_empty() {
        let completion_provider = RuntimeBatchProvider {
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "done".to_string(),
                usage: Default::default(),
            latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(runtime_complete_batch("turn-1")),
                provider_transcript_events: Vec::new(),
            },
        };
        let completions = service
            .poll_agent_provider_tasks_with_provider(&completion_provider, 1)
            .unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].terminal_state, AgentTurnState::Completed);
    }

    service
        .write_input_to_pane(&primary, Some("%1"), b"case $- in *e*u*|*u*e*) printf 'STRICT_OPTIONS_STILL_SET\\n';; *) printf 'STRICT_OPTIONS_LOST:%s\\n' \"$-\";; esac\n")
        .unwrap();
    let mut pane_text = String::new();
    for _ in 0..50 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        pane_text = service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        if pane_text.contains("STRICT_OPTIONS_STILL_SET") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        pane_text.contains("STRICT_OPTIONS_STILL_SET"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the normal command preview is bounded using the pane's
/// display width. Long generated commands should remain inspectable without
/// flooding the pane buffer or hiding the fact that more wrapped lines exist.
#[test]
fn runtime_agent_shell_command_preview_is_wrapped_and_capped() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 20).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-command-preview","input":"run a long command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let command = "printf 'alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\\n'";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a long command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a long command".to_string(),
                        command: command.to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("▐ $ printf 'alpha"), "{pane_text}");
    assert!(pane_text.contains("▐   ["), "{pane_text}");
    let command_preview_line_count = pane_text
        .lines()
        .skip_while(|line| !line.contains("▐ $ "))
        .take_while(|line| line.contains("▐ $ ") || line.starts_with("▐   "))
        .count();
    assert_eq!(command_preview_line_count, 10, "{pane_text}");
    assert!(
        !pane_text.contains("epsilon zeta eta theta iota kappa lambda mu"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies command previews on wide panes cap their display width at 120 cells.
///
/// The command preview renderer should avoid pane-width lines that are too long
/// to scan while still preserving the existing `$ ` prompt and continuation
/// indentation.
#[test]
fn runtime_agent_shell_command_preview_caps_wide_panes_at_120_cells() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 24).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 24).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_command_preview_to_terminal_buffer(
            "%1",
            &format!("printf '{}'", "abcdef ".repeat(40)),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let command_lines = styled_lines
        .iter()
        .filter(|line| line.text.starts_with("▐ $ ") || line.text.starts_with("▐   "))
        .collect::<Vec<_>>();

    assert!(command_lines.len() > 1, "{styled_lines:?}");
    assert!(
        command_lines
            .iter()
            .all(|line| line.text.chars().count() <= 120),
        "{command_lines:?}"
    );
    assert!(
        command_lines[0].text.starts_with("▐ $ "),
        "{command_lines:?}"
    );
    assert!(
        command_lines
            .iter()
            .skip(1)
            .all(|line| line.text.starts_with("▐   ")),
        "{command_lines:?}"
    );
}

/// Verifies that bootstrap parsing uses the hidden transaction capture rather
/// than the visible pane screen. Bootstrap traffic is normally hidden from the
/// terminal buffer, so parsing only screen history leaves the pane marked as
/// bootstrap-pending and causes a tick-time bootstrap loop.
#[test]
fn runtime_bootstrap_completion_uses_hidden_transaction_output_and_clears_pending() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);
    let marker = "bootstrap-marker";
    let turn_id = "bootstrap-%1-test";
    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\tkernel_version\t6.8.0-generic\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tshell_version\t/bin/sh\n\
env\tpath\t/usr/local/bin:/usr/bin:/bin\n\
env\tcwd\t/home/me/project\n\
env\tproject_root\t/home/me/project\n\
env\tgit_repo\t1\n\
bootstrap\tcomplete\t1714500000\n\
tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t1714500000\n";
    service.running_shell_transactions.insert(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: turn_id.to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: output.len(),
            observed_output_preview: output.to_string(),
            observed_output_truncated: false,
        },
    );

    let observed = service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service.pane_bootstrap_pending.contains("%1"),
        "bootstrap pending should be cleared after one completed attempt"
    );
    let signature = service.pane_environment_signatures.get("%1").unwrap();
    assert_eq!(signature.working_directory, "/home/me/project");
    assert_eq!(signature.project_root.as_deref(), Some("/home/me/project"));
    assert!(
        service
            .tool_discovery_cache
            .get(signature)
            .is_some_and(|inventory| inventory.sed)
    );
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    service.maybe_bootstrap_ready_panes().unwrap();
    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap)
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a completed but unparseable bootstrap attempt is still
/// one-shot. Retrying the same hidden wrapper on every tick floods the pane
/// with Mezzanine-owned shell boilerplate without improving context.
#[test]
fn runtime_bootstrap_unparsed_output_does_not_retry_forever() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);
    let marker = "bootstrap-unparsed-marker";
    let turn_id = "bootstrap-%1-unparsed";
    service.running_shell_transactions.insert(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: turn_id.to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let observed = service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.pane_bootstrap_pending.contains("%1"));
    assert!(!service.pane_environment_signatures.contains_key("%1"));
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    service.maybe_bootstrap_ready_panes().unwrap();
    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap)
    );
    let events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""bootstrap":"unparsed""#)),
        "{events:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies shell-integration prompt markers can clear a stale interactive
/// block after the foreground process has returned to the pane's primary shell.
///
/// Alternate-screen and foreground-interactive programs can leave a pane in
/// `interactive-blocked` even after the user exits back to the shell. The
/// runtime should trust a prompt marker only when process metadata separately
/// confirms that the primary shell is foreground again.
#[test]
fn runtime_passive_prompt_recovers_stale_interactive_blocked_shell() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let observed = service
        .observe_passive_shell_prompt_candidate("%1", "osc133-prompt")
        .unwrap();

    assert_eq!(observed, 1);
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies prompt markers alone do not clear an interactive block when the
/// runtime cannot prove that the pane primary shell is foreground.
///
/// This protects the conservative side of readiness recovery: shell-like text
/// or stale prompt metadata must not cause agent commands to enter an active
/// foreground program.
#[test]
fn runtime_passive_prompt_keeps_interactive_block_without_foreground_shell_proof() {
    let mut service = test_runtime_service();
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let observed = service
        .observe_passive_shell_prompt_candidate("%1", "osc133-prompt")
        .unwrap();

    assert_eq!(observed, 0);
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::InteractiveBlocked
    );
}

/// Verifies a pending shell action is recovered instead of failed when
/// `interactive-blocked` is stale and the pane shell is foreground again.
///
/// The dispatch path used to turn stale interactive-blocked readiness into a
/// hard `pane_not_ready` action failure. That was incorrect when host process
/// metadata already proved the user's shell had returned.
#[test]
fn runtime_shell_dispatch_recovers_stale_interactive_blocked_readiness() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "inspect").unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "inspect the working directory".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions.insert(
        turn.turn_id.clone(),
        crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action".to_string(),
                usage: Default::default(),
            latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![crate::agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.pending_agent_provider_tasks.remove(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let execution_after_dispatch = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap();

    assert!(execution_after_dispatch.is_some());
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Probing
    );
    assert!(
        service
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::ReadinessProbe)
    );
    let execution = service.agent_turn_executions.get(&turn.turn_id).unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(execution.action_results[0].error.is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that runtime model-profile overrides feed both provider execution
/// and the live `agent/list` state surface. The selected profile must remain
/// visible while the turn is running and after the turn completes so clients do
/// not see the generic offline `default` placeholder for a live agent.
#[test]
fn runtime_agent_shell_model_command_overrides_pane_model_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"model","method":"agent/shell/command","params":{"idempotency_key":"model","input":"/model gpt-5.4"}}"#,
        &primary,
    );
    assert!(model.contains("scope=pane:%1"), "{model}");
    assert!(model.contains("profile=gpt-5.4"), "{model}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"prompt","method":"agent/shell/command","params":{"idempotency_key":"prompt","input":"use the selected model"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].model_profile.model, "gpt-5.4");
    let agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(agents.contains(r#""model_profile":"gpt-5.4""#), "{agents}");
    assert!(agents.contains(r#""last_turn_id":"turn-1""#), "{agents}");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeEchoProvider,
            ModelProfile {
                provider: "openai".to_string(),
                model: "gpt-5.4".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let completed_agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents-after","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(
        completed_agents.contains(r#""model_profile":"gpt-5.4""#),
        "{completed_agents}"
    );
}

/// Verifies that clicking pane-frame model and reasoning status pills opens a
/// selector backed by the live provider catalog cache and applies the selected
/// value as a pane-scoped model override. This protects the mouse UI path from
/// drifting away from the `/model` command semantics that provider execution
/// already uses.
#[test]
fn runtime_pane_agent_status_selector_applies_model_and_reasoning() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "gpt-provider-only".to_string(),
            display_name: Some("Provider Only".to_string()),
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
            context_window_tokens: Some(777_777),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    let open_model = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::OpenPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Model,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &open_model)
        .unwrap();
    let model_index = service
        .pane_agent_status_selector
        .as_ref()
        .and_then(|selector| {
            selector
                .items
                .iter()
                .position(|item| item == "openai: gpt-provider-only")
        })
        .expect("model selector should include live provider catalog models");
    let select_model = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Model,
                item_index: model_index,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &select_model)
        .unwrap();
    let (_name, model_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(model_profile.model, "gpt-provider-only");
    assert_eq!(model_profile.reasoning_profile.as_deref(), Some("low"));
    assert_eq!(
        model_profile.provider_options.get("context_window_tokens"),
        Some(&"777777".to_string())
    );

    let open_reasoning = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::OpenPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Reasoning,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &open_reasoning)
        .unwrap();
    let reasoning_items = service
        .pane_agent_status_selector
        .as_ref()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    let reasoning_index = reasoning_items
        .iter()
        .position(|item| item == "high")
        .unwrap_or_else(|| {
            panic!(
                "reasoning selector should include configured provider reasoning levels: {reasoning_items:?}"
            )
        });
    let select_reasoning = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Reasoning,
                item_index: reasoning_index,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &select_reasoning)
        .unwrap();
    let (_name, reasoning_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(reasoning_profile.model, "gpt-provider-only");
    assert_eq!(reasoning_profile.reasoning_profile.as_deref(), Some("high"));
    assert!(service.pane_agent_status_selector.is_none());

    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    let full_access_index = service
        .pane_agent_status_selector
        .as_ref()
        .and_then(|selector| selector.items.iter().position(|item| item == "full-access"))
        .expect("approval selector should include full-access");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                        item_index: full_access_index,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let (_name, preserved_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(preserved_profile.model, "gpt-provider-only");
    assert_eq!(preserved_profile.reasoning_profile.as_deref(), Some("high"));
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        pane_context.agent_model.as_deref(),
        Some("gpt-provider-only")
    );
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}

/// Verifies that the pane agent status latency selector opens, populates with
/// the three allowed latency values, applies a selection as a pane-local
/// override, closes after selection, and surfaces the latency value in the
/// pane-frame context for pill rendering.
#[test]
fn runtime_pane_agent_status_selector_applies_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\nlatency_preference = \"default\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string()],
            context_window_tokens: Some(1_050_000),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    let latency_items = service
        .pane_agent_status_selector
        .as_ref()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    assert_eq!(
        latency_items,
        vec![
            "slow".to_string(),
            "default".to_string(),
            "fast".to_string()
        ]
    );
    let fast_index = latency_items
        .iter()
        .position(|item| item == "fast")
        .expect("latency selector should include fast");
    let select_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                        item_index: fast_index,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(select_report.view_refresh_required);
    assert!(service.pane_agent_status_selector.is_none());
    let (_name, latency_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(latency_profile.model, "gpt-5.5");
    assert_eq!(latency_profile.reasoning_profile.as_deref(), Some("low"));
    assert_eq!(latency_profile.latency_preference.as_deref(), Some("fast"));

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_latency.as_deref(), Some("fast"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("low"));
}

/// Verifies that changing reasoning from the pane-frame selector preserves the
/// active latency preference and keeps the latency pill visible.
///
/// Reasoning changes generate a new pane-scoped model profile. That generated
/// profile must carry forward the provider-visible latency selection so the
/// status bar does not lose its latency dropdown after the user changes only
/// the reasoning level.
#[test]
fn runtime_pane_agent_status_reasoning_preserves_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\nlatency_preference = \"fast\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
            context_window_tokens: Some(1_050_000),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Reasoning,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let reasoning_items = service
        .pane_agent_status_selector
        .as_ref()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    let high_index = reasoning_items
        .iter()
        .position(|item| item == "high")
        .expect("reasoning selector should include high");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Reasoning,
                        item_index: high_index,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(profile.latency_preference.as_deref(), Some("fast"));
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_latency.as_deref(), Some("fast"));

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service.pane_agent_status_selector.is_some(),
        "latency selector should remain available after reasoning changes"
    );
}

/// Verifies that pane-frame latency controls are hidden for providers that do
/// not support a provider-visible latency preference.
///
/// DeepSeek profiles can still carry `latency_preference` metadata for identity
/// and preset display, but exposing a clickable latency selector would suggest
/// a provider request behavior that DeepSeek does not implement.
#[test]
fn runtime_pane_agent_status_hides_latency_for_unsupported_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "deepseek"
default_model_profile = "deepseek-default"

[providers.deepseek]
kind = "deepseek"
models = ["deepseek-v4-pro"]
default_model = "deepseek-v4-pro"

[model_profiles.deepseek-default]
provider = "deepseek"
model = "deepseek-v4-pro"
reasoning_profile = "high"
latency_preference = "fast"

[model_profiles.deepseek-default.provider_options]
reasoning_effort = "high"
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        pane_context.agent_latency, None,
        "unsupported providers should not render a latency status pill"
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service.pane_agent_status_selector.is_none(),
        "unsupported providers should not expose a latency dropdown"
    );
}

/// Verifies that the pane-frame model selector prepends configured presets and
/// applies preset-local automatic sizing without mutating the global sizing
/// defaults. This also protects the model pill contract by keeping the visible
/// model value sourced from the active concrete model after a preset choice
/// changes the pane profile.
#[test]
fn runtime_pane_model_selector_prepends_presets_and_applies_them_locally() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "openai"
default_model_profile = "default"

[agents.auto_sizing]
router_model_profile = "openai-router"
small_model_profile = "openai-small"
medium_model_profile = "openai-medium"
large_model_profile = "openai-large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]

[providers.openai]
kind = "openai"
models = ["gpt-5.5", "gpt-5.4"]
default_model = "gpt-5.5"

[providers.deepseek]
kind = "deepseek"
models = ["deepseek-v4-flash", "deepseek-v4"]
default_model = "deepseek-v4-flash"

[model_profiles.default]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "medium"

[model_profiles.openai-router]
provider = "openai"
model = "gpt-5.4"
reasoning_profile = "medium"

[model_profiles.openai-small]
provider = "openai"
model = "gpt-5.4"
reasoning_profile = "low"

[model_profiles.openai-medium]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "medium"

[model_profiles.openai-large]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "high"

[model_profiles.deepseek-fast]
provider = "deepseek"
model = "deepseek-v4-flash"
reasoning_profile = "high"
latency_preference = "fast"

[model_profiles.deepseek-default]
provider = "deepseek"
model = "deepseek-v4"
reasoning_profile = "xhigh"

[model_presets.openai]
default_model_profile = "default"
auto_sizing_router_model_profile = "openai-router"
auto_sizing_small_model_profile = "openai-small"
auto_sizing_medium_model_profile = "openai-medium"
auto_sizing_large_model_profile = "openai-large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]

[model_presets.deepseek]
default_model_profile = "deepseek-fast"
auto_sizing_router_model_profile = "deepseek-fast"
auto_sizing_small_model_profile = "deepseek-fast"
auto_sizing_medium_model_profile = "deepseek-default"
auto_sizing_large_model_profile = "deepseek-default"
allowed_reasoning_efforts = ["high", "xhigh"]
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let initial_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let initial_pane_context = initial_config.frame_context.panes.get("%1").unwrap();
    assert_eq!(initial_pane_context.agent_preset.as_deref(), Some("openai"));
    assert_eq!(initial_pane_context.agent_model.as_deref(), Some("gpt-5.5"));

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let selector = service
        .pane_agent_status_selector
        .as_ref()
        .expect("model selector should open from the pane status field");
    assert_eq!(selector.field, PaneAgentStatusField::Model);
    assert_eq!(
        &selector.items[..2],
        ["preset: deepseek".to_string(), "preset: openai".to_string()]
    );
    assert_eq!(
        selector
            .items
            .get(selector.active_index)
            .map(String::as_str),
        Some("openai: gpt-5.5")
    );
    let deepseek_index = selector
        .items
        .iter()
        .position(|item| item == "preset: deepseek")
        .unwrap();

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        item_index: deepseek_index,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.pane_agent_status_selector.is_none());
    let (_name, active_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(active_profile.provider, "deepseek");
    assert_eq!(active_profile.model, "deepseek-v4-flash");
    assert_eq!(
        service.agent_auto_sizing.router_model_profile, "openai-router",
        "preset selection must not mutate the global auto-sizing defaults"
    );
    let pane_auto_sizing = service.agent_auto_sizing_overrides.get("%1").unwrap();
    assert_eq!(pane_auto_sizing.router_model_profile, "deepseek-fast");
    assert_eq!(pane_auto_sizing.medium_model_profile, "deepseek-default");
    assert_eq!(
        pane_auto_sizing.allowed_reasoning_efforts,
        vec!["high".to_string(), "xhigh".to_string()]
    );

    let updated_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let updated_pane_context = updated_config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        updated_pane_context.agent_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        updated_pane_context.agent_preset.as_deref(),
        Some("deepseek")
    );
}

/// Verifies that model presets validate every referenced auto-sizing profile at
/// config-load time. Without this guard, invalid preset groups can appear in
/// the selector and fail later during selection or automatic model sizing.
#[test]
fn runtime_model_presets_reject_unknown_auto_sizing_profile_references() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "openai"
default_model_profile = "default"

[providers.openai]
kind = "openai"
models = ["gpt-5.5"]
default_model = "gpt-5.5"

[model_profiles.default]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "medium"

[model_presets.openai]
default_model_profile = "default"
auto_sizing_router_model_profile = "missing-router"
"#
            .to_string(),
        }])
        .unwrap_err();

    assert!(
        error.message().contains(
            "model_presets.openai.auto_sizing_router_model_profile `missing-router` is not configured in model_profiles"
        ),
        "{error:?}"
    );
}

/// Verifies that the /latency slash command displays the current setting when
/// called without args and applies a pane-local override when given a valid
/// value.
#[test]
fn runtime_slash_command_latency_displays_and_applies_override() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["high".to_string()],
            context_window_tokens: Some(1_050_000),
        }],
        vec!["high".to_string()],
    );

    let status_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency")
        .unwrap();
    let status_text = match status_outcome {
        super::AgentShellCommandOutcome::Display { body, .. } => body,
        other => panic!("expected Display outcome for /latency without args, got {other:?}"),
    };
    assert!(
        status_text.contains("latency_preference=default"),
        "status should show default: {status_text}"
    );

    let apply_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency slow")
        .unwrap();
    let apply_text = match apply_outcome {
        super::AgentShellCommandOutcome::Mutated { body, .. } => body,
        other => panic!("expected Mutated outcome for /latency slow, got {other:?}"),
    };
    assert!(
        apply_text.contains("latency_preference=slow"),
        "outcome should show slow: {apply_text}"
    );

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(profile.latency_preference.as_deref(), Some("slow"));
}

/// Verifies that `/thinking` exposes DeepSeek's native thinking-mode toggle as
/// a pane-local model-profile override.
///
/// DeepSeek thinking and reasoning effort are separate provider controls: a
/// profile may retain its reasoning level while the operator disables thinking
/// to force strict MAAP tool calls. This test exercises the same control path
/// used by live agent-shell commands and confirms the resulting provider task
/// receives the generated profile.
#[test]
fn runtime_slash_command_thinking_displays_and_applies_deepseek_override() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[model_profiles.default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let status_outcome = service
        .execute_agent_shell_thinking_command("%1", "/thinking")
        .unwrap();
    let status_text = match status_outcome {
        super::AgentShellCommandOutcome::Display { body, .. } => body,
        other => panic!("expected Display outcome for /thinking without args, got {other:?}"),
    };
    assert!(status_text.contains("enabled=true"), "{status_text}");
    assert!(status_text.contains("explicit=false"), "{status_text}");

    let apply = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"thinking","method":"agent/shell/command","params":{"idempotency_key":"thinking","input":"/thinking off"}}"#,
        &primary,
    );
    assert!(apply.contains("source=runtime-thinking"), "{apply}");
    assert!(apply.contains("thinking=disabled"), "{apply}");
    assert!(apply.contains("changed=true"), "{apply}");

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(profile.thinking_enabled(), Some(false));
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(
        service.model_profile_thinking_enabled(&profile),
        Some(false)
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_thinking.as_deref(), Some("off"));

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"prompt","method":"agent/shell/command","params":{"idempotency_key":"prompt","input":"use the current DeepSeek thinking setting"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].model_profile.thinking_enabled(), Some(false));
}

/// Verifies unsupported providers reject `/thinking` instead of mutating
/// provider-neutral model profiles.
///
/// The thinking toggle is intentionally a provider capability, not a universal
/// model-profile field. OpenAI remains unaffected by the DeepSeek adapter's
/// compatibility controls, so the command should fail fast before creating a
/// runtime-generated profile for an unsupported provider.
#[test]
fn runtime_slash_command_thinking_rejects_unsupported_provider() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let error = service
        .execute_agent_shell_thinking_command("%1", "/thinking off")
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not support a thinking-mode toggle"),
        "{error}"
    );
}

/// Verifies that generated runtime model profiles produce distinct identities
/// when latency preference differs so pane-local overrides for the same
/// provider/model/reasoning tuple do not collapse together.
#[test]
fn runtime_generated_profile_identity_differs_by_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["high".to_string()],
            context_window_tokens: Some(1_050_000),
        }],
        vec!["high".to_string()],
    );

    let default_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency default")
        .unwrap();
    let default_text = match default_outcome {
        super::AgentShellCommandOutcome::Mutated { body, .. } => body,
        other => panic!("expected Mutated outcome, got {other:?}"),
    };
    assert!(default_text.contains("latency_preference=default"));

    let slow_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency slow")
        .unwrap();
    let slow_text = match slow_outcome {
        super::AgentShellCommandOutcome::Mutated { body, .. } => body,
        other => panic!("expected Mutated outcome, got {other:?}"),
    };
    assert!(slow_text.contains("latency_preference=slow"));

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(
        profile.latency_preference.as_deref(),
        Some("slow"),
        "last applied latency should be slow"
    );
}

/// Verifies that clickable pane-frame agent status pills cover live toggles
/// beyond model selection. Automatic reasoning should apply immediately like a
/// button, while approval policy should open the same selector flow used by
/// model and reasoning choices.
#[test]
fn runtime_pane_agent_status_selector_toggles_auto_and_selects_approval() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.agent_routing = false;

    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Routing,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    assert!(service.pane_agent_status_selector.is_none());
    assert_eq!(
        service.agent_routing_overrides.get("%1").copied(),
        Some(true)
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let full_access_index = service
        .pane_agent_status_selector
        .as_ref()
        .and_then(|selector| {
            assert_eq!(selector.field, PaneAgentStatusField::ApprovalPolicy);
            selector.items.iter().position(|item| item == "full-access")
        })
        .expect("approval selector should include full-access");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                        item_index: full_access_index,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.pane_agent_status_selector.is_none());
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(80, 24).unwrap(), &config)
        .unwrap()
        .unwrap();
    let rendered = view.lines.join("\n");
    assert!(rendered.contains("full-access"), "{rendered}");
    assert!(rendered.contains("route"), "{rendered}");
    assert!(rendered.contains("gpt"), "{rendered}");
}

/// Verifies the DeepSeek thinking status pill is an immediate toggle rather
/// than a dropdown selector.
///
/// The pane frame exposes thinking next to reasoning only when the provider
/// supports the capability. Clicking it should reuse the `/thinking toggle`
/// runtime mutation path, update the pane-local generated profile, and refresh
/// the frame context without opening selector state.
#[test]
fn runtime_pane_agent_status_thinking_pill_toggles_deepseek_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[model_profiles.default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let first_toggle = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Thinking,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(first_toggle.view_refresh_required);
    assert!(!first_toggle.full_redraw_required);
    assert!(service.pane_agent_status_selector.is_none());
    let (_off_name, off_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(off_profile.thinking_enabled(), Some(false));
    let off_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        off_config
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_thinking.as_deref()),
        Some("off")
    );

    let second_toggle = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Thinking,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(second_toggle.view_refresh_required);
    assert!(service.pane_agent_status_selector.is_none());
    let (_on_name, on_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(on_profile.thinking_enabled(), Some(true));
}

/// Verifies that pane-frame agent selectors remain modal until the user makes
/// an explicit selection or cancels them. Escape must close the selector
/// without leaking the escape byte into the active pane.
#[test]
fn runtime_pane_agent_status_selector_esc_closes_without_forwarding() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    assert!(service.pane_agent_status_selector.is_some());

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert!(service.pane_agent_status_selector.is_none());
}

/// Verifies pane-frame model and reasoning dropdowns support keyboard
/// navigation. The active row should move with arrow input and Enter should
/// apply the same pane-scoped `/model` mutation as mouse selection.
#[test]
fn runtime_pane_agent_status_selector_accepts_keyboard_navigation() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let (active_index, target_index) = service
        .pane_agent_status_selector
        .as_ref()
        .map(|selector| {
            (
                selector.active_index,
                selector
                    .items
                    .iter()
                    .position(|item| item == "openai: gpt-5.4")
                    .expect("model selector should include gpt-5.4"),
            )
        })
        .expect("model selector should be open");
    let movement = if target_index < active_index {
        b"\x1b[A".to_vec()
    } else {
        b"\x1b[B".to_vec()
    };

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(movement),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert!(service.pane_agent_status_selector.is_none());
    let (_name, model_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(model_profile.model, "gpt-5.4");
}

/// Verifies mouse-wheel input over an open pane agent selector scrolls the
/// selector itself rather than falling through to pane scrollback.
#[test]
fn runtime_pane_agent_status_selector_scrolls_only_dropdown_contents() {
    let mut service = test_runtime_service();
    let models = (0..40)
        .map(|index| format!("\"gpt-test-{index:02}\""))
        .collect::<Vec<_>>()
        .join(", ");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [{models}]\ndefault_model = \"gpt-test-00\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-test-00\"\n"
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .pane_agent_status_selector
            .as_ref()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: 3,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .pane_agent_status_selector
            .as_ref()
            .map(|selector| selector.scroll_offset),
        Some(3)
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: -30,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .pane_agent_status_selector
            .as_ref()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );
}

/// Verifies that `/model list` uses the active provider catalog surface instead
/// of listing only manually named profiles. In this test there is no auth store
/// attached, so the runtime must fall back to the configured provider model set
/// and clearly label the catalog source while still exposing reasoning choices.
#[tokio::test]
async fn runtime_agent_shell_model_list_displays_provider_model_catalog() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(model_list.contains(r#""kind":"display""#), "{model_list}");
    assert!(model_list.contains(r#""command":"model""#), "{model_list}");
    assert!(
        model_list.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{model_list}"
    );
    assert!(model_list.contains("## Model Catalog"), "{model_list}");
    assert!(!model_list.contains("### Active Selection"), "{model_list}");
    assert!(!model_list.contains("### Available Models"), "{model_list}");
    assert!(!model_list.contains("Provider catalog unavailable"), "{model_list}");
    assert!(
        model_list.contains(
            "| Provider | Model | Reasoning levels | Context limit | Source | Active profile |"
        ),
        "{model_list}"
    );
    assert!(
        model_list.contains("| openai | ★ gpt-5.5 |"),
        "{model_list}"
    );
    assert!(model_list.contains("| openai | gpt-5.4 |"), "{model_list}");
    assert!(
        model_list.contains("★ default, low, medium, high, xhigh"),
        "{model_list}"
    );
    assert!(!model_list.contains("### Quota Usage"), "{model_list}");
    assert!(!model_list.contains("provider quota"), "{model_list}");
    assert!(!model_list.contains("**Usage:**"), "{model_list}");
}
