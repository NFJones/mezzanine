//! Runtime tests for agent presentation terminal ui behavior.

use super::*;
use crate::runtime::{RenderInvalidationReason, RuntimeTransition};

/// Verifies partial terminal configuration retains the product's 30 FPS
/// default instead of falling back to the obsolete 5 FPS render cadence.
#[test]
fn runtime_uses_product_render_rate_default_when_config_key_is_absent() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\ncursor_blink = false\n".to_string(),
        }])
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();

    assert_eq!(config.render_rate_limit_fps, 30);
}

/// Verifies ordinary structured pane-log rows honor the configured agent
/// column cap even when the owning pane is wider than that cap. Continuation
/// rows must retain the agent gutter instead of relying on terminal soft wrap.
#[test]
fn runtime_structured_pane_log_rows_honor_configured_column_cap() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
        }])
        .unwrap();
    // Another runtime must not reset this service's configured wrap policy.
    let _unrelated_service = test_runtime_service();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 40).unwrap(), 200).unwrap(),
    );

    let status = "agent: provider recovery continues after a temporary outage";
    service
        .append_agent_status_text_to_terminal_buffer("%1", status)
        .unwrap();
    service
        .append_agent_error_text_to_terminal_buffer(
            "%1",
            "agent error: provider request failed after the configured timeout",
        )
        .unwrap();
    service
        .append_agent_pty_diagnostic_bytes_to_terminal_buffer(
            "%1",
            b"pty diagnostic: child process emitted a long sanitized warning",
        )
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "mcp-long-header".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "github".to_string(),
            tool: "search_issues_with_a_long_name".to_string(),
            arguments_json: r#"{"query":"pane log wrapping"}"#.to_string(),
        },
    };
    assert!(
        service
            .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
            .unwrap()
    );
    let result = mez_agent::ActionResult {
        protocol: "maap/1".to_string(),
        turn_id: "turn-pane-log-wrap".to_string(),
        agent_id: "agent-%1".to_string(),
        action_id: action.id.clone(),
        action_type: "mcp_call",
        status: ActionStatus::Succeeded,
        content: Vec::new(),
        structured_content_json: None,
        permission_evaluation: None,
        is_error: false,
        error: None,
    };
    service
        .append_agent_action_result_text_to_terminal_buffer(
            "%1",
            &action,
            &result,
            "result preview contains averyveryverylongunbrokentoken and trailing context",
        )
        .unwrap();

    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .filter(|line| !line.text.trim().is_empty())
        .collect::<Vec<_>>();
    assert!(rows.len() > 1, "{rows:?}");
    assert!(
        rows.iter()
            .all(|line| UnicodeWidthStr::width(line.text.as_str()) <= 24),
        "{rows:?}"
    );
    assert!(
        rows.iter().all(|line| line.text.starts_with("▐ ")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|line| line.text.starts_with("▐        recovery")),
        "{rows:?}"
    );

    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let action_line = rows
        .iter()
        .find(|line| line.text.contains("mcp call"))
        .unwrap();
    let action_column = display_column_for_fragment(&action_line.text, "mcp call");
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
    let error_line = rows
        .iter()
        .find(|line| line.text.contains("agent error:"))
        .unwrap();
    let error_column = display_column_for_fragment(&error_line.text, "agent error:");
    let error_rendition = styled_line_rendition_at(error_line, error_column);
    assert_eq!(
        error_rendition.foreground,
        Some(theme.colors.agent_transcript_error.foreground)
    );

    let copy_mode = ensure_agent_copy_mode_for_test(&mut service, "%1");
    let status_start = copy_mode
        .lines()
        .iter()
        .position(|line| line.contains("agent: provider"))
        .unwrap();
    let status_end = copy_mode
        .lines()
        .iter()
        .enumerate()
        .skip(status_start.saturating_add(1))
        .find(|(_index, line)| line.contains("agent error:"))
        .map(|(index, _line)| index.saturating_sub(1))
        .unwrap();
    let status_end_column = UnicodeWidthStr::width(copy_mode.lines()[status_end].as_str());
    copy_mode
        .select_range(
            CopyPosition {
                line: status_start,
                column: 0,
            },
            CopyPosition {
                line: status_end,
                column: status_end_column,
            },
        )
        .unwrap();
    assert_eq!(
        copy_mode
            .copy_selection_with_format(crate::host::terminal::CopySelectionFormat::Source)
            .unwrap(),
        status
    );
}

/// Verifies snapshot-only structured presentation rows are capped when replay
/// falls back to saved display text rather than a semantic source renderer.
#[test]
fn runtime_structured_pane_log_replay_fallback_honors_configured_column_cap() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
        }])
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 20).unwrap(), 200).unwrap(),
    );
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let source = "agent: legacy structured status continues beyond the configured cap";
    let entry = crate::storage::transcript::AgentPresentationEntry {
        conversation_id,
        sequence: 1,
        created_at_unix_seconds: 1,
        pane_id: "%1".to_string(),
        turn_id: None,
        terminal_width: 80,
        style_names: vec!["status".to_string()],
        display_lines: vec![source.to_string()],
        copy_lines: vec![source.to_string()],
        ansi_text: None,
        source_text: None,
        source_content_type: None,
    };

    assert!(
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &[entry])
            .unwrap()
    );
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert!(rows.len() > 1, "{rows:?}");
    assert!(
        rows.iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 24),
        "{rows:?}"
    );
    assert!(rows.iter().all(|line| line.starts_with("▐ ")), "{rows:?}");
}

/// Verifies legacy ANSI-only presentation records remain byte-stream replay
/// inputs. Rewrapping escape-bearing bytes could alter terminal controls, so
/// this compatibility path deliberately relies on the pane's physical width.
#[test]
fn runtime_legacy_raw_ansi_replay_is_not_rewrapped_to_agent_column_cap() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
        }])
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 10).unwrap(), 200).unwrap(),
    );
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let display = "▐ legacy raw ANSI projection remains wider than the cap";
    let entry = crate::storage::transcript::AgentPresentationEntry {
        conversation_id,
        sequence: 1,
        created_at_unix_seconds: 1,
        pane_id: "%1".to_string(),
        turn_id: None,
        terminal_width: 80,
        style_names: vec!["status".to_string()],
        display_lines: vec![display.to_string()],
        copy_lines: vec![display.to_string()],
        ansi_text: Some(format!("\r\x1b[2m{display}\x1b[0m\r\n")),
        source_text: None,
        source_content_type: None,
    };

    assert!(
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &[entry])
            .unwrap()
    );
    let row = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .into_iter()
        .find(|line| line.contains("legacy raw ANSI"))
        .unwrap();
    assert_eq!(row, display);
    assert!(UnicodeWidthStr::width(row.as_str()) > 24, "{row:?}");
}

/// Verifies that terminal cursor presentation settings are parsed from runtime
/// configuration layers and applied to attached-terminal render configuration.
#[test]
fn runtime_applies_cursor_presentation_options_from_config_layers() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\ncursor_style = \"bar\"\ncursor_blink = false\ncursor_blink_interval_ms = 250\nresize_debounce_ms = 125\nrender_rate_limit_fps = 8\nreduced_motion = true\nenhanced_keyboard_reporting = true\ncompletion_attention_flashing = false\n"
                .to_string(),
        }])
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();

    assert_eq!(
        config.cursor_style,
        mez_mux::presentation::TerminalCursorStyle::Bar
    );
    assert!(!config.cursor_blink);
    assert_eq!(config.cursor_blink_interval_ms, 250);
    assert_eq!(config.resize_debounce_ms, 125);
    assert_eq!(config.render_rate_limit_fps, 8);
    assert!(config.enhanced_keyboard_reporting);
    assert!(config.frame_context.reduced_motion);
    assert!(config.frame_context.completion_attention_static);
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}

