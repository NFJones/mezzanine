//! Transient shell previews, visual limits and exact terminal lineage.
//!
//! These fixtures keep live output separate from durable transcript rows and
//! require owner-specific cleanup before later output can consume a preview.

use super::*;

/// Verifies hidden model shell commands expose a bounded live latest-output tail.
///
/// Normal logging hides raw PTY output, but users still need lightweight
/// progress for long-running commands. The latest cleaned stdout/stderr lines
/// should replace the previous transient preview block and disappear when the
/// next durable agent transcript line is written.
#[test]
fn runtime_hidden_model_shell_command_shows_transient_latest_output_line() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .start_agent_prompt_turn("%1", "run a command")
        .unwrap();
    assert_eq!(start.state, AgentTurnState::Running);
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),

        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Run a command".to_string(),
            command: "sleep 1".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        "turn-1".to_string(),
        mez_agent::AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                model_capabilities: Default::default(),
                max_input_tokens: None,
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
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::for_capability(
                    mez_agent::AgentCapability::Shell,
                ),
                messages: vec![mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    content: "run a command".to_string(),
                }]
                .into(),
            },
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "test action batch rationale".to_string(),

                    actions: vec![action.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
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
    service.running_shell_transactions_mut_for_tests().insert(
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
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let assert_status_output = |service: &RuntimeSessionService, text: &str| {
        let styled_lines = service
            .pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let output_line = styled_lines
            .iter()
            .find(|line| line.text.contains(text))
            .unwrap_or_else(|| panic!("missing shell output `{text}` in {styled_lines:?}"));
        let output_column = output_line.text.find(text).unwrap();
        let rendition = styled_line_rendition_at(output_line, output_column);
        assert_eq!(
            rendition.foreground,
            Some(theme.colors.agent_transcript_status.foreground),
            "shell output should use the configured status foreground in every frame: {:?}",
            output_line.style_spans
        );
        assert!(
            rendition.dim && !rendition.bold,
            "shell output should retain baseline shadow styling in every frame: {:?}",
            output_line.style_spans
        );

        let view = service
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 12).unwrap(),
                &TerminalClientLoopConfig::default(),
            )
            .unwrap()
            .expect("active pane should render a primary client view");
        let view_row = view
            .lines
            .iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("missing shell output `{text}` in {:?}", view.lines));
        let view_column = view.lines[view_row].find(text).unwrap();
        let view_rendition = view.line_style_spans[view_row]
            .iter()
            .rev()
            .find(|span| {
                view_column >= span.start && view_column < span.start.saturating_add(span.length)
            })
            .map(|span| span.rendition)
            .unwrap_or_default();
        assert_eq!(
            view_rendition.foreground,
            Some(theme.colors.agent_transcript_status.foreground),
            "client view should preserve the configured status foreground in every frame: {:?}",
            view.line_style_spans[view_row]
        );
        assert!(
            view_rendition.dim && !view_rendition.bold,
            "client view should preserve baseline shadow output in every frame: {:?}",
            view.line_style_spans[view_row]
        );
    };
    let first_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(first_text.contains("first output"), "{first_text}");
    assert_status_output(&service, "first output");

    service
        .update_agent_shell_output_preview(
            "%1",
            crate::runtime::render::RuntimeAgentShellPreviewOwner {
                turn_id: "turn-1".to_string(),
                action_id: "shell-1".to_string(),
                marker: "marker-1".to_string(),
            },
            1,
            &["first output".to_string()],
        )
        .unwrap();
    let unchanged_preview_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(unchanged_preview_text, first_text);

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
    assert!(second_text.contains("first output"), "{second_text}");
    assert!(second_text.contains("second output"), "{second_text}");
    assert_status_output(&service, "first output");
    assert_status_output(&service, "second output");

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
    assert_status_output(&service, "decoded transported output");

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
    assert_status_output(&service, "final output");

    let settled_owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-1".to_string(),
        action_id: "shell-1".to_string(),
        marker: "marker-1".to_string(),
    };
    assert!(service.settle_agent_shell_output_preview("%1", &settled_owner));
    assert_status_output(&service, "final output");

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

