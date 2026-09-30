//! Runtime tests for agent presentation terminal ui behavior.

use super::*;
use crate::runtime::{RenderInvalidationReason, RuntimeTransition};

mod command_promotion;
mod peer_messages;
mod resize_admission;
mod streaming_identity;

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
            .any(|line| line.text.starts_with("▐      recovery")),
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

/// Verifies snapshot-only structured presentation rows rewrap agent and
/// thinking labels with the same fixed indent as semantic presentation.
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
    let thinking = "thinking: alpha beta gamma";
    let entry = crate::storage::transcript::AgentPresentationEntry {
        conversation_id,
        sequence: 1,
        created_at_unix_seconds: 1,
        pane_id: "%1".to_string(),
        turn_id: None,
        terminal_width: 80,
        style_names: vec!["status".to_string(), "status".to_string()],
        display_lines: vec![source.to_string(), thinking.to_string()],
        copy_lines: vec![source.to_string(), thinking.to_string()],
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
    assert!(
        rows.iter()
            .any(|line| line.starts_with("▐      structured")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|line| line == "▐ thinking: alpha beta"),
        "{rows:?}"
    );
    assert!(rows.iter().any(|line| line == "▐      gamma"), "{rows:?}");
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

mod source_ordering;

mod event_replay;

/// Static/streamed parity, promotion, and interrupted-output retention.
mod say_promotion {
    use super::*;

    /// A complete later say remains buffered behind an open command preview and
    /// becomes visible only after the command field closes.
    #[test]
    fn runtime_streaming_command_closure_releases_later_say() {
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
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
        for event in [
            mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
            mez_agent::StreamingSayEvent::ShellCommandTextDelta {
                action_index: 0,
                text: "printf first".to_string(),
            },
            mez_agent::StreamingSayEvent::Started {
                action_index: 1,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 1,
                text: "later answer".to_string(),
            },
            mez_agent::StreamingSayEvent::TextComplete { action_index: 1 },
            mez_agent::StreamingSayEvent::ActionComplete { action_index: 1 },
        ] {
            service
                .ingest_provider_log(
                    "%1",
                    "turn-command-order",
                    crate::runtime::RuntimeProviderLogInput::Progress(&event),
                )
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-command-order")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        let before = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(before.contains("printf first"), "{before}");
        assert!(!before.contains("later answer"), "{before}");
        service
            .ingest_provider_log(
                "%1",
                "turn-command-order",
                crate::runtime::RuntimeProviderLogInput::Progress(
                    &mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
                ),
            )
            .unwrap();
        assert!(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-command-order")
                .unwrap()
                .is_none(),
            "command field closure is not whole-action receipt"
        );
        service
            .ingest_provider_log(
                "%1",
                "turn-command-order",
                crate::runtime::RuntimeProviderLogInput::Progress(
                    &mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
                ),
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-command-order")
            .unwrap()
            .expect("command receipt must release buffered answer");
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        let after = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(after.contains("printf first"), "{after}");
        assert!(
            !after.contains("later answer"),
            "command receipt alone cannot finalize its preview: {after}"
        );
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
        let projection =
            RuntimeSessionService::build_agent_streaming_say_projection(projection_work)
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
                    rationale: String::new(),

                    actions: vec![action],
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
        service
            .append_agent_status_text_to_terminal_buffer("%1", "later durable row")
            .unwrap();
        let after_append = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        assert_eq!(
            after_append
                .iter()
                .filter(|line| line.text.contains("streamed output"))
                .count(),
            1,
            "{after_append:?}"
        );
        assert!(
            after_append
                .iter()
                .any(|line| line.text.contains("later durable row")),
            "{after_append:?}"
        );
    }

    /// Verifies a newer cumulative source generation that renders identically does
    /// not replace the pane screen or request an attached-client redraw.
    ///
    /// A shell summary can repeat the batch rationale exactly. The source revision
    /// still advances for reconciliation, but filtering the duplicate thinking row
    /// leaves the rendered generation unchanged and must preserve screen lineage.
    #[test]
    fn runtime_streaming_identical_projection_is_a_screen_noop() {
        let mut service = test_runtime_service();
        let conversation_id = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        let thinking = "Inspect the streaming compositor";
        for event in [
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: thinking.to_string(),
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
        let screen = service.agent_pane_screen("%1").unwrap().clone();
        let lineage = service
            .agent_pane_screen_lineage("%1", &conversation_id)
            .unwrap();

        for event in [
            mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 },
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
                action_index: 0,
                text: thinking.to_string(),
            },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
                .unwrap();
        }
        let duplicate_projection = RuntimeSessionService::build_agent_streaming_say_projection(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-1")
                .unwrap()
                .unwrap(),
        )
        .unwrap();

        assert!(
            !service
                .apply_agent_streaming_say_projection_result(duplicate_projection)
                .unwrap(),
            "an identical screen generation must not request physical output"
        );
        assert_eq!(service.agent_pane_screen("%1").unwrap(), &screen);
        assert_eq!(
            service.agent_pane_screen_lineage("%1", &conversation_id),
            Some(lineage),
            "a no-op projection must preserve screen lineage"
        );
        let metrics = service.runtime_metrics();
        assert_eq!(metrics.agent_streaming_projection_results, 2);
        assert_eq!(metrics.agent_streaming_projection_installs, 1);
        assert_eq!(metrics.agent_streaming_projection_rejections, 0);
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
                    rationale: rationale.to_string(),

                    actions: vec![action.clone()],
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

    /// The provider-neutral ingestion boundary must settle a validated batch to
    /// the same styled and persisted answer with or without optional fragments.
    #[test]
    fn runtime_validated_say_settlement_matches_with_and_without_progress() {
        let rationale = "Distinct validated rationale without fragments";
        let later = "Later validated action without fragments";
        for (case, content_type, source) in [
            (
                "plain",
                mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
                "same validated answer",
            ),
            (
                "markdown",
                mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
                "**same** validated answer",
            ),
            (
                "diff",
                mez_agent::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE,
                "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new",
            ),
        ] {
            let mut settled = Vec::new();
            for streamed_rationale in [false, true] {
                for streamed in [false, true] {
                    let mut service = test_runtime_service();
                    let store = AgentTranscriptStore::new(temp_root(&format!(
                        "mode-parity-{case}-{}-{}",
                        if streamed { "streamed" } else { "complete" },
                        if streamed_rationale {
                            "rationale"
                        } else {
                            "no-rationale"
                        }
                    )));
                    service.set_agent_transcript_store(store.clone());
                    service
                        .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
                        .unwrap();
                    let conversation_id = service
                        .agent_shell_store_mut()
                        .enter_or_resume("%1")
                        .unwrap()
                        .session_id
                        .clone();
                    set_agent_pane_screen_for_test(
                        &mut service,
                        "%1",
                        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
                    );
                    if streamed_rationale {
                        for event in [
                            mez_agent::StreamingSayEvent::RationaleStarted,
                            mez_agent::StreamingSayEvent::RationaleTextDelta {
                                text: rationale.to_string(),
                            },
                            mez_agent::StreamingSayEvent::RationaleTextComplete,
                        ] {
                            service
                                .ingest_provider_log(
                                    "%1",
                                    "turn-1",
                                    crate::runtime::RuntimeProviderLogInput::Progress(&event),
                                )
                                .unwrap();
                        }
                    }
                    if streamed {
                        for event in [
                            mez_agent::StreamingSayEvent::Started {
                                action_index: 0,
                                status: mez_agent::SayStatus::Final,
                                content_type: content_type.to_string(),
                            },
                            mez_agent::StreamingSayEvent::TextDelta {
                                action_index: 0,
                                text: source.to_string(),
                            },
                            mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
                            mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
                        ] {
                            service
                                .ingest_provider_log(
                                    "%1",
                                    "turn-1",
                                    crate::runtime::RuntimeProviderLogInput::Progress(&event),
                                )
                                .unwrap();
                        }
                        let projection =
                            RuntimeSessionService::build_agent_streaming_say_projection(
                                service
                                    .take_agent_streaming_say_projection_work("%1", "turn-1")
                                    .unwrap()
                                    .unwrap(),
                            )
                            .unwrap();
                        service
                            .apply_agent_streaming_say_projection_result(projection)
                            .unwrap();
                    }
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
                                rationale: rationale.to_string(),
                                actions: vec![
                                    mez_agent::AgentAction {
                                        id: "answer".to_string(),
                                        payload: mez_agent::AgentActionPayload::Say {
                                            status: mez_agent::SayStatus::Final,
                                            text: source.to_string(),
                                            content_type: content_type.to_string(),
                                        },
                                    },
                                    mez_agent::AgentAction {
                                        id: "later".to_string(),
                                        payload: mez_agent::AgentActionPayload::Say {
                                            status: mez_agent::SayStatus::Final,
                                            text: later.to_string(),
                                            content_type:
                                                mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                                    .to_string(),
                                        },
                                    },
                                ],
                            }),
                            provider_transcript_events: Vec::new(),
                        },
                        latest_response_usage: Default::default(),
                        routing_token_usage_by_model: Default::default(),
                        action_results: Vec::new(),
                        final_turn: true,
                        terminal_state: AgentTurnState::Completed,
                    };
                    service
                        .ingest_provider_log(
                            "%1",
                            "turn-1",
                            crate::runtime::RuntimeProviderLogInput::Validated(&execution),
                        )
                        .unwrap();
                    service
                        .ingest_provider_log(
                            "%1",
                            "turn-1",
                            crate::runtime::RuntimeProviderLogInput::Settled(&execution),
                        )
                        .unwrap();
                    service
                        .ingest_provider_log(
                            "%1",
                            "turn-1",
                            crate::runtime::RuntimeProviderLogInput::Settled(&execution),
                        )
                        .unwrap();
                    let rows = service
                        .agent_pane_screen("%1")
                        .unwrap()
                        .normal_styled_content_lines();
                    let entries = store.inspect_presentation(&conversation_id).unwrap();
                    assert_eq!(
                        entries
                            .iter()
                            .filter(|entry| entry.source_text.as_deref() == Some(rationale))
                            .count(),
                        1,
                        "{case}: {entries:?}"
                    );
                    assert_eq!(
                        entries
                            .iter()
                            .filter(|entry| entry.source_text.as_deref() == Some(source))
                            .count(),
                        1
                    );
                    assert_eq!(
                        entries
                            .iter()
                            .filter(|entry| entry.source_text.as_deref() == Some(later))
                            .count(),
                        1,
                        "{case}: {entries:?}"
                    );
                    assert_eq!(
                        entries
                            .iter()
                            .filter_map(|entry| entry.source_text.as_deref())
                            .filter(|text| [rationale, source, later].contains(text))
                            .collect::<Vec<_>>(),
                        vec![rationale, source, later],
                        "{case}: {entries:?}"
                    );
                    settled.push((
                        rows,
                        entries
                            .into_iter()
                            .filter(|entry| {
                                [source, rationale, later]
                                    .contains(&entry.source_text.as_deref().unwrap_or(""))
                            })
                            .map(|entry| (entry.display_lines, entry.copy_lines))
                            .collect::<Vec<_>>(),
                    ));
                    // A later request in the same turn is a distinct response even
                    // if it repeats the same provider-authored batch verbatim.
                    let mut continuation = execution.clone();
                    continuation.request.messages.push(mez_agent::ModelMessage {
                        role: mez_agent::ModelMessageRole::User,
                        source: mez_agent::ContextSourceKind::UserInstruction,
                        placement: mez_agent::ContextPlacement::ConversationAppend,
                        content: "new provider request chronology".to_string(),
                    });
                    service
                        .ingest_provider_log(
                            "%1",
                            "turn-1",
                            crate::runtime::RuntimeProviderLogInput::Settled(&continuation),
                        )
                        .unwrap();
                    let continued = store.inspect_presentation(&conversation_id).unwrap();
                    assert_eq!(
                        continued
                            .iter()
                            .filter(|entry| entry.source_text.as_deref() == Some(source))
                            .count(),
                        2,
                        "{case}: {continued:?}"
                    );
                }
            }
            for actual in &settled[1..] {
                assert_eq!(&settled[0], actual, "{case}");
            }
        }
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
}