/// Verifies explicit streaming opt-out and reduced-motion policy both suppress
/// provisional provider presentation without preventing the provider turn
/// from remaining active for authoritative completion.
#[test]
fn runtime_streaming_output_policy_suppresses_provider_deltas() {
    for terminal_config in [
        "streaming_output = false\nreduced_motion = false",
        "streaming_output = true\nreduced_motion = true",
    ] {
        let mut service = test_runtime_service();
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: format!("[terminal]\n{terminal_config}\n"),
            }])
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
        );
        let started = service
            .start_agent_prompt_turn("%1", "stream this response")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        let high_water_mark = service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .event_sequence_high_water_mark();
        service
            .record_claimed_agent_provider_context_for_tests(&turn.turn_id, high_water_mark)
            .unwrap();
        let baseline = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();

        let transition = service.apply_agent_provider_streaming_say_transition(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            "%1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: "text/markdown; charset=utf-8".to_string(),
            },
        );

        assert_eq!(transition, RuntimeTransition::default());
        assert!(
            service
                .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines(),
            baseline
        );
        assert!(service.agent_provider_task_is_claimed(&turn.turn_id));
    }
}

/// Verifies disabling streaming output during a provider response restores the
/// pre-stream screen, suppresses subsequent deltas, and allows only newly
/// started actions to render after streaming is enabled again.
#[test]
fn runtime_streaming_output_toggle_discards_and_restarts_provisional_rendering() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
    );
    let started = service
        .start_agent_prompt_turn("%1", "stream around a config reload")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let high_water_mark = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .event_sequence_high_water_mark();
    service
        .record_claimed_agent_provider_context_for_tests(&turn.turn_id, high_water_mark)
        .unwrap();
    let agent_id = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let baseline = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();

    service.apply_agent_provider_streaming_say_transition(
        &agent_id,
        &turn.turn_id,
        "%1",
        &mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: "text/plain; charset=utf-8".to_string(),
        },
    );
    service.apply_agent_provider_streaming_say_transition(
        &agent_id,
        &turn.turn_id,
        "%1",
        &mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "discarded prefix".to_string(),
        },
    );
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("discarded prefix")
    );

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nstreaming_output = false\n".to_string(),
        }])
        .unwrap();
    assert_eq!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        baseline
    );
    assert_eq!(
        service.apply_agent_provider_streaming_say_transition(
            &agent_id,
            &turn.turn_id,
            "%1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "suppressed suffix".to_string(),
            },
        ),
        RuntimeTransition::default()
    );

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nstreaming_output = true\n".to_string(),
        }])
        .unwrap();
    service.apply_agent_provider_streaming_say_transition(
        &agent_id,
        &turn.turn_id,
        "%1",
        &mez_agent::StreamingSayEvent::Started {
            action_index: 1,
            status: mez_agent::SayStatus::Progress,
            content_type: "text/plain; charset=utf-8".to_string(),
        },
    );
    service.apply_agent_provider_streaming_say_transition(
        &agent_id,
        &turn.turn_id,
        "%1",
        &mez_agent::StreamingSayEvent::TextDelta {
            action_index: 1,
            text: "new action only".to_string(),
        },
    );
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let rendered = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(rendered.contains("new action only"), "{rendered}");
    assert!(!rendered.contains("discarded prefix"), "{rendered}");
    assert!(!rendered.contains("suppressed suffix"), "{rendered}");
}

/// Verifies that pane split actions which cannot fit inside the active window
/// become transient status-line errors instead of escaping as runtime errors.
/// The failing action must be consumed with no partial pane/process side
/// effects, and the next action while the error is visible must only dismiss
/// the presentational error instead of replaying the same split request.
#[test]
fn runtime_attached_split_error_is_presentational_and_not_replayed_on_dismiss() {
    let mut service = test_runtime_service_with_size(Size::new(3, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(3, 8).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::SplitPaneVertical,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(
        service
            .primary_error_status_overlay()
            .is_some_and(|message| message.contains("cannot split vertically")),
        "{:?}",
        service.primary_error_status_overlay()
    );

    let dismiss = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(dismiss.mux_actions_applied, 0);
    assert!(dismiss.view_refresh_required);
    assert!(dismiss.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(service.primary_error_status_overlay().is_none());

    let retried = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(retried.mux_actions_applied, 0);
    assert!(service.primary_error_status_overlay().is_some());
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
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
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "alpha beta gamma delta epsilon",
            mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("▐ mez> alpha beta gamma"), "{pane_text}");
    assert!(pane_text.contains("▐      delta epsilon"), "{pane_text}");
}

/// Verifies every published cumulative Markdown and diff prefix is identical
/// to a fresh static render of the same source snapshot.
///
/// Later Markdown fragments may reinterpret prior rows as Setext headings or
/// tables, while a unified diff becomes progressively more structured. Each
/// generation must replace the whole provisional component through the
/// ordinary renderer so no literal tail or stale styling survives.
#[test]
fn runtime_streaming_say_prefixes_match_static_rich_renderers() {
    let cases = [
        (
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
            vec![
                "Heading",
                "\n---",
                "\n\n| Name | Value |",
                "\n| --- | --- |",
                "\n| alpha | beta |",
            ],
        ),
        (
            mez_agent::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE,
            vec![
                "diff --git a/demo.rs b/demo.rs\n",
                "--- a/demo.rs\n",
                "+++ b/demo.rs\n",
                "@@ -1 +1 @@\n",
                "-old\n",
                "+new\n",
            ],
        ),
    ];

    for (case_index, (content_type, fragments)) in cases.into_iter().enumerate() {
        let mut streaming = test_runtime_service();
        streaming
            .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
            .unwrap();
        streaming
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut streaming,
            "%1",
            TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
        );
        streaming
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-prefix",
                &mez_agent::StreamingSayEvent::Started {
                    action_index: 0,
                    status: mez_agent::SayStatus::Progress,
                    content_type: content_type.to_string(),
                },
            )
            .unwrap();

        let mut source = String::new();
        for fragment in fragments {
            source.push_str(fragment);
            streaming
                .apply_agent_streaming_say_event_to_terminal_buffer(
                    "%1",
                    "turn-prefix",
                    &mez_agent::StreamingSayEvent::TextDelta {
                        action_index: 0,
                        text: fragment.to_string(),
                    },
                )
                .unwrap();
            let work = streaming
                .take_agent_streaming_say_projection_work("%1", "turn-prefix")
                .unwrap()
                .expect("each non-empty source prefix should be dirty");
            let projection = RuntimeSessionService::build_agent_streaming_say_projection(work)
                .expect("each cumulative source prefix should render");
            assert!(
                streaming
                    .apply_agent_streaming_say_projection_result(projection)
                    .unwrap(),
                "case {case_index} prefix {source:?} should install"
            );

            let mut static_render = test_runtime_service();
            static_render
                .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
                .unwrap();
            set_agent_pane_screen_for_test(
                &mut static_render,
                "%1",
                TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
            );
            static_render
                .append_agent_assistant_content_to_terminal_buffer("%1", &source, content_type)
                .unwrap();

            assert_eq!(
                streaming
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines(),
                static_render
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines(),
                "case {case_index} prefix {source:?} display must match static rendering"
            );
            assert_eq!(
                streaming
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_styled_content_lines(),
                static_render
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_styled_content_lines(),
                "case {case_index} prefix {source:?} styles must match static rendering"
            );
        }
    }
}