/// Hidden shell output limits must count terminal visual rows after wrapping,
/// including hard wrapping for a long source line without whitespace.
#[test]
fn runtime_shell_output_preview_limit_counts_visual_rows() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(12, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service.set_agent_pane_screen("%1", conversation_id, screen);
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-visual-tail".to_string(),
        action_id: "shell-visual-tail".to_string(),
        marker: "marker-visual-tail".to_string(),
    };
    let source = "x".repeat(100);

    service
        .update_agent_shell_output_preview("%1", owner.clone(), 1, std::slice::from_ref(&source))
        .unwrap();

    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let preview_rows = visible.iter().filter(|line| line.contains('x')).count();
    assert_eq!(preview_rows, 5, "{visible:?}");
    assert_eq!(
        service.agent_shell_output_previews_for_tests("%1"),
        vec![(owner, 0, 1, vec![source])]
    );
}

/// Hidden shell previews honor the configured agent wrap cap on panes wider
/// than the cap while retaining the newest visual rows after wrapping.
#[test]
fn runtime_shell_output_preview_honors_configured_column_cap() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 12\n".to_string(),
        }])
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(40, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service.set_agent_pane_screen("%1", conversation_id, screen);
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-capped-preview".to_string(),
        action_id: "shell-capped-preview".to_string(),
        marker: "marker-capped-preview".to_string(),
    };
    let source = "discard discard alpha beta gamma 0123456789abcdefghij".to_string();

    service
        .update_agent_shell_output_preview("%1", owner, 1, std::slice::from_ref(&source))
        .unwrap();

    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let preview_rows = visible
        .iter()
        .filter(|line| {
            ["alpha", "beta", "gamma", "0123456789", "abcdefghij"]
                .iter()
                .any(|segment| line.contains(segment))
        })
        .collect::<Vec<_>>();
    assert_eq!(preview_rows.len(), 5, "{visible:?}");
    assert!(
        preview_rows
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 12),
        "{visible:?}"
    );
    assert!(
        visible.iter().all(|line| !line.contains("discard")),
        "{visible:?}"
    );
}

/// Verifies concurrent shell previews retain actor chronology and durable rows.
///
/// Each running action owns an independent preview slot. Updating one owner
/// must preserve first-seen ordering, stale revisions must be ignored, and a
/// durable diff appended between updates must survive later projection and
/// owner-specific retirement.
#[test]
fn runtime_shell_previews_preserve_owner_order_and_durable_output() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(100, 20).unwrap(), 40).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let owner_a = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-a".to_string(),
        action_id: "action-a".to_string(),
        marker: "marker-a".to_string(),
    };
    let owner_b = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-b".to_string(),
        action_id: "action-b".to_string(),
        marker: "marker-b".to_string(),
    };

    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            1,
            &["shared owner output".to_string()],
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_b.clone(),
            1,
            &["shared owner output".to_string()],
        )
        .unwrap();
    let identical_previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(identical_previews.len(), 2, "{identical_previews:?}");
    assert_ne!(identical_previews[0].0, identical_previews[1].0);
    assert_eq!(identical_previews[0].3, identical_previews[1].3);

    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            2,
            &["owner A revision 2".to_string()],
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            1,
            &["stale owner A revision".to_string()],
        )
        .unwrap();

    let previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 2, "{previews:?}");
    assert_eq!(
        previews[0],
        (
            owner_a.clone(),
            0,
            2,
            vec!["owner A revision 2".to_string()]
        )
    );
    assert_eq!(
        previews[1],
        (
            owner_b.clone(),
            1,
            1,
            vec!["shared owner output".to_string()]
        )
    );

    service
        .append_agent_diff_text_to_terminal_buffer(
            "%1",
            "diff --git a/note.txt b/note.txt\n--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_b.clone(),
            2,
            &["owner B revision 2".to_string()],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner_a));
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            3,
            &["post-settlement owner A revision".to_string()],
        )
        .unwrap();
    assert_eq!(
        service.agent_shell_output_previews_for_tests("%1")[0].3,
        vec!["owner A revision 2".to_string()]
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: next stage")
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("+++ note.txt"), "{pane_text}");
    assert!(pane_text.contains("+new"), "{pane_text}");
    assert!(!pane_text.contains("owner A revision"), "{pane_text}");
    assert!(pane_text.contains("owner B revision 2"), "{pane_text}");
    assert!(!pane_text.contains("stale owner A revision"), "{pane_text}");
    let diff_index = pane_text.find("+++ note.txt").unwrap();
    let preview_index = pane_text.find("owner B revision 2").unwrap();
    assert!(diff_index < preview_index, "{pane_text}");
}