/// Sender-side presentation remains provisional until message acceptance.
mod outbound_messages {
    use super::*;

    /// Verifies a presentation-eligible outbound message appears before provider
    /// completion, retains its requested recipient marker, and obeys the active
    /// transcript width cap.
    #[test]
    fn runtime_streaming_outbound_message_projects_before_completion() {
        let mut service = test_runtime_service();
        service.set_subagent_lineage(
            "agent-%1",
            RuntimeSubagentLineage {
                parent_agent_id: "agent-root".to_string(),
                root_agent_id: "agent-root".to_string(),
                depth: 1,
                display_name: "child".to_string(),
                terminal: false,
            },
        );
        service.set_subagent_lineage(
            "agent-%2",
            RuntimeSubagentLineage {
                parent_agent_id: "agent-root".to_string(),
                root_agent_id: "agent-root".to_string(),
                depth: 1,
                display_name: "nonhuman".to_string(),
                terminal: false,
            },
        );
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
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
        );
        let turn = service
            .start_agent_prompt_turn("%1", "stream an outbound message")
            .unwrap();

        for event in [
            mez_agent::StreamingSayEvent::MessageStarted {
                action_index: 0,
                recipient: "agent-%2".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadDelta {
                action_index: 0,
                text: "partial outbound payload with averyveryverylongtoken".to_string(),
            },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(
            service
                .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                .unwrap()
                .expect("partial outbound message should produce projection work"),
        )
        .unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );

        let lines = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(
            lines.iter().any(|line| line.contains("nonhuman<")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("partial")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .filter(|line| !line.trim().is_empty())
                .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 24),
            "{lines:?}"
        );
    }

    /// A validated say is permanent even while a later outbound message retains
    /// response ownership for its separate delivery settlement.
    #[test]
    fn runtime_promoted_say_survives_pending_message_and_durable_append() {
        for resize in [false, true] {
            for accepted in [None, Some(false), Some(true)] {
                promoted_say_pending_message_case(resize, accepted);
            }
        }
    }