/// Verifies streamed Markdown is the canonical assistant presentation rather
/// than a bounded preview that is replayed after validated completion.
///
/// The prefix must exist before source text arrives, cumulative Markdown must
/// render richly before its source string closes, exact reconciliation must
/// persist the raw source once, and ordinary completion presentation must not
/// append a duplicate assistant block.
#[test]
fn runtime_streaming_say_promotes_rich_output_without_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-say-promotion"));
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    let started = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(started.contains("mez>"), "{started}");

    let source = "**streamed** output";
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: source.to_string(),
            },
        )
        .unwrap();
    let projection_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("incomplete streamed source should produce projection work");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(projection_work)
        .expect("incomplete streamed source should render off actor");
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap(),
        "current incomplete projection should install atomically"
    );
    let rendered_before_completion = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let rendered_text = rendered_before_completion.join("\n");
    assert!(rendered_text.contains("streamed output"), "{rendered_text}");
    assert!(!rendered_text.contains("**streamed**"), "{rendered_text}");
    let streamed_line = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("streamed output"))
        .expect("streamed Markdown line should be visible");
    assert!(!streamed_line.style_spans.is_empty(), "{streamed_line:?}");
    let projection_before_completion = service.agent_pane_screen("%1").unwrap().clone();

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .is_none(),
        "completion without new source must not request another projection"
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap(),
        &projection_before_completion,
        "completion without new source must not alter the visible generation"
    );

    let action = mez_agent::AgentAction {
        id: "say-streamed".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: source.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: source.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: String::new(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![action],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    assert_eq!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap(),
        std::collections::BTreeSet::from([0])
    );
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    assert_eq!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        rendered_before_completion
    );
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    let matching = entries
        .iter()
        .filter(|entry| entry.source_text.as_deref() == Some(source))
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1, "{entries:?}");
    assert_eq!(
        matching[0].source_content_type.as_deref(),
        Some(mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE)
    );
}

/// Verifies validated provider completion finalizes streamed say rows in place.
///
/// Production MAAP batches carry a non-empty batch rationale and one result per
/// action. Completion must preserve the streamed assistant block, persist it
/// once, and apply the same final styling as the static renderer without
/// appending a second copy below the provisional rows.
#[tokio::test]
async fn runtime_streaming_say_completion_does_not_append_final_duplicate() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-say-finalization"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "stream the final response")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let rationale = "Report the completed result";
    let source = "**streamed final** output";
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: rationale.to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: source.to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("complete streamed source should project"),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let streamed_line = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("streamed final"))
        .expect("streamed assistant row should be visible");

    let action = mez_agent::AgentAction {
        id: "say-streamed".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: source.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: source.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: rationale.to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action.clone()],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec![source.to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    let transition = service
        .apply_agent_provider_completed_transition(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();

    assert!(transition.applied);
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::PaneOutput,
            ..
        }
    )));
    assert!(transition.side_effects.iter().all(|effect| !matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    )));
    let final_screen = service.agent_pane_screen("%1").unwrap();
    let final_lines = final_screen.normal_content_lines();
    assert_eq!(
        final_lines
            .iter()
            .filter(|line| line.contains("streamed final"))
            .count(),
        1,
        "{final_lines:?}"
    );
    let finalized_line = final_screen
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("streamed final"))
        .expect("finalized assistant row should remain visible");
    assert_eq!(finalized_line, streamed_line);
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some(source))
            .count(),
        1,
        "{entries:?}"
    );
}

/// Verifies interrupting a turn freezes already streamed output in the pane
/// buffer rather than restoring the screen that existed before streaming.
///
/// Provider cancellation can occur after a user-visible partial response has
/// been rendered but before an authoritative response batch is available.
/// The interruption path must retire streaming ownership so later projection
/// work cannot mutate the pane, while retaining that partial response as a
/// terminal log record followed by the stopped-turn status output.
#[test]
fn runtime_interrupted_turn_retains_partial_streamed_output_in_pane_buffer() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let turn = service
        .start_agent_prompt_turn("%1", "stream a partial response")
        .unwrap();

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: "text/plain; charset=utf-8".to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "partial streamed log".to_string(),
            },
        )
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("partial streamed output should produce projection work"),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Interrupted)
        .unwrap();

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("partial streamed log"), "{pane_text}");
    assert!(pane_text.contains("Stopped after"), "{pane_text}");
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .is_none(),
        "interrupted output must no longer have live streaming ownership"
    );
}

/// Verifies streaming projection updates retain an active agent copy viewport.
///
/// A projection replaces the backing agent terminal screen while an operator
/// may be reading older output. The retained copy-mode snapshot, viewport, and
/// selection must remain intact so the next render does not pull the operator
/// to the streaming tail.
#[test]
fn runtime_streaming_say_projection_preserves_agent_copy_mode() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 120).unwrap();
    screen.feed(b"history one\r\nhistory two\r\nhistory three\r\nhistory four\r\nhistory five");
    set_agent_pane_screen_for_test(&mut service, "%1", screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let retained_viewport = {
        let copy_mode = ensure_agent_copy_mode_for_test(&mut service, "%1");
        copy_mode.scroll_to_top();
        copy_mode
            .select_range(
                CopyPosition { line: 0, column: 0 },
                CopyPosition { line: 0, column: 7 },
            )
            .unwrap();
        (
            copy_mode.scroll_top(),
            copy_mode.selection(),
            copy_mode.visible_lines().to_vec(),
        )
    };
    service.mark_presented_surface_scrollback_copy_mode("%1");

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .active_copy_mode_for_presented_surface("%1")
            .map(|copy_mode| (
                copy_mode.scroll_top(),
                copy_mode.selection(),
                copy_mode.visible_lines().to_vec(),
            )),
        Some(retained_viewport.clone())
    );
    assert!(service.presented_surface_uses_scrollback_copy_mode("%1"));

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "streaming tail".to_string(),
            },
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("streaming text should produce projection work");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work)
        .expect("streaming text should render off actor");

    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let projected_screen = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        projected_screen.contains("streaming") && projected_screen.contains("tail"),
        "{projected_screen}"
    );
    assert_eq!(
        service
            .active_copy_mode_for_presented_surface("%1")
            .map(|copy_mode| (
                copy_mode.scroll_top(),
                copy_mode.selection(),
                copy_mode.visible_lines().to_vec(),
            )),
        Some(retained_viewport)
    );
    assert!(service.presented_surface_uses_scrollback_copy_mode("%1"));
}