/// Verifies provider-claim settlement leaves a completed shell tail installed
/// until the next durable row replaces it at the same pane-log position.
///
/// Clearing worker ownership is bookkeeping rather than visible output. If it
/// restores the preview baseline first, the attached terminal scrolls to erase
/// the tail and then scrolls again for the following action, producing a jump.
#[test]
fn runtime_shell_preview_claim_cleanup_waits_for_next_durable_row() {
    let mut service = test_runtime_service();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service.set_agent_pane_screen(
        "%1",
        conversation_id,
        TerminalScreen::new(Size::new(60, 12).unwrap(), 40).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable baseline")
        .unwrap();
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-preview-handoff".to_string(),
        action_id: "shell-preview-handoff".to_string(),
        marker: "marker-preview-handoff".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &["completed command tail".to_string()],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    let retained_screen = service.agent_pane_screen("%1").unwrap().clone();
    let preview_row = retained_screen
        .normal_content_lines()
        .iter()
        .position(|line| line.contains("completed command tail"))
        .unwrap();

    service.clear_claimed_agent_provider_task("turn-preview-handoff");

    assert_eq!(
        service.agent_pane_screen("%1").unwrap(),
        &retained_screen,
        "claim cleanup must not erase settled command output"
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "next action")
        .unwrap();
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("completed command tail")),
        "the durable append should retire the transient tail: {lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .position(|line| line.contains("next action"))
            .unwrap(),
        preview_row,
        "the next durable item should occupy the preview head row"
    );
}

/// Verifies a settled multi-row shell tail is replaced without rewinding a full pane.
///
/// Once the transient tail has scrolled durable rows into history, the next
/// durable item must reuse that presented displacement instead of rebuilding
/// from the pre-tail baseline and shifting the complete viewport downward.
#[test]
fn runtime_shell_preview_handoff_preserves_bottom_viewport_origin() {
    let mut service = test_runtime_service();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(60, 5).unwrap(), 40).unwrap();
    screen.feed(b"durable-zero\r\ndurable-one\r\ndurable-two\r\ndurable-three\r\ndurable-four");
    service.set_agent_pane_screen("%1", conversation_id, screen);
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-bottom-handoff".to_string(),
        action_id: "shell-bottom-handoff".to_string(),
        marker: "marker-bottom-handoff".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &[
                "tail-one".to_string(),
                "tail-two".to_string(),
                "tail-three".to_string(),
            ],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    let projected = service.agent_pane_screen("%1").unwrap();
    assert_eq!(
        projected.visible_lines(),
        vec![
            "durable-three",
            "durable-four",
            "│ tail-one",
            "│ tail-two",
            "│ tail-three",
        ]
    );
    let projected_history_len = projected.history().len();

    service
        .append_agent_status_text_to_terminal_buffer("%1", "next durable")
        .unwrap();

    let replaced = service.agent_pane_screen("%1").unwrap();
    assert_eq!(replaced.history().len(), projected_history_len);
    assert_eq!(
        replaced.visible_lines(),
        vec!["durable-three", "durable-four", "│ next durable", "", "",]
    );
    assert_eq!(replaced.cursor_state().row, 3);
}

/// Verifies a shorter live revision cannot undo scrolling by an earlier tail.
///
/// The command window has already displaced durable rows. Replacing its three
/// rows with one must clear the unused rows below, not pull history back down;
/// the next durable append must then consume that same window head.
#[test]
fn runtime_shell_preview_shrink_preserves_visible_rows() {
    let mut service = test_runtime_service();
    let client_size = Size::new(60, 12).unwrap();
    service
        .attach_primary("primary", true, client_size, 120)
        .unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(60, 5).unwrap(), 40).unwrap();
    screen.feed(b"durable-zero\r\ndurable-one\r\ndurable-two\r\ndurable-three\r\ndurable-four");
    service.set_agent_pane_screen("%1", conversation_id, screen);
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shrink".to_string(),
        action_id: "shell-shrink".to_string(),
        marker: "marker-shrink".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &[
                "tail-one".to_string(),
                "tail-two".to_string(),
                "tail-three".to_string(),
            ],
        )
        .unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let displayed_tail = service
        .render_client_view(ClientViewRole::Primary, client_size, &config)
        .unwrap()
        .unwrap();
    service
        .update_agent_shell_output_preview("%1", owner.clone(), 2, &["shorter tail".to_string()])
        .unwrap();
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().visible_lines(),
        vec!["durable-three", "durable-four", "│ shorter tail", "", ""]
    );
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    service
        .append_agent_status_text_to_terminal_buffer("%1", "next durable")
        .unwrap();
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().visible_lines(),
        vec!["durable-three", "durable-four", "│ next durable", "", ""]
    );
    let displayed_replacement = service
        .render_client_view(ClientViewRole::Primary, client_size, &config)
        .unwrap()
        .unwrap();
    for marker in ["durable-three", "durable-four"] {
        let row = displayed_tail
            .lines
            .iter()
            .position(|line| line.contains(marker))
            .unwrap();
        assert_eq!(
            displayed_replacement
                .lines
                .iter()
                .position(|line| line.contains(marker)),
            Some(row)
        );
    }
    assert_eq!(
        displayed_replacement
            .lines
            .iter()
            .position(|line| line.contains("next durable"))
            .unwrap(),
        displayed_tail
            .lines
            .iter()
            .position(|line| line.contains("tail-one"))
            .unwrap(),
    );
}