    /// Checks the pending-message baseline both with and without a resize replay.
    fn promoted_say_pending_message_case(resize: bool, accepted: Option<bool>) {
        let mut service = test_runtime_service();
        service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
            "promoted-say-pending-message",
        )));
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
        );
        let turn = service
            .start_agent_prompt_turn("%1", "report then send")
            .unwrap();
        let answer = mez_agent::AgentAction {
            id: "answer".to_string(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Progress,
                text: "permanent sibling".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        let message = mez_agent::AgentAction {
            id: "message".to_string(),
            payload: mez_agent::AgentActionPayload::SendMessage {
                recipient: "agent-%2".to_string(),
                scope: None,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                payload: "pending payload".to_string(),
                correlation_id: None,
            },
        };
        let trailing = mez_agent::AgentAction {
            id: "trailing".to_string(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Progress,
                text: "unpromoted trailing say".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        for event in [
            mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "permanent sibling".to_string(),
            },
            mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
            mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
            mez_agent::StreamingSayEvent::MessageStarted {
                action_index: 1,
                recipient: "agent-%2".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadDelta {
                action_index: 1,
                text: "pending payload".to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 1 },
            mez_agent::StreamingSayEvent::ActionComplete { action_index: 1 },
            mez_agent::StreamingSayEvent::Started {
                action_index: 2,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 2,
                text: "unpromoted trailing say".to_string(),
            },
            mez_agent::StreamingSayEvent::TextComplete { action_index: 2 },
            mez_agent::StreamingSayEvent::ActionComplete { action_index: 2 },
        ] {
            service
                .ingest_provider_log(
                    "%1",
                    &turn.turn_id,
                    crate::runtime::RuntimeProviderLogInput::Progress(&event),
                )
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: String::new(),
                    actions: vec![answer, message, trailing],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: Vec::new(),
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
        assert_eq!(
            service
                .reconcile_agent_streaming_say_completion("%1", &turn.turn_id, &execution)
                .unwrap(),
            std::collections::BTreeSet::from([0])
        );
        let reconciled = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(
            !reconciled.contains("unpromoted trailing say"),
            "{reconciled}"
        );
        if resize {
            assert!(
                service
                    .rebuild_agent_presentation_after_resize("%1", Size::new(72, 12).unwrap())
                    .unwrap()
            );
            let resized = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n");
            assert_eq!(resized.matches("permanent sibling").count(), 1, "{resized}");
            assert!(!resized.contains("unpromoted trailing say"), "{resized}");
        }
        if let Some(accepted) = accepted {
            let mut settled = execution.clone();
            let batch = settled.response.action_batch.as_ref().unwrap();
            let ledger_turn = service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .clone();
            settled.action_results = vec![
                mez_agent::ActionResult::succeeded(
                    &ledger_turn,
                    &batch.actions[0],
                    Vec::new(),
                    None,
                ),
                if accepted {
                    mez_agent::ActionResult::succeeded(
                        &ledger_turn,
                        &batch.actions[1],
                        Vec::new(),
                        None,
                    )
                } else {
                    mez_agent::ActionResult::failed(
                        &ledger_turn,
                        &batch.actions[1],
                        mez_agent::ActionStatus::Failed,
                        "recipient_unavailable",
                        "recipient unavailable",
                    )
                    .unwrap()
                },
                mez_agent::ActionResult::succeeded(
                    &ledger_turn,
                    &batch.actions[2],
                    Vec::new(),
                    None,
                ),
            ];
            settled.terminal_state = if accepted {
                AgentTurnState::Running
            } else {
                AgentTurnState::Failed
            };
            if accepted {
                service
                    .settle_accepted_outbound_message_preview(
                        "%1",
                        &turn.turn_id,
                        1,
                        &batch.actions[1],
                    )
                    .unwrap();
            }
            service
                .finalize_settled_outbound_message_previews("%1", &turn.turn_id, &settled)
                .unwrap();
            service
                .present_deferred_agent_say_actions_to_terminal_buffer("%1", &settled)
                .unwrap();
            let settled_text = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n");
            assert_eq!(
                settled_text.matches("permanent sibling").count(),
                1,
                "{settled_text}"
            );
            assert_eq!(
                settled_text.contains("pending payload"),
                accepted,
                "{settled_text}"
            );
            assert_eq!(
                settled_text.matches("unpromoted trailing say").count(),
                usize::from(accepted),
                "{settled_text}"
            );
            if accepted {
                assert!(
                    settled_text.find("pending payload").unwrap()
                        < settled_text.find("unpromoted trailing say").unwrap(),
                    "{settled_text}"
                );
            }
        }
        service
            .append_agent_status_text_to_terminal_buffer("%1", "later durable status")
            .unwrap();
        let text = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(text.matches("permanent sibling").count(), 1, "{text}");
        assert_eq!(
            text.matches("unpromoted trailing say").count(),
            usize::from(accepted == Some(true)),
            "{text}"
        );
        assert!(text.contains("later durable status"), "{text}");
    }

    /// Verifies normal-mode filtering prevents noncanonical outbound message media
    /// from creating provisional source state or sender-pane rows.
    #[test]
    fn runtime_streaming_outbound_message_filters_json_in_normal_mode() {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let turn = service
            .start_agent_prompt_turn("%1", "stream a filtered outbound message")
            .unwrap();

        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                &turn.turn_id,
                &mez_agent::StreamingSayEvent::MessageStarted {
                    action_index: 0,
                    recipient: "agent-%2".to_string(),
                    content_type: "application/json".to_string(),
                },
            )
            .unwrap();
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                &turn.turn_id,
                &mez_agent::StreamingSayEvent::MessagePayloadDelta {
                    action_index: 0,
                    text: "{\"hidden\":true}".to_string(),
                },
            )
            .unwrap();

        assert!(
            service
                .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                .unwrap()
                .is_none()
        );
    }

    /// Verifies verbose mode renders an established noncanonical message payload
    /// literally and retains its exact provisional source through provider
    /// reconciliation until message-service settlement decides its final fate.
    #[test]
    fn runtime_streaming_outbound_message_renders_verbose_json_until_settlement() {
        let mut service = test_runtime_service();
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "verbose-peer-log".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
            }])
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
        );
        let turn = service
            .start_agent_prompt_turn("%1", "stream a verbose outbound message")
            .unwrap();
        let baseline = service.agent_pane_screen("%1").unwrap().clone();
        let payload = r#"{\"visible\":true}"#;

        for event in [
            mez_agent::StreamingSayEvent::MessageStarted {
                action_index: 0,
                recipient: "agent-%2".to_string(),
                content_type: "application/json".to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadDelta {
                action_index: 0,
                text: payload.to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 },
            mez_agent::StreamingSayEvent::MessageStarted {
                action_index: 1,
                recipient: "agent-%3".to_string(),
                content_type: "application/json".to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadDelta {
                action_index: 1,
                text: "rejected sibling payload".to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 1 },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(
            service
                .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                .unwrap()
                .expect("verbose outbound payload should produce projection work"),
        )
        .unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let preview = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(preview.contains("agent-%2<"), "{preview}");
        assert!(preview.contains(payload), "{preview}");

        let action = mez_agent::AgentAction {
            id: "streamed-message".to_string(),
            payload: mez_agent::AgentActionPayload::SendMessage {
                recipient: "agent-%2".to_string(),
                scope: None,
                content_type: "application/json".to_string(),
                payload: payload.to_string(),
                correlation_id: None,
            },
        };
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: payload.to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: String::new(),
                    actions: vec![
                        action,
                        mez_agent::AgentAction {
                            id: "rejected-streamed-message".to_string(),
                            payload: mez_agent::AgentActionPayload::SendMessage {
                                recipient: "agent-%3".to_string(),
                                scope: None,
                                content_type: "application/json".to_string(),
                                payload: "rejected sibling payload".to_string(),
                                correlation_id: None,
                            },
                        },
                    ],
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
                .reconcile_agent_streaming_say_completion("%1", &turn.turn_id, &execution)
                .unwrap()
                .is_empty()
        );
        assert_ne!(service.agent_pane_screen("%1").unwrap(), &baseline);
        let reconciled = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(reconciled.contains("agent-%2<"), "{reconciled}");
        assert!(reconciled.contains(payload), "{reconciled}");

        let ledger_turn = service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .expect("streamed message turn remains in the ledger")
            .clone();
        let mut blocked_execution = execution.clone();
        let blocked_batch = blocked_execution.response.action_batch.as_ref().unwrap();
        blocked_execution.action_results = vec![
            mez_agent::ActionResult::succeeded(
                &ledger_turn,
                &blocked_batch.actions[0],
                Vec::new(),
                None,
            ),
            mez_agent::ActionResult::blocked(
                &ledger_turn,
                &blocked_batch.actions[1],
                Vec::new(),
                "{\"approval\":{}}".to_string(),
            ),
        ];
        let blocked_action = &blocked_execution
            .response
            .action_batch
            .as_ref()
            .unwrap()
            .actions[0];
        service
            .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, blocked_action)
            .unwrap();
        service
            .finalize_settled_outbound_message_previews("%1", &turn.turn_id, &blocked_execution)
            .unwrap();
        let blocked = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(blocked.contains("rejected sibling payload"), "{blocked}");

        let mut accepted_execution = execution.clone();
        let accepted_batch = accepted_execution.response.action_batch.as_ref().unwrap();
        accepted_execution.action_results = vec![
            mez_agent::ActionResult::succeeded(
                &ledger_turn,
                &accepted_batch.actions[0],
                Vec::new(),
                None,
            ),
            mez_agent::ActionResult::failed(
                &ledger_turn,
                &accepted_batch.actions[1],
                mez_agent::ActionStatus::Failed,
                "transport_error",
                "recipient unavailable",
            )
            .unwrap(),
        ];
        let accepted_action = &accepted_execution
            .response
            .action_batch
            .as_ref()
            .unwrap()
            .actions[0];
        service
            .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, accepted_action)
            .unwrap();
        service
            .finalize_settled_outbound_message_previews("%1", &turn.turn_id, &accepted_execution)
            .unwrap();
        service
            .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, accepted_action)
            .unwrap();
        let settled = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(settled.matches("agent-%2<").count(), 1, "{settled}");
        assert!(!settled.contains("rejected sibling payload"), "{settled}");
    }

    /// Verifies an accepted outbound row persists a sent-only semantic source and
    /// replays through the outbound renderer without becoming a receiver receipt.
    #[test]
    fn runtime_accepted_outbound_message_persists_sent_source_for_replay() {
        let mut service = test_runtime_service();
        service.set_subagent_lineage(
            "agent-%1",
            RuntimeSubagentLineage {
                parent_agent_id: "agent-root".to_string(),
                root_agent_id: "agent-root".to_string(),
                depth: 1,
                display_name: "child".to_string(),
                terminal: false,
            },
        );
        service.set_subagent_lineage(
            "agent-%2",
            RuntimeSubagentLineage {
                parent_agent_id: "agent-root".to_string(),
                root_agent_id: "agent-root".to_string(),
                depth: 1,
                display_name: "nonhuman".to_string(),
                terminal: false,
            },
        );
        let transcript_store = AgentTranscriptStore::new(temp_root("accepted-outbound-source"));
        service
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
        let turn = service
            .start_agent_prompt_turn("%1", "persist accepted outbound presentation")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        let action = mez_agent::AgentAction {
            id: "accepted-outbound-action".to_string(),
            payload: mez_agent::AgentActionPayload::SendMessage {
                recipient: "agent-%2".to_string(),
                scope: None,
                content_type: "text/plain".to_string(),
                payload: "accepted outbound replay evidence".to_string(),
                correlation_id: None,
            },
        };

        service
            .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, &action)
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
        let entry = entries
            .iter()
            .find(|entry| {
                entry
                    .source_text
                    .as_deref()
                    .is_some_and(|source| source.contains("accepted outbound replay evidence"))
            })
            .expect("accepted outbound presentation entry");
        let source = entry.source_text.as_deref().unwrap();
        assert!(source.contains("\"direction\":\"sent\""), "{source}");
        assert!(source.contains("\"peer\":\"nonhuman\""), "{source}");
        assert!(source.contains("accepted-outbound-action"), "{source}");
        assert_eq!(
            crate::runtime::render::peer_message_presentation_receive_identity(
                entry.source_content_type.as_deref().unwrap(),
                source,
            ),
            None
        );

        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        assert!(
            service
                .rebuild_agent_presentation_after_resize("%1", Size::new(40, 12).unwrap())
                .unwrap()
        );
        let replayed = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(
            replayed.contains("nonhuman< accepted outbound replay"),
            "{replayed}"
        );
        assert!(replayed.contains("evidence"), "{replayed}");
        service.terminate_all_pane_processes().unwrap();
    }

    /// Verifies settlement preserves a direct-parent label captured by streaming
    /// even if the sender's lineage becomes fenced before message acceptance.
    #[test]
    fn runtime_streamed_outbound_parent_label_persists_after_lineage_fence() {
        let mut service = test_runtime_service();
        service.set_subagent_lineage(
            "agent-%1",
            RuntimeSubagentLineage {
                parent_agent_id: "agent-%2".to_string(),
                root_agent_id: "agent-root".to_string(),
                depth: 2,
                display_name: "child".to_string(),
                terminal: false,
            },
        );
        let transcript_store = AgentTranscriptStore::new(temp_root("streamed-outbound-parent"));
        service
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
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        let turn = service
            .start_agent_prompt_turn("%1", "stream direct-parent message")
            .unwrap();
        let payload = "stable direct-parent evidence";
        for event in [
            mez_agent::StreamingSayEvent::MessageStarted {
                action_index: 0,
                recipient: "agent-%2".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::MessagePayloadDelta {
                action_index: 0,
                text: payload.to_string(),
            },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(
            service
                .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                .unwrap()
                .expect("complete outbound message should produce projection work"),
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
                .iter()
                .any(|line| line.contains("parent< stable direct-parent"))
        );

        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                &turn.turn_id,
                &mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 },
            )
            .unwrap();

        service.fence_subagent_descendants_for_parent_conversation("agent-%2", "replaced");
        let action = mez_agent::AgentAction {
            id: "streamed-parent-action".to_string(),
            payload: mez_agent::AgentActionPayload::SendMessage {
                recipient: "agent-%2".to_string(),
                scope: None,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                payload: payload.to_string(),
                correlation_id: None,
            },
        };
        service
            .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, &action)
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
        let source = entries
            .iter()
            .filter_map(|entry| entry.source_text.as_deref())
            .find(|source| source.contains(payload))
            .expect("accepted outbound presentation source");
        assert!(source.contains("\"peer\":\"agent-%2\""), "{source}");
        assert!(source.contains("\"direct_parent\":true"), "{source}");

        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        assert!(
            service
                .rebuild_agent_presentation_after_resize("%1", Size::new(40, 12).unwrap())
                .unwrap()
        );
        let replayed = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(
            replayed.contains("parent< stable direct-parent evidence"),
            "{replayed}"
        );
        service.terminate_all_pane_processes().unwrap();
    }
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