/// Verifies streamed rationale and command source use the existing prefixes,
/// converge through the ordinary static renderers, and remain provisional.
///
/// The completed worker projection must equal direct static thinking and
/// command-preview output at the same geometry. Validated completion then
/// restores the shared baseline so normal response presentation and shell
/// dispatch remain the only durable, executable authority.
#[test]
fn runtime_streaming_rationale_and_command_match_static_projection_and_restore() {
    let mut streaming = test_runtime_service();
    let mut static_render = test_runtime_service();
    for service in [&mut streaming, &mut static_render] {
        service
            .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        service
            .append_agent_status_text_to_terminal_buffer("%1", "baseline")
            .unwrap();
    }
    let baseline = streaming.agent_pane_screen("%1").unwrap().clone();
    let rationale = "Inspect the current files";
    let command = "printf 'alpha beta\\n'";

    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: rationale.to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 0,
            text: command.to_string(),
        },
        mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
    ] {
        streaming
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let work = streaming
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("closed rationale and command source should project");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        streaming
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    static_render
        .append_agent_thinking_text_to_terminal_buffer("%1", rationale)
        .unwrap();
    static_render
        .append_agent_command_preview_to_terminal_buffer("%1", command)
        .unwrap();
    assert_eq!(
        streaming
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        static_render
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        "completed provisional projection must match static display text"
    );
    assert_eq!(
        streaming
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines(),
        static_render
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines(),
        "completed provisional projection must match static styling"
    );

    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: rationale.to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-streamed".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: rationale.to_string(),
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
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert!(
        streaming
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    assert_eq!(streaming.agent_pane_screen("%1").unwrap(), &baseline);
}

/// Verifies exact streamed rationale and command rows become the authoritative
/// shell-action presentation without restoring or appending the preview again.
///
/// A current projection for one ready, running shell action already has final
/// wrapping and styling. Completion must preserve that screen, persist both
/// semantic sources once, dispatch the command, and request only incremental
/// pane output so the attached client never performs a full-display clear.
#[tokio::test]
async fn runtime_streaming_command_completion_promotes_without_full_redraw() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-command-promotion"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    mark_test_pane_ready(&mut service, "%1");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "print alpha beta")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let rationale = "Run the requested print command";
    let command = "printf 'alpha beta\\n'";
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: rationale.to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 0,
            text: command.to_string(),
        },
        mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
        .unwrap()
        .expect("complete rationale and command should project");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let projected_screen = service.agent_pane_screen("%1").unwrap().clone();
    let projected_command_rows = projected_screen
        .normal_content_lines()
        .into_iter()
        .filter(|line| line.contains("printf") || line.contains("alpha beta"))
        .count();

    let action = mez_agent::AgentAction {
        id: "shell-streamed".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: rationale.to_string(),
            command: command.to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut request = runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id);
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Shell);
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: rationale.to_string(),
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
        action_results: vec![mez_agent::ActionResult::running(
            &turn,
            &action,
            vec!["shell action accepted".to_string()],
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let transition = service
        .apply_agent_provider_completed_transition(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();

    assert!(transition.applied);
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::PaneOutput,
            ..
        }
    )));
    assert!(transition.side_effects.iter().all(|effect| !matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    )));
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &projected_screen);
    let settled_command_rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .into_iter()
        .filter(|line| line.contains("printf") || line.contains("alpha beta"))
        .count();
    assert_eq!(settled_command_rows, projected_command_rows);
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| {
                matches!(
                    &transaction.kind,
                    RunningShellTransactionKind::AgentAction { action_id }
                        if action_id == "shell-streamed"
                )
            })
    );

    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some(rationale))
            .count(),
        1,
        "{entries:?}"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some(command))
            .count(),
        1,
        "{entries:?}"
    );
    service.terminate_all_pane_processes().unwrap();
    drop(primary);
}

/// Verifies a rich generation captured before newer source arrived
/// cannot replace the pane or expose rows from two streaming generations.
///
/// The renderer may finish after the actor has accepted another action. The
/// stale candidate must be rejected as a whole, leaving the newer cumulative
/// projection byte-for-byte unchanged.
#[test]
fn runtime_streaming_say_rejects_stale_projection_generation_atomically() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "**old generation**".to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        )
        .unwrap();
    let stale_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("completed source should produce projection work");
    let stale_projection = RuntimeSessionService::build_agent_streaming_say_projection(stale_work)
        .expect("private stale generation should still render completely");

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 1,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 1,
                text: "new literal generation".to_string(),
            },
        )
        .unwrap();
    let latest_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("newer cumulative source should produce projection work");
    let latest_projection =
        RuntimeSessionService::build_agent_streaming_say_projection(latest_work)
            .expect("newer cumulative generation should render completely");
    assert!(
        service
            .apply_agent_streaming_say_projection_result(latest_projection)
            .unwrap(),
        "the newest cumulative generation should install atomically"
    );
    let before_stale_install = service.agent_pane_screen("%1").unwrap().clone();

    assert!(
        !service
            .apply_agent_streaming_say_projection_result(stale_projection)
            .unwrap(),
        "a prior source generation must be rejected"
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap(),
        &before_stale_install,
        "rejecting stale work must not publish any partial candidate rows"
    );
    let text = before_stale_install.normal_content_lines().join("\n");
    assert!(text.contains("old generation"), "{text}");
    assert!(!text.contains("**old generation**"), "{text}");
    assert!(text.contains("new literal generation"), "{text}");
}

/// Verifies a same-content screen replacement still fences an old worker result.
///
/// Structural screen equality cannot distinguish this ABA transition because
/// the replacement retains identical visible content and history. Runtime-owned
/// lineage must reject the old result without inspecting or recording content.
#[test]
fn runtime_streaming_say_rejects_same_content_aba_projection_lineage() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap();
    for line in 0..256 {
        screen.feed(format!("retained history {line}\r\n").as_bytes());
    }
    set_agent_pane_screen_for_test(&mut service, "%1", screen);
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-aba",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-aba",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "**same-content ABA**".to_string(),
            },
        )
        .unwrap();
    let stale_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-aba")
        .unwrap()
        .expect("streaming source should produce projection work");
    let stale_projection = RuntimeSessionService::build_agent_streaming_say_projection(stale_work)
        .expect("captured ABA generation should render completely");
    let original_lineage = service
        .agent_pane_screen_lineage("%1", &conversation_id)
        .unwrap();
    let same_content = service.agent_pane_screen("%1").unwrap().clone();

    service.set_agent_pane_screen("%1", conversation_id.clone(), same_content.clone());

    assert_ne!(
        service
            .agent_pane_screen_lineage("%1", &conversation_id)
            .unwrap(),
        original_lineage
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &same_content);
    assert!(
        !service
            .apply_agent_streaming_say_projection_result(stale_projection)
            .unwrap(),
        "an old worker result must not survive a same-content ABA replacement"
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &same_content);
    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_streaming_projection_results, 1);
    assert_eq!(metrics.agent_streaming_projection_installs, 0);
    assert_eq!(metrics.agent_streaming_projection_rejections, 1);
    assert_eq!(metrics.agent_streaming_projection_lineage_rejections, 1);
}

/// Verifies response-local MAAP action ordinals cannot append to source from a
/// preceding provider interaction in the same logical turn.
///
/// A capability continuation or repair starts its parser at action zero. The
/// ordered response barrier must retire the preceding provisional generation
/// before action zero from the new response is accepted.
#[test]
fn runtime_streaming_say_scopes_action_indices_to_provider_responses() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );

    for event in [
        mez_agent::StreamingSayEvent::ResponseStarted { response_index: 0 },
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "first response".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );

    for event in [
        mez_agent::StreamingSayEvent::ResponseStarted { response_index: 1 },
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "second response".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let second_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(second_projection)
            .unwrap()
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("second response"), "{text}");
    assert!(!text.contains("first responsesecond response"), "{text}");
}

/// Verifies an ordinary pane write revokes a streaming projection's authority
/// before a delayed worker result or rollback can replace that write.
///
/// This covers the actor-serialized form of the reported race: projection work
/// is captured, a status row is appended, and the delayed projection must be
/// rejected while later cleanup preserves the status row.
#[test]
fn runtime_streaming_say_preserves_intervening_pane_writes() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::ResponseStarted { response_index: 0 },
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**provisional**".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let delayed_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();

    service
        .append_agent_status_text_to_terminal_buffer("%1", "intervening status")
        .unwrap();
    assert!(
        !service
            .apply_agent_streaming_say_projection_result(delayed_projection)
            .unwrap()
    );
    assert!(
        !service
            .discard_agent_streaming_say_presentation("%1", Some("turn-1"))
            .unwrap()
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("intervening status"), "{text}");
    assert!(!text.contains("provisional"), "{text}");
}