/// Verifies a clipped command window remains usable after a shorter revision.
///
/// A tail taller than the pane can put its original head into history. New
/// live and durable rows must use the visible window head rather than disappear
/// into that obsolete off-screen position.
#[test]
fn runtime_shell_preview_shrink_keeps_clipped_replacement_visible() {
    let mut service = test_runtime_service();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(60, 2).unwrap(), 40).unwrap();
    screen.feed(b"durable-zero\r\ndurable-one");
    service.set_agent_pane_screen("%1", conversation_id, screen);
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-clipped".to_string(),
        action_id: "shell-clipped".to_string(),
        marker: "marker-clipped".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &[
                "tail-one".to_string(),
                "tail-two".to_string(),
                "tail-three".to_string(),
            ],
        )
        .unwrap();
    service
        .update_agent_shell_output_preview("%1", owner.clone(), 2, &["replacement".to_string()])
        .unwrap();
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().visible_lines(),
        vec!["│ replacement", ""]
    );
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    service
        .append_agent_status_text_to_terminal_buffer("%1", "next durable")
        .unwrap();
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().visible_lines(),
        vec!["│ next durable", ""]
    );
}

/// Verifies aggregate turn cancellation retires only the matching owners.
///
/// Cancellation may cover managed or native work and therefore operates at the
/// turn scope. Other turns sharing the pane must remain projected in their
/// original first-seen order, and durable rows must remain untouched.
#[test]
fn runtime_shell_preview_turn_retirement_preserves_other_turns() {
    let mut service = test_runtime_service();
    let screen = TerminalScreen::new(Size::new(100, 20).unwrap(), 40).unwrap();
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable sentinel")
        .unwrap();
    let owner_a = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-a".to_string(),
        action_id: "action-a".to_string(),
        marker: "marker-a".to_string(),
    };
    let owner_b = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-b".to_string(),
        action_id: "action-b".to_string(),
        marker: "marker-b".to_string(),
    };
    service
        .update_agent_shell_output_preview("%1", owner_a, 1, &["cancelled turn output".to_string()])
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_b.clone(),
            1,
            &["surviving turn output".to_string()],
        )
        .unwrap();

    assert_eq!(
        service
            .retire_agent_shell_output_previews_for_turn("turn-a")
            .unwrap(),
        1
    );

    let previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 1, "{previews:?}");
    assert_eq!(previews[0].0, owner_b);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("durable sentinel"), "{pane_text}");
    assert!(!pane_text.contains("cancelled turn output"), "{pane_text}");
    assert!(pane_text.contains("surviving turn output"), "{pane_text}");
}

/// Verifies stale preview cleanup cannot erase an intervening pane generation.
///
/// If a write outside the preview compositor changes the retained screen, the
/// remembered installed generation no longer owns the pane. Cleanup must then
/// discard only stale metadata and leave the intervening row untouched.
#[test]
fn runtime_shell_preview_cleanup_requires_exact_screen_lineage() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(100, 20).unwrap(), 40).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-a".to_string(),
        action_id: "action-a".to_string(),
        marker: "marker-a".to_string(),
    };
    service
        .update_agent_shell_output_preview("%1", owner, 1, &["live preview".to_string()])
        .unwrap();
    let mut intervening = service.agent_pane_screen("%1").unwrap().clone();
    intervening.feed(b"\r\nintervening durable row\r\n");
    service.set_agent_pane_screen("%1", conversation_id, intervening);

    service.clear_agent_shell_output_status_line("%1").unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("intervening durable row"), "{pane_text}");
    assert!(
        service
            .agent_shell_output_previews_for_tests("%1")
            .is_empty()
    );
}