mod rationale_fallback;

/// Exact and rejected header projections retain only accepted source rows.
mod header_reconciliation {
    use super::*;

    /// Verifies safe shell summaries and closed web-search headers render before
    /// provider completion with the same rows and styling as static presentation.
    ///
    /// These components remain provisional: authoritative completion restores the
    /// baseline so an unvalidated header cannot imply that an action dispatched.
    #[test]
    fn runtime_streaming_summary_and_web_header_match_static_projection_and_restore() {
        let mut streaming = test_runtime_service();
        let mut static_render = test_runtime_service();
        for service in [&mut streaming, &mut static_render] {
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
            service
                .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
                .unwrap();
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            set_agent_pane_screen_for_test(
                service,
                "%1",
                TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
            );
            service
                .append_agent_status_text_to_terminal_buffer("%1", "baseline")
                .unwrap();
        }
        let baseline = streaming.agent_pane_screen("%1").unwrap().clone();
        let summary = "Inspect current streaming previews";
        let query = "streaming-previews-with-an-unbroken-target-0123456789abcdefghij";
        let action = mez_agent::AgentAction {
            id: "search-streamed".to_string(),

            payload: mez_agent::AgentActionPayload::WebSearch {
                query: query.to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };

        for event in [
            mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 },
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
                action_index: 0,
                text: summary.to_string(),
            },
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextComplete { action_index: 0 },
            mez_agent::StreamingSayEvent::ActionHeader {
                action_index: 1,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                    query: query.to_string(),
                }),
            },
        ] {
            streaming
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
                .unwrap();
        }
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(
            streaming
                .take_agent_streaming_say_projection_work("%1", "turn-1")
                .unwrap()
                .expect("safe provisional components should project before completion"),
        )
        .unwrap();
        assert!(
            streaming
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );

        let streamed_lines = streaming
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(
            streamed_lines
                .iter()
                .filter(|line| !line.trim().is_empty())
                .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 24),
            "{streamed_lines:?}"
        );

        static_render
            .append_agent_thinking_text_to_terminal_buffer("%1", summary)
            .unwrap();
        assert!(
            static_render
                .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
                .unwrap()
        );
        assert_eq!(
            streaming
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines(),
            static_render
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines(),
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
                    rationale: String::new(),

                    actions: vec![action],
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

    /// Discovery and wait previews use the ordinary transient action-header path;
    /// neither is durable or evidence that a peer response has arrived.
    #[test]
    fn runtime_streaming_discovery_and_wait_headers_restore_on_reconciliation() {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .append_agent_status_text_to_terminal_buffer("%1", "baseline")
            .unwrap();
        let baseline = service.agent_pane_screen("%1").unwrap().clone();
        for (index, payload) in [
            mez_agent::AgentActionPayload::ListAgents {
                agent_type: Some("subagent".to_string()),
                scope: Some("project".to_string()),
            },
            mez_agent::AgentActionPayload::Wait,
        ]
        .into_iter()
        .enumerate()
        {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer(
                    "%1",
                    "turn-1",
                    &mez_agent::StreamingSayEvent::ActionHeader {
                        action_index: index,
                        header: Box::new(mez_agent::StreamingActionHeader::Action {
                            action: Box::new(mez_agent::AgentAction {
                                id: String::new(),
                                payload,
                            }),
                        }),
                    },
                )
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let text = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(
            text.contains("list agents: type=subagent scope=project"),
            "{text}"
        );
        assert!(text.contains("wait: peer reply requested"), "{text}");
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture("turn-1"),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: Vec::new(),
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
        assert!(
            service
                .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
                .unwrap()
                .is_empty()
        );
        assert_eq!(service.agent_pane_screen("%1").unwrap(), &baseline);
    }

    /// Verifies a validated header does not vanish at provider completion while
    /// its action is still pending, and never counts as an executed result.
    #[tokio::test]
    async fn runtime_streaming_matching_header_survives_reconciliation() {
        let mut service = test_runtime_service();
        let transcript_store = AgentTranscriptStore::new(temp_root("streaming-header-settlement"));
        service.set_agent_transcript_store(transcript_store.clone());
        let conversation_id = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        let started = service
            .start_agent_prompt_turn("%1", "search for matching header")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        let query = "matching header";
        for event in [
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: "Search for the matching header".to_string(),
            },
            mez_agent::StreamingSayEvent::RationaleTextComplete,
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
                .unwrap();
        }
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-1",
                &mez_agent::StreamingSayEvent::ActionHeader {
                    action_index: 0,
                    header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                        query: query.to_string(),
                    }),
                },
            )
            .unwrap();
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-1",
                &mez_agent::StreamingSayEvent::ActionHeader {
                    action_index: 1,
                    header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                        query: "second matching header".to_string(),
                    }),
                },
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let visible = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(
            visible
                .join("\n")
                .contains("agent: web search: matching header")
        );
        let action = mez_agent::AgentAction {
            id: "action-1".to_string(),
            payload: mez_agent::AgentActionPayload::WebSearch {
                query: query.to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
        let second = mez_agent::AgentAction {
            id: "action-2".to_string(),
            payload: mez_agent::AgentActionPayload::WebSearch {
                query: "second matching header".to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "Search for the matching header".to_string(),
                    actions: vec![action.clone(), second.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: [&action, &second]
                .into_iter()
                .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
                .collect(),
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
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert_eq!(
            rows.iter()
                .filter(|line| line.contains("web search: matching header"))
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|line| line.contains("web search: second matching header"))
                .count(),
            1
        );
        assert_eq!(
            service
                .runtime_metrics()
                .agent_streaming_settlement_restorations,
            0
        );
        assert!(
            rows.iter()
                .any(|line| line.contains("thinking: Search for the matching header"))
        );
        assert!(
            visible
                .iter()
                .any(|line| line.contains("web search: matching header"))
        );
        let entries = transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some("web search: matching header"))
                .count(),
            1,
            "{entries:?}"
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref()
                    == Some("web search: second matching header"))
                .count(),
            1
        );
    }

    /// A changed accepted header must instead replace the unvalidated preview,
    /// retaining only the authoritative action text.
    #[test]
    fn runtime_streaming_header_mismatch_restores_validated_source() {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-1",
                &mez_agent::StreamingSayEvent::ActionHeader {
                    action_index: 0,
                    header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                        query: "unvalidated query".to_string(),
                    }),
                },
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let action = mez_agent::AgentAction {
            id: "search".to_string(),
            payload: mez_agent::AgentActionPayload::WebSearch {
                query: "validated query".to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
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
                    rationale: "Use the validated query".to_string(),
                    actions: vec![action.clone()],
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
            service
                .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
                .unwrap()
                .is_empty()
        );
        assert!(
            service
                .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
                .unwrap()
        );
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(rows.contains("validated query"), "{rows}");
        assert!(!rows.contains("unvalidated query"), "{rows}");
    }

    /// Distinct raw queries with the same compact header must not inherit a
    /// provisional row or durable source from the rejected provider preview.
    #[test]
    fn runtime_streaming_header_preview_collision_restores_baseline() {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "check header collision")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        let prefix = "x".repeat(120);
        let streamed = format!("{prefix}a");
        let accepted = format!("{prefix}b");
        assert_ne!(streamed, accepted);
        let baseline = service.agent_pane_screen("%1").unwrap().clone();
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-1",
                &mez_agent::StreamingSayEvent::ActionHeader {
                    action_index: 0,
                    header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                        query: streamed,
                    }),
                },
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let action = mez_agent::AgentAction {
            id: "search".to_string(),
            payload: mez_agent::AgentActionPayload::WebSearch {
                query: accepted,
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "Use validated query".to_string(),
                    actions: vec![action.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::succeeded(
                &turn,
                &action,
                vec!["search complete".to_string()],
                None,
            )],
            final_turn: true,
            terminal_state: AgentTurnState::Completed,
        };
        assert!(
            service
                .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
                .unwrap()
                .is_empty()
        );
        assert_eq!(service.agent_pane_screen("%1").unwrap(), &baseline);
    }

    /// A changed header must not erase a matching sibling progress row while the
    /// authoritative header replaces only the rejected preview.
    #[tokio::test]
    async fn runtime_streaming_header_mismatch_retains_matching_progress() {
        let mut service = test_runtime_service();
        let store = AgentTranscriptStore::new(temp_root("streaming-mismatch-sibling"));
        service.set_agent_transcript_store(store.clone());
        let conversation_id = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        let started = service
            .start_agent_prompt_turn("%1", "inspect both rows")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        for event in [
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: "Inspect both rows".to_string(),
            },
            mez_agent::StreamingSayEvent::RationaleTextComplete,
            mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "keep sibling".to_string(),
            },
            mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
            mez_agent::StreamingSayEvent::ActionHeader {
                action_index: 1,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                    query: "wrong query".to_string(),
                }),
            },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let say = mez_agent::AgentAction {
            id: "say".to_string(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Progress,
                text: "keep sibling".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        let search = mez_agent::AgentAction {
            id: "search".to_string(),
            payload: mez_agent::AgentActionPayload::WebSearch {
                query: "right query".to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "Inspect both rows".to_string(),
                    actions: vec![say.clone(), search.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![
                mez_agent::ActionResult::succeeded(&turn, &say, Vec::new(), None),
                mez_agent::ActionResult::succeeded(&turn, &search, Vec::new(), None),
            ],
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
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(rows.matches("keep sibling").count(), 1, "{rows}");
        assert!(!rows.contains("wrong query"), "{rows}");
        assert_eq!(rows.matches("right query").count(), 1, "{rows}");
        assert_eq!(
            service
                .runtime_metrics()
                .agent_streaming_settlement_restorations,
            0,
            "matching siblings must survive an atomic replacement, not baseline restoration"
        );
        let entries = store.inspect_presentation(&conversation_id).unwrap();
        for source in ["keep sibling", "web search: right query"] {
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry.source_text.as_deref() == Some(source))
                    .count(),
                1,
                "{entries:?}"
            );
        }
        service
            .append_agent_status_text_to_terminal_buffer("%1", "later durable row")
            .unwrap();
        let appended = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(appended.matches("keep sibling").count(), 1, "{appended}");
        assert_eq!(appended.matches("right query").count(), 1, "{appended}");
        assert!(appended.contains("later durable row"), "{appended}");
    }
}

/// Final components and headers settle together after runtime-visible work.
mod deferred_finals {
    use super::*;

    /// A matching progress say and accepted search header share one projected
    /// screen; accepting the batch must not erase either visible component.
    #[tokio::test]
    async fn runtime_streaming_progress_and_header_keep_matching_rows() {
        for header_first in [false, true] {
            streaming_progress_and_header_case(header_first, mez_agent::SayStatus::Progress).await;
        }
    }

    /// A final say following an accepted header is already in settled display order.
    #[tokio::test]
    async fn runtime_streaming_header_then_final_say_keeps_matching_rows() {
        streaming_progress_and_header_case(true, mez_agent::SayStatus::Final).await;
    }

    /// A final say after pending work must not become durable before the runtime
    /// result authorizes deferred presentation.
    #[tokio::test]
    async fn runtime_streaming_header_then_final_say_waits_for_runtime_settlement() {
        streaming_progress_and_header_case_with_outcome(
            true,
            mez_agent::SayStatus::Final,
            false,
            false,
            true,
        )
        .await;
        streaming_progress_and_header_case_with_outcome(
            true,
            mez_agent::SayStatus::Final,
            false,
            true,
            true,
        )
        .await;
        streaming_progress_and_header_case_with_outcome(
            true,
            mez_agent::SayStatus::Final,
            true,
            true,
            false,
        )
        .await;
        streaming_progress_and_header_case_with_finals(true, true, false).await;
        streaming_progress_and_header_case_with_finals(true, false, false).await;
        streaming_progress_and_header_case_with_finals(false, true, true).await;
    }

    /// Two final says after pending work remain provisional together and either
    /// become durable on success or disappear together on failure.
    async fn streaming_progress_and_header_case_with_finals(
        changed: bool,
        completed: bool,
        trailing: bool,
    ) {
        let mut service = test_runtime_service();
        let store = AgentTranscriptStore::new(temp_root("streaming-multiple-finals"));
        service.set_agent_transcript_store(store.clone());
        let conversation_id = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        let started = service
            .start_agent_prompt_turn("%1", "inspect two finals")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        let search = mez_agent::AgentAction {
            id: "search".to_string(),
            payload: mez_agent::AgentActionPayload::WebSearch {
                query: "accepted query".to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
        let finals = ["first final", "second final"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| mez_agent::AgentAction {
                id: format!("final-{index}"),
                payload: mez_agent::AgentActionPayload::Say {
                    status: mez_agent::SayStatus::Final,
                    text: text.to_string(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
            })
            .collect::<Vec<_>>();
        let progress = mez_agent::AgentAction {
            id: "trailing-progress".to_string(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Progress,
                text: "trailing progress".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        for event in [
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: "Inspect two finals".to_string(),
            },
            mez_agent::StreamingSayEvent::RationaleTextComplete,
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                &turn.turn_id,
                &mez_agent::StreamingSayEvent::ActionHeader {
                    action_index: 0,
                    header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                        query: if changed {
                            "preview query"
                        } else {
                            "accepted query"
                        }
                        .to_string(),
                    }),
                },
            )
            .unwrap();
        for (index, text) in ["first final", "second final"].into_iter().enumerate() {
            for event in [
                mez_agent::StreamingSayEvent::Started {
                    action_index: index + 1,
                    status: mez_agent::SayStatus::Final,
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
                mez_agent::StreamingSayEvent::TextDelta {
                    action_index: index + 1,
                    text: text.to_string(),
                },
                mez_agent::StreamingSayEvent::TextComplete {
                    action_index: index + 1,
                },
            ] {
                service
                    .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                    .unwrap();
            }
        }
        if trailing {
            for event in [
                mez_agent::StreamingSayEvent::Started {
                    action_index: 3,
                    status: mez_agent::SayStatus::Progress,
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
                mez_agent::StreamingSayEvent::TextDelta {
                    action_index: 3,
                    text: "trailing progress".to_string(),
                },
                mez_agent::StreamingSayEvent::TextComplete { action_index: 3 },
            ] {
                service
                    .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                    .unwrap();
            }
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let execution =
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
                response: mez_agent::ModelResponse {
                    provider: "runtime-batch".to_string(),
                    model: "test".to_string(),
                    raw_text: String::new(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: Some(mez_agent::MaapBatch {
                        rationale: "Inspect two finals".to_string(),
                        actions: std::iter::once(search.clone())
                            .chain(finals.clone())
                            .chain(trailing.then_some(progress.clone()))
                            .collect(),
                    }),
                    provider_transcript_events: Vec::new(),
                },
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: std::iter::once(mez_agent::ActionResult::running(
                    &turn,
                    &search,
                    Vec::new(),
                    None,
                ))
                .chain(finals.iter().map(|action| {
                    mez_agent::ActionResult::succeeded(&turn, action, Vec::new(), None)
                }))
                .chain(trailing.then(|| {
                    mez_agent::ActionResult::succeeded(&turn, &progress, Vec::new(), None)
                }))
                .collect(),
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
        let entries = store.inspect_presentation(&conversation_id).unwrap();
        for text in ["first final", "second final"] {
            let rows = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines();
            assert!(rows.iter().any(|row| row.contains(text)), "{rows:?}");
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry.source_text.as_deref() == Some(text))
                    .count(),
                0
            );
        }
        assert_eq!(
            service
                .settle_pending_final_say_preview("%1", &turn.turn_id, completed)
                .unwrap(),
            completed
        );
        let entries = store.inspect_presentation(&conversation_id).unwrap();
        for text in ["first final", "second final"] {
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry.source_text.as_deref() == Some(text))
                    .count(),
                usize::from(completed)
            );
            assert_eq!(
                service
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .iter()
                    .any(|row| row.contains(text)),
                completed
            );
        }
        if trailing && completed {
            let live = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines();
            let live_final = live
                .iter()
                .position(|row| row.contains("second final"))
                .unwrap();
            let live_progress = live
                .iter()
                .position(|row| row.contains("trailing progress"))
                .unwrap();
            let sources = entries
                .iter()
                .filter_map(|entry| entry.source_text.as_deref())
                .collect::<Vec<_>>();
            let durable_final = sources
                .iter()
                .position(|source| *source == "second final")
                .unwrap();
            let durable_progress = sources
                .iter()
                .position(|source| *source == "trailing progress")
                .unwrap();
            assert_eq!(
                live_final < live_progress,
                durable_final < durable_progress,
                "{live:?} {sources:?}"
            );
        }
    }

    /// A blocked header does not become execution evidence, but its accepted
    /// rationale and progress sibling should not disappear during settlement.
    #[tokio::test]
    async fn runtime_streaming_blocked_header_keeps_matching_siblings() {
        streaming_progress_and_header_case_with_blocked(
            false,
            mez_agent::SayStatus::Progress,
            true,
            false,
        )
        .await;
        streaming_progress_and_header_case_with_blocked(
            false,
            mez_agent::SayStatus::Progress,
            true,
            true,
        )
        .await;
    }

    /// Exercises both action orders so persisted replay matches the live projection.
    async fn streaming_progress_and_header_case(header_first: bool, status: mez_agent::SayStatus) {
        streaming_progress_and_header_case_with_outcome(header_first, status, false, false, false)
            .await;
    }

    /// Exercises the blocked variant without changing the successful replay oracle.
    async fn streaming_progress_and_header_case_with_blocked(
        header_first: bool,
        status: mez_agent::SayStatus,
        blocked: bool,
        changed: bool,
    ) {
        streaming_progress_and_header_case_with_outcome(
            header_first,
            status,
            blocked,
            changed,
            false,
        )
        .await;
    }

    /// Exercises the pending variant without treating a visible final say as durable.
    async fn streaming_progress_and_header_case_with_outcome(
        header_first: bool,
        status: mez_agent::SayStatus,
        blocked: bool,
        changed: bool,
        pending: bool,
    ) {
        let say_index = usize::from(header_first);
        let header_index = usize::from(!header_first);
        let mut service = test_runtime_service();
        let store = AgentTranscriptStore::new(temp_root("streaming-progress-header"));
        service.set_agent_transcript_store(store.clone());
        let conversation_id = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        let started = service
            .start_agent_prompt_turn("%1", "search with progress")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        let say = mez_agent::AgentAction {
            id: "progress".to_string(),
            payload: mez_agent::AgentActionPayload::Say {
                status,
                text: "searching now".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        let search = mez_agent::AgentAction {
            id: "search".to_string(),
            payload: mez_agent::AgentActionPayload::WebSearch {
                query: "matching query".to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
        for event in [
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: "Look up matching query".to_string(),
            },
            mez_agent::StreamingSayEvent::RationaleTextComplete,
            mez_agent::StreamingSayEvent::Started {
                action_index: say_index,
                status,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: say_index,
                text: "searching now".to_string(),
            },
            mez_agent::StreamingSayEvent::TextComplete {
                action_index: say_index,
            },
            mez_agent::StreamingSayEvent::ActionHeader {
                action_index: header_index,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                    query: if changed {
                        "wrong query"
                    } else {
                        "matching query"
                    }
                    .to_string(),
                }),
            },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let visible = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(visible.iter().any(|row| row.contains("searching now")));
        assert!(visible.iter().any(|row| row.contains(if changed {
            "web search: wrong query"
        } else {
            "web search: matching query"
        })));
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "Look up matching query".to_string(),
                    actions: if header_first {
                        vec![search.clone(), say.clone()]
                    } else {
                        vec![say.clone(), search.clone()]
                    },
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![
                mez_agent::ActionResult::succeeded(
                    &turn,
                    &say,
                    vec!["searching now".to_string()],
                    None,
                ),
                if pending {
                    mez_agent::ActionResult::running(&turn, &search, Vec::new(), None)
                } else if blocked {
                    mez_agent::ActionResult::blocked(
                        &turn,
                        &search,
                        Vec::new(),
                        "{\"approval\":{}}".to_string(),
                    )
                } else {
                    mez_agent::ActionResult::succeeded(
                        &turn,
                        &search,
                        vec!["search complete".to_string()],
                        None,
                    )
                },
            ],
            final_turn: !blocked && !pending,
            terminal_state: if pending {
                AgentTurnState::Running
            } else if blocked {
                AgentTurnState::Blocked
            } else {
                AgentTurnState::Completed
            },
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
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains("searching now"))
                .count(),
            usize::from(!blocked || status == mez_agent::SayStatus::Progress)
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains("web search: matching query"))
                .count(),
            usize::from(!blocked)
        );
        if blocked {
            assert_eq!(
                service
                    .runtime_metrics()
                    .agent_streaming_settlement_restorations,
                0
            );
        }
        let entries = store.inspect_presentation(&conversation_id).unwrap();
        if pending {
            assert_eq!(status, mez_agent::SayStatus::Final);
            assert!(rows.iter().any(|row| row.contains("searching now")));
            assert!(
                !entries
                    .iter()
                    .any(|entry| entry.source_text.as_deref() == Some("searching now")),
                "pending final must not become durable before the action settles"
            );
            return;
        }
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some("searching now"))
                .count(),
            usize::from(!blocked || status == mez_agent::SayStatus::Progress)
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some("web search: matching query"))
                .count(),
            usize::from(!blocked)
        );
        let ordered_sources = entries
            .iter()
            .filter_map(|entry| entry.source_text.as_deref())
            .filter(|source| {
                matches!(
                    *source,
                    "Look up matching query" | "searching now" | "web search: matching query"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_sources,
            if blocked {
                if status == mez_agent::SayStatus::Progress {
                    vec!["Look up matching query", "searching now"]
                } else {
                    vec!["Look up matching query"]
                }
            } else if header_first {
                vec![
                    "Look up matching query",
                    "web search: matching query",
                    "searching now",
                ]
            } else {
                vec![
                    "Look up matching query",
                    "searching now",
                    "web search: matching query",
                ]
            }
        );
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        assert!(
            service
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
                .unwrap()
        );
        let replayed = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        let progress_row = replayed
            .iter()
            .position(|row| row.contains("searching now"));
        if blocked {
            assert_eq!(
                progress_row.is_some(),
                status == mez_agent::SayStatus::Progress
            );
            assert!(
                !replayed
                    .iter()
                    .any(|row| row.contains("web search: matching query"))
            );
        } else {
            let progress_row = progress_row.unwrap();
            let header_row = replayed
                .iter()
                .position(|row| row.contains("web search: matching query"))
                .unwrap();
            assert_eq!(header_row < progress_row, header_first, "{replayed:?}");
        }
    }
}

/// Accepted command projections share rows with independently owned siblings.
mod command_siblings;

/// Independent pane writes and shell suffixes preserve exact screen lineage.
mod preview_interleaving {
    use super::*;

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
        let mut screen = TerminalScreen::new(Size::new(40, 4).unwrap(), 120).unwrap();
        screen.feed(b"durable zero\r\ndurable one\r\ndurable two\r\ndurable three");
        set_agent_pane_screen_for_test(&mut service, "%1", screen);
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
        let retained_history_len = service.agent_pane_screen("%1").unwrap().history().len();

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

        let updated_screen = service.agent_pane_screen("%1").unwrap();
        let updated = updated_screen.normal_content_lines().join("\n");
        assert!(
            updated.contains("provider one and provider two"),
            "{updated}"
        );
        assert!(!updated.contains("settled shell tail one"), "{updated}");
        assert!(!updated.contains("settled shell tail two"), "{updated}");
        assert_eq!(updated_screen.history().len(), retained_history_len);
        assert!(
            service
                .agent_shell_output_previews_for_tests("%1")
                .is_empty()
        );
    }

    /// Verifies a new provider response consumes an already displayed shell window.
    ///
    /// Unlike an update to a provider response that predates the shell preview,
    /// this is the ordinary next-response handoff. Both its first projection and
    /// subsequent deltas must leave durable rows at their installed coordinates.
    #[test]
    fn runtime_streaming_say_after_settled_tail_preserves_visible_rows() {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(60, 5).unwrap(), 40).unwrap();
        screen.feed(b"durable-zero\r\ndurable-one\r\ndurable-two\r\ndurable-three\r\ndurable-four");
        set_agent_pane_screen_for_test(&mut service, "%1", screen);
        let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
            turn_id: "turn-shell".to_string(),
            action_id: "shell".to_string(),
            marker: "marker".to_string(),
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
        assert_eq!(
            service.agent_pane_screen("%1").unwrap().visible_lines(),
            vec![
                "durable-three",
                "durable-four",
                "▐ tail-one",
                "▐ tail-two",
                "▐ tail-three"
            ]
        );
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-next",
                &mez_agent::StreamingSayEvent::Started {
                    action_index: 0,
                    status: mez_agent::SayStatus::Progress,
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
                },
            )
            .unwrap();
        let mut frames = Vec::new();
        for text in ["replacement", " continued"] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer(
                    "%1",
                    "turn-next",
                    &mez_agent::StreamingSayEvent::TextDelta {
                        action_index: 0,
                        text: text.to_string(),
                    },
                )
                .unwrap();
            let projection = RuntimeSessionService::build_agent_streaming_say_projection(
                service
                    .take_agent_streaming_say_projection_work("%1", "turn-next")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert!(
                service
                    .apply_agent_streaming_say_projection_result(projection)
                    .unwrap()
            );
            frames.push(service.agent_pane_screen("%1").unwrap().visible_lines());
        }
        assert_eq!(
            frames,
            vec![
                vec![
                    "durable-three",
                    "durable-four",
                    "▐ mez> replacement",
                    "",
                    ""
                ],
                vec![
                    "durable-three",
                    "durable-four",
                    "▐ mez> replacement continued",
                    "",
                    ""
                ],
            ]
        );
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-next",
                &mez_agent::StreamingSayEvent::TextDelta {
                    action_index: 0,
                    text: "\nline two\nline three\nline four".to_string(),
                },
            )
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-next")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let before_rollback = service.agent_pane_screen("%1").unwrap().visible_lines();
        assert!(
            service
                .discard_agent_streaming_say_presentation("%1", Some("turn-next"))
                .unwrap()
        );
        let after_rollback = service.agent_pane_screen("%1").unwrap().visible_lines();
        for marker in ["durable-three", "durable-four"] {
            assert_eq!(
                after_rollback.iter().position(|line| line.contains(marker)),
                before_rollback
                    .iter()
                    .position(|line| line.contains(marker)),
                "rollback moved {marker} downward: {before_rollback:?} -> {after_rollback:?}",
            );
        }
    }

    /// Verifies a full-pane provider rebase retires only settled shell ownership.
    ///
    /// When settled and active preview owners share a full pane, consuming the
    /// settled suffix must retain its viewport displacement, project the active
    /// owner exactly once, and transfer that rebased baseline to later provider
    /// cleanup without resurrecting or duplicating either owner.
    #[test]
    fn runtime_streaming_say_rebases_mixed_shell_preview_owners_in_full_pane() {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(40, 4).unwrap(), 120).unwrap();
        screen.feed(b"durable zero\r\ndurable one\r\ndurable two\r\ndurable three");
        set_agent_pane_screen_for_test(&mut service, "%1", screen);
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
        let settled_owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
            turn_id: "turn-shell-settled".to_string(),
            action_id: "shell-settled".to_string(),
            marker: "marker-settled".to_string(),
        };
        let active_owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
            turn_id: "turn-shell-active".to_string(),
            action_id: "shell-active".to_string(),
            marker: "marker-active".to_string(),
        };
        service
            .update_agent_shell_output_preview(
                "%1",
                settled_owner.clone(),
                1,
                &[
                    "settled shell one".to_string(),
                    "settled shell two".to_string(),
                ],
            )
            .unwrap();
        service
            .update_agent_shell_output_preview(
                "%1",
                active_owner.clone(),
                1,
                &["active shell once".to_string()],
            )
            .unwrap();
        assert!(service.settle_agent_shell_output_preview("%1", &settled_owner));
        let retained_history_len = service.agent_pane_screen("%1").unwrap().history().len();

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

        let rebased_screen = service.agent_pane_screen("%1").unwrap();
        let rebased = rebased_screen.normal_content_lines().join("\n");
        assert_eq!(rebased_screen.history().len(), retained_history_len);
        assert!(
            rebased.contains("provider one and provider two"),
            "{rebased}"
        );
        assert!(!rebased.contains("settled shell one"), "{rebased}");
        assert!(!rebased.contains("settled shell two"), "{rebased}");
        assert_eq!(rebased.matches("active shell once").count(), 1, "{rebased}");
        assert_eq!(service.agent_shell_output_previews_for_tests("%1").len(), 1);

        assert!(
            service
                .discard_agent_streaming_say_presentation("%1", Some("turn-provider"))
                .unwrap()
        );
        let restored_screen = service.agent_pane_screen("%1").unwrap();
        let restored = restored_screen.normal_content_lines().join("\n");
        assert_eq!(restored_screen.history().len(), retained_history_len);
        assert!(!restored.contains("provider one"), "{restored}");
        assert!(!restored.contains("settled shell one"), "{restored}");
        assert_eq!(
            restored.matches("active shell once").count(),
            1,
            "{restored}"
        );
        assert_eq!(
            service.agent_shell_output_previews_for_tests("%1"),
            vec![(active_owner, 1, 1, vec!["active shell once".to_string()])]
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
                    rationale: String::new(),

                    actions: vec![action],
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
        service
            .presentation
            .seed_accepted_streaming_header_for_tests(
                "%1",
                "turn-snapshot",
                "search-snapshot",
                "web search: snapshot",
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
        assert!(
            service
                .presentation
                .has_accepted_streaming_header_for_tests("%1", "turn-snapshot", "search-snapshot")
        );

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
        assert!(
            !service
                .presentation
                .has_accepted_streaming_header_for_tests("%1", "turn-snapshot", "search-snapshot")
        );
        let text = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(text.contains("intervening rollback row"), "{text}");
    }
}