/// Verifies provider projections and shell previews share one composite lineage.
///
/// Provider updates must retain independently owned shell progress, shell
/// updates must not retire provisional provider source, and discarding the
/// provider projection must restore its durable baseline while preserving the
/// still-running shell preview.
#[test]
fn runtime_streaming_say_composes_with_active_shell_preview() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable baseline")
        .unwrap();
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**provider one**".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-provider", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );

    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell".to_string(),
        action_id: "shell-1".to_string(),
        marker: "marker-1".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &["shell progress one".to_string()],
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-provider",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: " and provider two".to_string(),
            },
        )
        .unwrap();
    let second_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(second_projection)
            .unwrap()
    );
    service
        .update_agent_shell_output_preview("%1", owner, 2, &["shell progress two".to_string()])
        .unwrap();

    let composite = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        composite.contains("provider one and provider two"),
        "{composite}"
    );
    assert!(composite.contains("shell progress two"), "{composite}");
    assert!(!composite.contains("shell progress one"), "{composite}");
    assert_eq!(service.agent_shell_output_previews_for_tests("%1").len(), 1);

    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some("turn-provider"))
            .unwrap()
    );
    let restored = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(restored.contains("durable baseline"), "{restored}");
    assert!(restored.contains("shell progress two"), "{restored}");
    assert!(!restored.contains("provider one"), "{restored}");
}

/// Verifies a provider update removes a settled command tail in one projection.
///
/// A completed command intentionally remains visible until the next pane
/// content is installed. Provider streaming must be that cleanup boundary, or
/// stale terminal rows survive until later output overwrites them physically.
#[test]
fn runtime_streaming_say_retires_settled_shell_preview() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "provider one".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-provider", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );

    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell".to_string(),
        action_id: "shell-1".to_string(),
        marker: "marker-1".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &[
                "settled shell tail one".to_string(),
                "settled shell tail two".to_string(),
            ],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    let retained = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(retained.contains("settled shell tail one"), "{retained}");

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-provider",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: " and provider two".to_string(),
            },
        )
        .unwrap();
    let second_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(second_projection)
            .unwrap()
    );

    let updated = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        updated.contains("provider one and provider two"),
        "{updated}"
    );
    assert!(!updated.contains("settled shell tail one"), "{updated}");
    assert!(!updated.contains("settled shell tail two"), "{updated}");
    assert!(
        service
            .agent_shell_output_previews_for_tests("%1")
            .is_empty()
    );
}

/// Verifies streamed source is neither truncated by shell-preview settings nor
/// retained when validated completion supplies different authoritative text.
///
/// Long live output must retain its beginning and end in terminal history. A
/// later mismatch must restore the pre-stream screen so normal presentation can
/// append only the validated replacement.
#[test]
fn runtime_streaming_say_is_untruncated_and_mismatch_restores_baseline() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(32, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(32, 8).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "baseline")
        .unwrap();
    let long_source = (0..24)
        .map(|index| format!("stream-line-{index:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: long_source,
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("long cumulative source should produce projection work");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work)
        .expect("long cumulative source should render completely");
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap(),
        "the complete long-source generation should install atomically"
    );
    let streamed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(streamed.contains("stream-line-00"), "{streamed}");
    assert!(streamed.contains("stream-line-23"), "{streamed}");

    let replacement = "validated replacement";
    let action = mez_agent::AgentAction {
        id: "say-replacement".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: replacement.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: replacement.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: String::new(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![action],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    assert!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let final_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(final_text.contains("baseline"), "{final_text}");
    assert!(final_text.contains(replacement), "{final_text}");
    assert!(!final_text.contains("stream-line-00"), "{final_text}");
    assert_eq!(final_text.matches(replacement).count(), 1, "{final_text}");
}

/// Verifies resize rebuilds durable source and then reprojects active transients.
///
/// Provider source and shell progress are not durable replay entries. A width
/// change must rebuild only the persisted baseline, retain owner and revision
/// metadata, regenerate provider presentation at the new geometry, and append
/// the active shell preview once after that provider projection.
#[test]
fn runtime_agent_resize_reprojects_provider_and_shell_preview() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-transient-resize"));
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable resize baseline")
        .unwrap();
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**provider resize source**".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-provider-resize",
                &event,
            )
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider-resize")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell-resize".to_string(),
        action_id: "shell-resize".to_string(),
        marker: "marker-resize".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            7,
            &["shell resize progress".to_string()],
        )
        .unwrap();

    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(28, 12).unwrap())
            .unwrap()
    );

    let resized = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(resized.contains("durable resize baseline"), "{resized}");
    let resized_compact = resized
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '▐')
        .collect::<String>();
    assert!(
        resized_compact.contains("providerresizesource"),
        "{resized}"
    );
    assert!(resized.contains("shell resize progress"), "{resized}");
    assert_eq!(
        resized.matches("shell resize progress").count(),
        1,
        "{resized}"
    );
    let previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 1, "{previews:?}");
    assert_eq!(previews[0].0, owner);
    assert_eq!(previews[0].2, 7);
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert!(
        entries.iter().all(|entry| {
            entry.source_text.as_deref() != Some("provider resize source")
                && !entry
                    .display_lines
                    .iter()
                    .any(|line| line.contains("shell resize progress"))
        }),
        "{entries:?}"
    );
}

/// Verifies failed-transition snapshots restore transients only with exact lineage.
///
/// A matching rollback may restore its owner metadata. If an intervening pane
/// generation appears first, the same snapshot must not reattach stale provider
/// or shell projection ownership to that newer screen.
#[test]
fn runtime_agent_resume_snapshot_requires_exact_transient_lineage() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-snapshot".to_string(),
        action_id: "shell-snapshot".to_string(),
        marker: "marker-snapshot".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            3,
            &["snapshot preview".to_string()],
        )
        .unwrap();

    let matching = service.snapshot_agent_resume_presentation("%1");
    service.restore_agent_resume_presentation("%1", matching);
    let restored = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(restored.len(), 1, "{restored:?}");
    assert_eq!(restored[0].0, owner);
    assert_eq!(restored[0].2, 3);

    let stale = service.snapshot_agent_resume_presentation("%1");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut intervening = service.agent_pane_screen("%1").unwrap().clone();
    intervening.feed(b"\r\nintervening rollback row\r\n");
    service.set_agent_pane_screen("%1", conversation_id, intervening);
    service.restore_agent_resume_presentation("%1", stale);

    assert!(
        service
            .agent_shell_output_previews_for_tests("%1")
            .is_empty()
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("intervening rollback row"), "{text}");
}

/// Verifies user-visible status rows persist typed source and replay through
/// their original presentation style after a geometry-aware rebuild.
#[test]
fn runtime_agent_status_presentation_persists_typed_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-status-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: restoring durable status")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("styled-lines+json")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let replayed_compact = replayed
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("agentrestoringdurablestatus"),
        "{replayed}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies source-backed reconstruction is bounded only by terminal history,