/// Durable semantic source reproduces prompt, status, and action presentation.
mod source_replay {
    use super::*;

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
        let transcript_store =
            AgentTranscriptStore::new(temp_root("agent-complete-reconstruction"));
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
        let transcript_store =
            AgentTranscriptStore::new(temp_root("agent-command-preview-bounded"));
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
        let parent_marker_foreground = service.ui_theme().colors.agent_transcript_parent.foreground;
        let live_parent_marker = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines()
            .into_iter()
            .find(|line| line.text.contains("parent> "))
            .and_then(|line| {
                line.style_spans
                    .iter()
                    .find(|span| span.rendition.foreground == Some(parent_marker_foreground))
                    .copied()
            })
            .expect("live parent line must carry a name-marker span");
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
        let replayed_parent_marker = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines()
            .into_iter()
            .find(|line| line.text.contains("parent> "))
            .and_then(|line| {
                line.style_spans
                    .iter()
                    .find(|span| span.rendition.foreground == Some(parent_marker_foreground))
                    .copied()
            })
            .expect("replayed parent line must carry a name-marker span");
        assert_eq!(
            live_parent_marker, replayed_parent_marker,
            "a replayed parent prompt must keep the live name-marker span"
        );
        assert_eq!(replayed_parent_marker.start, "▐ ".chars().count());
        assert_eq!(replayed_parent_marker.length, "parent>".chars().count());
        assert!(
            replayed_parent_marker.rendition.background.is_none(),
            "name markers must never paint a background"
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
            replayed_compact
                .contains("thinkingpreservethisdurablerationaleacrossthereconstructedpane"),
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
}

/// Peer-source replay retains direction, label, eligibility, and corruption guards.
mod peer_replay {
    use super::*;

    /// Verifies logged peer and parent lines colorize their name markers only.
    ///
    /// The marker is the bounded name plus its received-message glyph. The payload
    /// keeps the terminal's default color and the span adds no display cells, so a
    /// received peer line and a parent prompt retain their distinct markers without
    /// changing line text or wrapping.
    #[test]
    fn runtime_agent_peer_and_parent_lines_colorize_name_markers() {
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
            .unwrap();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
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
            .append_agent_received_peer_message_to_terminal_buffer(
                "%1",
                "agent-%3",
                "text/plain; charset=utf-8",
                "check cwd",
            )
            .unwrap();
        service
            .append_agent_received_direct_parent_message_to_terminal_buffer(
                "%1",
                "text/plain; charset=utf-8",
                "direct parent evidence",
            )
            .unwrap();
        service
            .append_agent_received_peer_message_to_terminal_buffer(
                "%1",
                "parent",
                "text/plain; charset=utf-8",
                "ordinary peer evidence",
            )
            .unwrap();
        service
            .append_agent_parent_prompt_to_terminal_buffer("%1", "restore parent")
            .unwrap();

        let styled_lines = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        assert_ne!(
            service.ui_theme().colors.agent_transcript_parent.foreground,
            service.ui_theme().colors.agent_transcript_error.foreground,
            "the parent marker must not reuse the transcript error foreground"
        );
        let gutter = "▐ ".chars().count();
        for (text, marker, pair) in [
            (
                "▐ agent-%3> check cwd",
                "agent-%3>",
                service.ui_theme().colors.agent_transcript_peer_sender,
            ),
            (
                "▐ parent> direct parent evidence",
                "parent>",
                service.ui_theme().colors.agent_transcript_parent,
            ),
            (
                "▐ parent> ordinary peer evidence",
                "parent>",
                service.ui_theme().colors.agent_transcript_peer_sender,
            ),
            (
                "▐ parent> restore parent",
                "parent>",
                service.ui_theme().colors.agent_transcript_parent,
            ),
        ] {
            let line = styled_lines
                .iter()
                .find(|line| line.text == text)
                .unwrap_or_else(|| panic!("missing styled line {text:?}: {styled_lines:#?}"));
            let marker_end = gutter + marker.chars().count();
            assert!(
                line.style_spans.iter().any(|span| {
                    span.start == gutter
                        && span.length == marker.chars().count()
                        && span.rendition.foreground == Some(pair.foreground)
                        && span.rendition.background.is_none()
                }),
                "{text:?} must colorize only its name marker: {:?}",
                line.style_spans
            );
            assert!(
                line.style_spans.iter().all(|span| {
                    span.start.saturating_add(span.length) <= marker_end
                        || span.rendition.foreground != Some(pair.foreground)
                }),
                "{text:?} must not carry the name-marker color past the marker: {:?}",
                line.style_spans
            );
        }
        service.terminate_all_pane_processes().unwrap();
    }