/// not by an arbitrary number of durable presentation entries.
#[test]
fn runtime_agent_resize_reconstructs_more_than_two_hundred_presentation_entries() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-complete-reconstruction"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    for sequence in 1..=205 {
        transcript_store
            .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
                conversation_id: conversation_id.clone(),
                sequence,
                created_at_unix_seconds: sequence,
                pane_id: "%1".to_string(),
                turn_id: None,
                terminal_width: 28,
                style_names: vec!["assistant".to_string()],
                display_lines: vec![format!("entry-{sequence:03}")],
                copy_lines: vec![format!("entry-{sequence:03}")],
                ansi_text: None,
                source_text: Some(format!("entry-{sequence:03}")),
                source_content_type: Some(
                    mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                ),
            })
            .unwrap();
    }
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 500).unwrap(),
    );

    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );

    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(replayed.contains("entry-001"), "{replayed}");
    assert!(replayed.contains("entry-205"), "{replayed}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a source-backed transcript is not replayed into a viewport
/// cleared by the user. Resizing after Ctrl+L must retain the blank live pane
/// while preserving the prior agent output in scrollback.
#[test]
fn runtime_agent_presentation_resize_preserves_cleared_viewport() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-cleared-viewport"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "preserve this cleared agent viewport",
            mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
        .unwrap();

    service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .clear_visible_into_history();

    assert!(
        !service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let screen = service.agent_pane_screen("%1").unwrap();
    assert!(
        screen
            .visible_lines()
            .iter()
            .all(|line| line.trim().is_empty()),
        "{:?}",
        screen.visible_lines()
    );
    let history = screen
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        history.contains("preservethisclearedagentviewport"),
        "{history}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies user prompts persist their raw source and recompute wrapping when
/// an agent pane is rebuilt at a narrower geometry.
#[test]
fn runtime_agent_user_prompt_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-user-prompt-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", "restore this durable user prompt")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("user-prompt+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("userrestorethisdurableuserprompt"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies command previews persist their raw command and recompute their
/// syntax-aware projection when an agent pane is rebuilt at a new geometry.
#[test]
fn runtime_agent_command_preview_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-command-preview-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_command_preview_to_terminal_buffer("%1", "printf 'durable preview'")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("command-preview+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("printfdurablepreview"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies oversized command previews persist only their bounded UTF-8 source
/// projection and retain explicit truncation when replayed after a resize.
/// Presentation persistence must not turn a bounded renderer into durable
/// multi-megabyte storage or lose the omission marker at a new geometry.
#[test]
fn runtime_agent_command_preview_persists_bounded_truncated_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-command-preview-bounded"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let command = format!(
        "printf 'start {} tail-sentinel'",
        "x".repeat(2 * 1024 * 1024)
    );

    service
        .append_agent_command_preview_to_terminal_buffer("%1", &command)
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let source = entries[0].source_text.as_deref().unwrap();
    assert!(source.len() <= 16 * 1024, "stored {} bytes", source.len());
    assert!(!source.contains("tail-sentinel"), "{source}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("command-preview-truncated+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(replayed.contains("preview"), "{replayed}");
    assert!(replayed.contains("truncated"), "{replayed}");
    assert!(!replayed.contains("tail-sentinel"), "{replayed}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies action execution headers persist their semantic text and rebuild
/// through the action-header renderer at a narrower destination geometry.
#[test]
fn runtime_agent_action_header_persists_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-action-header-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "mcp-1".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "github".to_string(),
            tool: "search_issues".to_string(),
            arguments_json: r#"{"query":"durable header"}"#.to_string(),
        },
    };

    service
        .append_agent_action_execution_header_to_terminal_buffer(
            "%1",
            &action,
            "mcp call: github/search_issues args={\"query\":\"durable header\"}",
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("action-header+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("mcpcallgithubsearchissuesargsquerydurableheader"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies parent prompts persist their raw instruction and recompute wrapping
/// when a child agent pane is rebuilt at a narrower destination geometry.
#[test]
fn runtime_agent_parent_prompt_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-parent-prompt-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_parent_prompt_to_terminal_buffer("%1", "restore this parent instruction")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("parent-prompt+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("parentrestorethisparentinstruction"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies thinking-log body text retains the muted status rendition used by
/// its gutter instead of resetting to the terminal's default rendition.
///
/// Thinking lines use the rich-line presentation path without explicit body
/// spans, so this regression protects the base style inherited by unspanned
/// cells after the gutter has been rendered.
#[test]
fn runtime_agent_thinking_renders_body_as_shadow_text() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();

    service
        .append_agent_thinking_text_to_terminal_buffer("%1", "inspect the rendering path")
        .unwrap();

    let thinking_line = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("thinking: inspect the rendering path"))
        .expect("thinking log should be present in the terminal buffer");
    let body_column = thinking_line
        .text
        .find("thinking:")
        .expect("thinking log should include its label");
    assert!(
        thinking_line.style_spans.iter().any(|span| {
            body_column >= span.start
                && body_column < span.start.saturating_add(span.length)
                && span.rendition.dim
                && span.rendition.foreground
                    == Some(service.ui_theme().colors.agent_transcript_status.foreground)
        }),
        "thinking body should retain the muted status rendition: {thinking_line:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies visible thinking text persists its raw source and reflows when the
/// agent pane is rebuilt at a narrower destination geometry.
#[test]
fn runtime_agent_thinking_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-thinking-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();

    service
        .append_agent_thinking_text_to_terminal_buffer(
            "%1",
            "preserve this durable rationale across the reconstructed pane",
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("thinking+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("thinkingpreservethisdurablerationaleacrossthereconstructedpane"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies structured macro lifecycle status persists its fields and rebuilds
/// through the macro renderer at a narrower destination geometry.
#[test]
fn runtime_agent_macro_lifecycle_persists_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-macro-lifecycle-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_macro_status_to_terminal_buffer(
            "%1",
            "durable macro",
            Some(1),
            3,
            "waiting for child result",
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("macro-lifecycle+json")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("macrodurablemacro"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a geometry-aware rebuild preserves an earlier legacy snapshot
/// before replaying a later semantic entry at the destination geometry.
#[test]
fn runtime_agent_resize_keeps_legacy_snapshots_ordered_with_semantic_entries() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-mixed-presentation-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["status".to_string()],
            display_lines: vec!["agent: legacy snapshot".to_string()],
            copy_lines: vec!["agent: legacy snapshot".to_string()],
            ansi_text: None,
            source_text: None,
            source_content_type: None,
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id,
            sequence: 2,
            created_at_unix_seconds: 2,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale cached projection".to_string()],
            copy_lines: vec!["stale cached projection".to_string()],
            ansi_text: None,
            source_text: Some("# Semantic entry\n\nreflows at destination width".to_string()),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let compact = replayed
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(compact.contains("agentlegacysnapshot"), "{replayed}");
    assert!(
        compact.contains("Semanticentryreflowsatdestinationwidth"),
        "{replayed}"
    );
    assert!(
        compact.find("agentlegacysnapshot").unwrap()
            < compact
                .find("Semanticentryreflowsatdestinationwidth")
                .unwrap(),
        "{replayed}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a live width change rebuilds a source-backed agent screen instead
/// of reflowing its stale cached terminal rows. This keeps Markdown rendering
/// semantic across pane geometry changes while preserving legacy resize
/// behavior for panes that do not retain presentation source.
#[test]
fn runtime_agent_resize_rebuilds_source_backed_presentation_at_new_width() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id,
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale cached projection".to_string()],
            copy_lines: vec!["stale cached projection".to_string()],
            ansi_text: None,
            source_text: Some(
                "# Rebuilt heading\n\n- source layout changes with width".to_string(),
            ),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 12).unwrap())
        .unwrap();

    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("width change should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should rebuild at the resized width");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );

    let rebuilt = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(rebuilt.contains("Rebuiltheading"), "{rebuilt}");
    assert!(
        rebuilt.contains("sourcelayoutchangeswithwidth"),
        "{rebuilt}"
    );
    assert!(!rebuilt.contains("stalecachedprojection"), "{rebuilt}");
    let rebuilt_size = service.agent_pane_screen("%1").unwrap().size();
    assert!(
        !service
            .rebuild_agent_presentation_after_resize("%1", rebuilt_size)
            .unwrap(),
        "the installed projection should bypass repeated semantic replay"
    );
    assert_eq!(
        transcript_store
            .inspect_presentation(
                service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .as_str()
            )
            .unwrap()
            .len(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies resizing a pane after its agent session is hidden preserves the
/// shell-owned screen instead of replaying retained agent presentation.
///
/// Hidden sessions retain durable transcript records for a later resume, but
/// their pane screen belongs to the shell. A width resize must therefore use
/// ordinary terminal resizing without replacing the shell prompt.
#[test]
fn runtime_agent_resize_does_not_replay_hidden_session_over_shell_prompt() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-hidden-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale agent transcript".to_string()],
            copy_lines: vec!["stale agent transcript".to_string()],
            ansi_text: None,
            source_text: Some("# Retained agent source".to_string()),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let mut shell_screen = TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap();
    shell_screen.feed(b"distinct-shell$ ");
    service.set_pane_screen("%1", shell_screen);

    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 12).unwrap())
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("distinct-shell$"), "{pane_text}");
    assert!(!pane_text.contains("Retained agent source"), "{pane_text}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().visibility,
        AgentShellVisibility::Hidden
    );
    assert_eq!(
        transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .len(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a row-only terminal resize updates a retained hidden agent screen
/// without replacing either surface or requiring source-backed width replay.
#[test]
fn runtime_hidden_agent_screen_resizes_when_only_rows_change() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap();
    agent_screen.feed(b"retained-agent-view");
    service.set_agent_pane_screen("%1", &conversation_id, agent_screen);
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let mut process_screen = TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap();
    process_screen.feed(b"retained-process-view");
    service.set_process_pane_screen("%1", process_screen);

    service
        .resize_attached_primary_terminal(&primary, Size::new(28, 16).unwrap())
        .unwrap();

    let window = service.session().active_window().unwrap();
    let expected_process_size = service.pane_presentation_size_for(window, "%1").unwrap();
    let expected_agent_size = service.pane_process_size_for(window, "%1").unwrap();
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        expected_process_size
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().size(),
        expected_agent_size
    );
    assert!(
        service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("retained-process-view")
    );
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("retained-agent-view")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies pane-divider dragging defers expensive source-backed agent replay
/// until the resize gesture finishes at its final pane size.
///
/// Geometry and terminal sizing must still update during the drag, repeated
/// movement must coalesce into one pending semantic presentation rebuild, and
/// a debounce firing while the pointer remains held must retain that work.
#[test]
fn runtime_structural_agent_resize_does_not_read_presentation_history_inline() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-structural-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Deferred structural rebuild\n\nsemantic source must be read off actor",
        )
        .unwrap();
    let presentation_path = transcript_store
        .presentation_path(&conversation_id)
        .unwrap();
    std::fs::write(&presentation_path, b"not a presentation stream").unwrap();

    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    assert_eq!(service.session().active_window().unwrap().panes().len(), 2);
    service.terminate_all_pane_processes().unwrap();
}

/// Builds one delayed canonical resize result together with its live owner.
///
/// Each stale-result regression mutates exactly one actor-owned input after
/// capture, then verifies that atomic installation rejects the old candidate.
fn delayed_agent_resize_result_for_test(
    name: &str,
) -> (
    RuntimeSessionService,
    crate::runtime::RuntimeAgentPresentationResizeResult,
) {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root(name));
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Delayed resize\n\ncanonical source must not overwrite newer state",
        )
        .unwrap();
    assert!(
        service
            .apply_pane_resize_completion_event("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("width change should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should build a delayed resize result");
    (service, result)
}

/// Verifies delayed canonical resize results cannot overwrite newer output,
/// geometry, conversation, theme, viewport, or pane-lifecycle state.
#[test]
fn runtime_agent_resize_projection_rejects_every_stale_owner_generation() {
    let (mut output_service, output_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-output");
    output_service
        .append_agent_status_text_to_terminal_buffer("%1", "newer pane output")
        .unwrap();
    assert!(
        !output_service
            .apply_agent_presentation_resize_result(output_result)
            .unwrap()
    );

    let (mut geometry_service, geometry_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-geometry");
    geometry_service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .resize(Size::new(18, 12).unwrap());
    assert!(
        !geometry_service
            .apply_agent_presentation_resize_result(geometry_result)
            .unwrap()
    );

    let (mut conversation_service, conversation_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-conversation");
    conversation_service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    assert!(
        !conversation_service
            .apply_agent_presentation_resize_result(conversation_result)
            .unwrap()
    );

    let (mut theme_service, theme_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-theme");
    theme_service.set_ui_theme_for_tests(mez_mux::theme::deepforest_ui_theme());
    assert!(
        !theme_service
            .apply_agent_presentation_resize_result(theme_result)
            .unwrap()
    );

    let (mut viewport_service, viewport_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-viewport");
    viewport_service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .clear_visible_into_history();
    assert!(
        !viewport_service
            .apply_agent_presentation_resize_result(viewport_result)
            .unwrap()
    );

    let (mut removed_service, removed_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-pane-removal");
    removed_service.agent_shell_store_mut().remove_session("%1");
    removed_service.remove_agent_pane_screen("%1");
    assert!(
        !removed_service
            .apply_agent_presentation_resize_result(removed_result)
            .unwrap()
    );
}

/// Verifies resize replay caches decoded durable entries across widths, reuses
/// an exact canonical snapshot when a prior width returns, and invalidates both
/// layers after new durable presentation source is appended.
#[test]
fn runtime_agent_resize_projection_reuses_bounded_canonical_cache() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-replay-cache"));
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Cached resize\n\ncanonical source is replayed once per new width",
        )
        .unwrap();

    let settle_resize = |service: &mut RuntimeSessionService, size: Size| {
        assert!(
            service
                .apply_pane_resize_completion_event("%1", size)
                .unwrap()
        );
        let work = service
            .take_agent_presentation_resize_work("%1")
            .unwrap()
            .expect("width change should expose canonical resize work");
        let result = RuntimeSessionService::build_agent_presentation_resize(work)
            .unwrap()
            .expect("semantic source should produce a canonical projection");
        assert!(
            service
                .apply_agent_presentation_resize_result(result)
                .unwrap()
        );
    };

    let narrow = Size::new(20, 12).unwrap();
    settle_resize(&mut service, narrow);
    let narrow_screen = service.agent_pane_screen("%1").unwrap().clone();
    settle_resize(&mut service, Size::new(24, 12).unwrap());
    settle_resize(&mut service, narrow);

    assert_eq!(service.agent_pane_screen("%1"), Some(&narrow_screen));
    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_presentation_decoded_cache_misses, 1);
    assert_eq!(metrics.agent_presentation_decoded_cache_hits, 2);
    assert_eq!(metrics.agent_presentation_snapshot_cache_hits, 1);
    assert_eq!(metrics.agent_presentation_snapshot_cache_misses, 2);
    assert_eq!(metrics.agent_presentation_replayed_entries, 2);

    service
        .append_agent_status_text_to_terminal_buffer("%1", "new durable source")
        .unwrap();
    settle_resize(&mut service, Size::new(22, 12).unwrap());

    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_presentation_decoded_cache_misses, 2);
    assert_eq!(metrics.agent_presentation_snapshot_cache_hits, 1);
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("new durable source"), "{text}");
}

/// Verifies adapter-owned presentation writes fence resize replay until the
/// conversation-specific persistence settlement makes the durable prefix authoritative.
#[test]
fn runtime_agent_resize_projection_waits_for_presentation_persistence() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-pending-source"));
    service.set_agent_transcript_store(transcript_store.clone());
    service.persistence.enable_transcript_adapter();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Settled resize\n\ncanonical replay waits for durable presentation source",
        )
        .unwrap();

    assert!(
        service
            .apply_pane_resize_completion_event("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    assert!(
        service
            .take_agent_presentation_resize_work("%1")
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );

    let mut presentation_path = None;
    let mut entries = Vec::new();
    for effect in service
        .drain_transcript_persistence_transition()
        .side_effects
    {
        if let RuntimeSideEffect::PersistPresentationEntries {
            path,
            entries: queued,
            ..
        } = effect
        {
            presentation_path = Some(path);
            entries.extend(queued);
        }
    }
    assert!(!entries.is_empty());
    let entry_count = entries.len();
    let bytes = transcript_store.append_presentation_many(&entries).unwrap();
    let transition = service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationCompleted {
            conversation_id,
            path: presentation_path.expect("presentation write should expose its durable path"),
            entries: entry_count,
            bytes,
        })
        .unwrap();
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::DispatchAgentPresentationResize { pane_id, .. } if pane_id == "%1"
    )));

    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("settled presentation persistence should release deferred resize work");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("settled semantic source should build a canonical projection");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(text.contains("Settledresize"), "{text}");
    assert!(
        text.contains("canonicalreplaywaitsfordurablepresentationsource"),
        "{text}"
    );
}

/// Verifies width snapshots obey fixed entry and memory bounds, evict the
/// least-recently-used widths, and continue reusing decoded durable source.
#[test]
fn runtime_agent_resize_projection_cache_evicts_old_widths_within_bounds() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-cache-eviction"));
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Bounded cache\n\nrepeated widths retain canonical source",
        )
        .unwrap();

    for columns in 20..=27 {
        let size = Size::new(columns, 12).unwrap();
        assert!(
            service
                .apply_pane_resize_completion_event("%1", size)
                .unwrap()
        );
        let work = service
            .take_agent_presentation_resize_work("%1")
            .unwrap()
            .expect("new width should expose resize work");
        let result = RuntimeSessionService::build_agent_presentation_resize(work)
            .unwrap()
            .expect("semantic source should build a canonical width");
        assert!(
            service
                .apply_agent_presentation_resize_result(result)
                .unwrap()
        );
    }

    let (decoded, snapshots, estimated_bytes) =
        service.agent_presentation_replay_cache_stats_for_tests();
    assert_eq!(decoded, 1);
    assert!(
        snapshots <= 6,
        "snapshot cache retained {snapshots} entries"
    );
    assert!(
        estimated_bytes <= 64 * 1024 * 1024,
        "cache retained {estimated_bytes} bytes"
    );
    let metrics = service.runtime_metrics();
    assert!(metrics.agent_presentation_cache_evictions >= 2);
    assert_eq!(metrics.agent_presentation_decoded_cache_misses, 1);
    assert_eq!(metrics.agent_presentation_decoded_cache_hits, 7);
}