    /// Verifies a logged received peer message persists its peer name and payload
    /// so a presentation replay rebuilds the byte-identical prompt-style line.
    ///
    /// A restart must not lose the originating agent: the peer content type stores a
    /// JSON record instead of user-prompt text, so replay prints the same `{peer}> `
    /// prefix at the destination geometry rather than a nameless or re-trusted
    /// line. The stored record keeps the unbounded peer payload, so a
    /// payload above the peer-context bound is bounded once, at render time, and the
    /// live and replayed rows stay byte-identical, truncation marker included.
    ///
    /// The stored media type keeps canonical plaintext filtering reproducible: a
    /// suppressed non-plaintext payload leaves no record behind for replay to
    /// resurrect.
    #[test]
    fn runtime_agent_peer_message_persists_source_for_replay() {
        let mut service = test_runtime_service();
        let transcript_store = AgentTranscriptStore::new(temp_root("agent-peer-message-source"));
        service
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

        // The live rows are captured from this fresh screen so the comparison below
        // isolates the peer echo instead of pane process output.
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        // A payload above the peer-context bound: the bound has to be applied once,
        // at render time, so the replayed line carries the same truncation marker.
        let large_payload = "peer payload segment ".repeat(16_000);
        assert!(
            large_payload.len() > 256 * 1024,
            "the payload must exceed the peer-context bound: {}",
            large_payload.len()
        );

        service
            .append_agent_received_peer_message_to_terminal_buffer(
                "%1",
                "agent-%3",
                "text/plain; charset=utf-8",
                large_payload.as_str(),
            )
            .unwrap();
        service
            .append_agent_received_peer_message_to_terminal_buffer(
                "%1",
                "agent-%3",
                "text/plain; charset=utf-8",
                "check the pane cwd",
            )
            .unwrap();
        service
            .append_agent_received_direct_parent_message_to_terminal_buffer(
                "%1",
                "text/plain; charset=utf-8",
                "direct parent replay evidence",
            )
            .unwrap();
        // JSON payloads, including runtime bridge traffic and a model-authored
        // result payload, remain presentation-silent in normal mode.
        service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "application/json",
            r#"{"task_id":"task-10","state":"running","progress_percent":0,"summary":"working"}"#,
        )
        .unwrap();
        service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "application/json",
            r#"{"task_id":"task-9","success":true,"summary":"done","output":{"rows":41,"ok":true}}"#,
        )
        .unwrap();
        let live_rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        let live_text = live_rows
            .join("\n")
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect::<String>();
        for suppressed in ["rows41oktrue", "task9", "task10", "success", "working"] {
            assert!(
                !live_text.contains(suppressed),
                "normal mode suppresses JSON presentation, so {suppressed} must not reach the log: {live_text}"
            );
        }
        let live_styled_rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let conversation_id = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let entries = transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap();
        assert_eq!(
            entries.len(),
            3,
            "each suppressed JSON payload writes no presentation record: {entries:?}"
        );
        assert!(
            entries.iter().all(|entry| {
                !entry
                    .source_text
                    .as_deref()
                    .is_some_and(|source| source.contains("task-10") || source.contains("task-9"))
            }),
            "suppressed JSON payloads must not be persisted for replay: {entries:#?}"
        );
        assert!(
            entries.iter().all(|entry| entry
                .source_content_type
                .as_deref()
                .is_some_and(|content_type| content_type.contains("peer-message+json"))),
            "{entries:?}"
        );
        let received_source = entries
            .iter()
            .find_map(|entry| {
                let source = entry.source_text.as_deref()?;
                source.contains("check the pane cwd").then_some(source)
            })
            .expect("received peer presentation source");
        assert!(
            received_source.contains("\"direction\":\"received\""),
            "{received_source}"
        );
        assert!(received_source.contains("agent-%3"), "{received_source}");
        let direct_parent_source = entries
            .iter()
            .find_map(|entry| {
                let source = entry.source_text.as_deref()?;
                source
                    .contains("direct parent replay evidence")
                    .then_some(source)
            })
            .expect("direct-parent peer presentation source");
        assert!(
            direct_parent_source.contains("\"peer\":\"parent\""),
            "{direct_parent_source}"
        );
        assert!(
            direct_parent_source.contains("\"direct_parent\":true"),
            "{direct_parent_source}"
        );
        let large_source = entries
            .iter()
            .find_map(|entry| {
                let source = entry.source_text.as_deref()?;
                (source.len() > large_payload.len()).then_some(source)
            })
            .expect("peer presentation source carrying the unbounded payload");
        assert!(
            !large_source.contains("truncated; original_bytes="),
            "the persisted peer source must keep the unbounded payload: {}",
            large_source.len()
        );

        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        assert!(
            service
                .rebuild_agent_presentation_after_resize("%1", Size::new(40, 12).unwrap())
                .unwrap()
        );
        let replayed = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        // Replay may rebuild a prefix from earlier persisted entries, so the live
        // rows must reappear byte-identical as the trailing rows of the rebuild.
        assert_eq!(
            live_rows.as_slice(),
            &replayed[replayed.len().saturating_sub(live_rows.len())..],
            "a live peer line and its replayed line must be byte-identical"
        );
        assert!(
            replayed
                .iter()
                .any(|line| line == "▐ agent-%3> check the pane cwd"),
            "{replayed:#?}"
        );
        assert!(
            replayed
                .iter()
                .any(|line| line == "▐ parent> direct parent replay evidence"),
            "{replayed:#?}"
        );
        // Replay re-derives each received-message marker from the persisted label,
        // so the replayed rows must keep the live spans exactly.
        let replayed_styled_rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        assert_ne!(
            service.ui_theme().colors.agent_transcript_parent.foreground,
            service.ui_theme().colors.agent_transcript_error.foreground,
            "the replayed parent marker must not reuse the transcript error foreground"
        );
        assert_eq!(
            live_styled_rows.as_slice(),
            &replayed_styled_rows[replayed_styled_rows
                .len()
                .saturating_sub(live_styled_rows.len())..],
            "a live peer line and its replayed line must keep identical name-marker spans"
        );
        let received_marker = replayed_styled_rows
            .iter()
            .find(|line| line.text == "▐ agent-%3> check the pane cwd")
            .expect("replayed received peer line");
        assert!(
            received_marker.style_spans.iter().any(|span| {
                span.start == "▐ ".chars().count()
                    && span.length == "agent-%3>".chars().count()
                    && span.rendition.foreground
                        == Some(
                            service
                                .ui_theme()
                                .colors
                                .agent_transcript_peer_sender
                                .foreground,
                        )
                    && span.rendition.background.is_none()
            }),
            "{received_marker:?}"
        );
        let direct_parent_marker = replayed_styled_rows
            .iter()
            .find(|line| line.text == "▐ parent> direct parent replay evidence")
            .expect("replayed direct-parent peer line");
        assert!(
            direct_parent_marker.style_spans.iter().any(|span| {
                span.start == "▐ ".chars().count()
                    && span.length == "parent>".chars().count()
                    && span.rendition.foreground
                        == Some(service.ui_theme().colors.agent_transcript_parent.foreground)
                    && span.rendition.background.is_none()
            }),
            "{direct_parent_marker:?}"
        );
        assert!(
            !replayed
                .iter()
                .any(|line| line.contains("task-10") || line.contains("task-9")),
            "suppressed JSON payloads must stay silent after replay: {replayed:#?}"
        );
        let replayed_text = replayed
            .join("\n")
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect::<String>();
        assert!(
            replayed_text.contains("originalbytes"),
            "the replayed line must carry the peer-context truncation marker: {}",
            replayed_text.len()
        );
        service.terminate_all_pane_processes().unwrap();
    }

    /// Verifies legacy peer records without media types stay suppressed in normal
    /// mode while verbose replay preserves their bounded raw payloads.
    #[test]
    fn runtime_agent_legacy_peer_message_record_still_replays() {
        let mut service = test_runtime_service();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        let conversation_id = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let legacy_peer_entry =
            |sequence: u64, source: &str| crate::storage::transcript::AgentPresentationEntry {
                conversation_id: conversation_id.clone(),
                sequence,
                created_at_unix_seconds: 1,
                pane_id: "%1".to_string(),
                turn_id: None,
                terminal_width: 40,
                style_names: vec!["user-prompt".to_string()],
                display_lines: vec![source.to_string()],
                copy_lines: Vec::new(),
                ansi_text: None,
                source_text: Some(source.to_string()),
                source_content_type: Some(
                    "application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8"
                        .to_string(),
                ),
            };
        // Neither record carries the optional media-type field, exactly like a peer
        // presentation record written before canonical plaintext filtering existed.
        let entries = vec![
            legacy_peer_entry(
                1,
                r#"{"direction":"received","peer":"agent-%3","payload":"legacy peer evidence"}"#,
            ),
            legacy_peer_entry(
                2,
                r#"{"direction":"sent","peer":"agent-%2","payload":"legacy ack"}"#,
            ),
        ];

        assert!(
            service
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
                .unwrap()
        );
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(
            !rows
                .iter()
                .any(|line| line.contains("legacy peer evidence")),
            "normal mode suppresses legacy records without media types: {rows:#?}"
        );
        assert!(
            !rows.iter().any(|line| line.contains("legacy ack")),
            "normal mode suppresses legacy records without media types: {rows:#?}"
        );
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "verbose-peer-replay".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
            }])
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        assert!(
            service
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
                .unwrap()
        );
        let verbose_rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(
            verbose_rows
                .iter()
                .any(|line| line == "▐ agent-%3> legacy peer evidence"),
            "verbose replay preserves legacy received payloads: {verbose_rows:#?}"
        );
        assert!(
            !verbose_rows.iter().any(|line| line.contains("legacy ack")),
            "verbose replay skips legacy sent records: {verbose_rows:#?}"
        );
        service.terminate_all_pane_processes().unwrap();
    }

    /// Verifies malformed, unknown-direction, and oversized stored peer sources are
    /// skipped instead of panicking or rendering corrupt log state as transcript.
    ///
    /// The peer content type is written only by the peer echo writer, so a record
    /// that does not decode is damaged presentation state. Replay must drop that one
    /// line, never invent assistant output from its raw bytes, and never allocate or
    /// render from an untrusted stored length. The oversized case is valid JSON above
    /// the decode guard, so it is skipped only because the guard is enforced.
    #[test]
    fn runtime_agent_peer_message_replay_skips_malformed_source() {
        let mut service = test_runtime_service();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
        );
        let conversation_id = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let peer_content_type =
            "application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8";
        let peer_entry =
            |sequence: u64, source: String| crate::storage::transcript::AgentPresentationEntry {
                conversation_id: conversation_id.clone(),
                sequence,
                created_at_unix_seconds: 1,
                pane_id: "%1".to_string(),
                turn_id: None,
                terminal_width: 40,
                style_names: vec!["user-prompt".to_string()],
                display_lines: vec![source.clone()],
                copy_lines: Vec::new(),
                ansi_text: None,
                source_text: Some(source),
                source_content_type: Some(peer_content_type.to_string()),
            };
        // A syntactically valid record larger than the decode guard. Its payload
        // would render as a visible peer line if the guard were removed, so the
        // skip cannot be a side effect of JSON parsing.
        let oversize_source = serde_json::json!({
            "direction": "received",
            "peer": "agent-%9",
            "payload": "oversize peer payload ".repeat(200_000),
        })
        .to_string();
        assert!(
            oversize_source.len() > 4 * 1024 * 1024,
            "the oversize case must exceed the decode guard: {}",
            oversize_source.len()
        );
        let entries = vec![
            peer_entry(1, "{\"direction\":\"received\"".to_string()),
            peer_entry(
                2,
                "{\"direction\":\"sideways\",\"peer\":\"agent-%3\",\"payload\":\"bad direction\"}"
                    .to_string(),
            ),
            peer_entry(3, oversize_source),
        ];

        assert!(
            service
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
                .unwrap()
        );
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(
            rows.iter().all(|line| line.trim().is_empty()),
            "undecodable peer sources must not render: {rows:#?}"
        );
        assert!(
            rows.iter().all(|line| !line.contains("agent-%9")),
            "an oversize peer source must be skipped instead of rendered: {rows:#?}"
        );
    }
}

/// Live resize rebuilds source-backed screens without touching hidden shell surfaces.
mod resize_replay {
    use super::*;

    /// Verifies a geometry-aware rebuild preserves an earlier legacy snapshot
    /// before replaying a later semantic entry at the destination geometry.
    #[test]
    fn runtime_agent_resize_keeps_legacy_snapshots_ordered_with_semantic_entries() {
        let mut service = test_runtime_service();
        let transcript_store =
            AgentTranscriptStore::new(temp_root("agent-mixed-presentation-source"));
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
            assert_eq!(
                transition.side_effects,
                vec![RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::ResizeDrag,
                }]
            );
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
            .apply_resize_debounce_timer_transition(primary.as_str(), true)
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
        assert!(!release.full_redraw_required);
        assert!(
            release_transition
                .side_effects
                .iter()
                .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
        );
        assert!(
            service
                .presentation
                .agent_presentation_resize_is_deferred("%1")
        );
        let commit = service
            .apply_resize_debounce_timer_transition(primary.as_str(), true)
            .unwrap();
        assert!(commit.applied);
        assert!(commit.side_effects.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::RenderClient {
                reason: RenderInvalidationReason::FullRedraw,
                ..
            }
        )));
        assert!(commit.side_effects.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::DispatchAgentPresentationResize { pane_id, .. }
                if pane_id == "%1"
        )));
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
                source_text: Some(
                    "# Async rebuild\n\nsource survives completion resize".to_string(),
                ),
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
                    rationale: "render the requested table".to_string(),

                    actions: vec![mez_agent::AgentAction {
                        id: "say-table".to_string(),

                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: table.to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
                                .to_string(),
                        },
                    }],
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
}