/// Verifies pane-divider dragging defers expensive source-backed agent replay
/// until the resize gesture finishes at its final pane size.
///
/// Geometry and terminal sizing must still update during the drag, repeated
/// movement must coalesce into one pending semantic presentation rebuild, and
/// a debounce firing while the pointer remains held must retain that work.
#[test]
fn runtime_agent_divider_drag_debounces_source_backed_presentation_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-drag-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Deferred rebuild\n\nsemantic source uses the final drag width",
        )
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );

    let border = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .mouse_border_cells
        .into_iter()
        .next()
        .expect("vertical split should expose a draggable divider");
    for column in [
        border.column,
        border.column.saturating_add(2),
        border.column.saturating_add(4),
    ] {
        let (_, transition) = service
            .apply_attached_terminal_step_transition(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(
                        MouseAction::ResizePane {
                            column,
                            row: border.row,
                        },
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
        assert!(transition.side_effects.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::RenderClient {
                reason: RenderInvalidationReason::ResizeDrag,
                ..
            }
        )));
    }

    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    let intermediate = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!intermediate.contains("Deferred rebuild"), "{intermediate}");
    let final_size = service.agent_pane_screen("%1").unwrap().size();

    let transition = service
        .apply_resize_debounce_timer_transition(true)
        .unwrap();

    assert!(!transition.applied);
    assert!(transition.side_effects.is_empty());
    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap().size(), final_size);
    let still_deferred = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !still_deferred.contains("Deferred rebuild"),
        "{still_deferred}"
    );

    let (release, release_transition) = service
        .apply_attached_terminal_step_transition(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::FinishResizePane,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(release.view_refresh_required);
    assert!(release.full_redraw_required);
    assert!(
        release_transition
            .side_effects
            .iter()
            .any(|effect| matches!(
                effect,
                RuntimeSideEffect::RenderClient {
                    reason: RenderInvalidationReason::FullRedraw,
                    ..
                }
            ))
    );
    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("released drag should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should build a canonical resize generation");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );
    assert!(
        !service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    let rebuilt = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(rebuilt.contains("Deferredrebuild"), "{rebuilt}");
    assert!(
        rebuilt.contains("semanticsourceusesthefinaldragwidth"),
        "{rebuilt}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an asynchronous PTY resize completion rebuilds source-backed agent
/// presentation instead of resizing the stale terminal-cell projection.
#[test]
fn runtime_agent_async_resize_completion_rebuilds_source_backed_presentation() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-async-resize-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id,
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale async projection".to_string()],
            copy_lines: vec!["stale async projection".to_string()],
            ansi_text: None,
            source_text: Some("# Async rebuild\n\nsource survives completion resize".to_string()),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    assert!(
        service
            .apply_pane_resize_completion_event("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );

    let provisional = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!provisional.contains("Async rebuild"), "{provisional}");
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("resize completion should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should build a canonical resize generation");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );

    let rebuilt = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(rebuilt.contains("Asyncrebuild"), "{rebuilt}");
    assert!(
        rebuilt.contains("sourcesurvivescompletionresize"),
        "{rebuilt}"
    );
    assert!(!rebuilt.contains("staleasyncprojection"), "{rebuilt}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a stale adapter-owned resize completion cannot overwrite the
/// newest queued pane geometry or its source-backed agent projection.
#[test]
fn runtime_agent_ignores_superseded_async_resize_completion() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-stale-async-resize"));
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let _process = service.take_running_pane_process_for_adapter("%1").unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "semantic projection survives only the newest resize completion",
        )
        .unwrap();

    service
        .resize_attached_primary_terminal(&primary, Size::new(24, 12).unwrap())
        .unwrap();
    let stale_size = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::Resize { size },
                ..
            } => Some(size),
            _ => None,
        })
        .unwrap();
    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 12).unwrap())
        .unwrap();
    let newest_size = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::Resize { size },
                ..
            } => Some(size),
            _ => None,
        })
        .unwrap();
    assert_ne!(stale_size, newest_size);
    assert!(
        !service
            .apply_pane_resize_completion_event("%1", stale_size)
            .unwrap()
    );
    assert!(
        service
            .apply_pane_resize_completion_event("%1", newest_size)
            .unwrap()
    );
    assert_eq!(service.pane_screen("%1").unwrap().size(), newest_size);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider-produced Markdown tables persist their semantic source and
/// redraw through the attached client after a production resize path changes
/// the pane geometry.
#[test]
fn runtime_provider_markdown_table_persists_and_reprojects_after_resize() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("provider-table-projection"));
    let primary = service
        .attach_primary("primary", true, Size::new(48, 16).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"provider-table","method":"agent/shell/command","params":{"idempotency_key":"provider-table","input":"render a wide table"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let table = "| Component | Durable projection detail |\n| --- | --- |\n| renderer | semantic table cells reflow at the destination pane width |\n| resume | persisted source redraws after restoring a conversation |";
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: table.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "render the requested table".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-table".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: table.to_string(),
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

    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert!(
        entries.iter().any(|entry| {
            entry.source_text.as_deref() == Some(table)
                && entry.source_content_type.as_deref()
                    == Some(mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE)
        }),
        "{entries:?}"
    );

    let wide_projection = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    service
        .resize_attached_primary_terminal(&primary, Size::new(24, 16).unwrap())
        .unwrap();
    let narrow_projection = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert_ne!(
        wide_projection, narrow_projection,
        "wide={wide_projection:?} narrow={narrow_projection:?}"
    );
    assert!(
        narrow_projection.iter().any(|line| line.contains('│')),
        "{narrow_projection:?}"
    );
    service
        .resize_attached_primary_terminal(&primary, Size::new(48, 16).unwrap())
        .unwrap();
    assert_eq!(
        transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .len(),
        entries.len()
    );
    service.terminate_all_pane_processes().unwrap();
}
