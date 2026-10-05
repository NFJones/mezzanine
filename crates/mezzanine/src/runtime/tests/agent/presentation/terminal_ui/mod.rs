//! Runtime tests for agent presentation terminal ui behavior.
//!
//! Behavior owners separate event ordering/replay, validated promotion, message
//! acceptance, transient composition, source replay, and resize admission.
//! Original test names and assertions are retained; one-consumer fixtures stay
//! beside their owning tests and shared screen/copy setup remains with the parent.

use super::*;
use crate::runtime::{RenderInvalidationReason, RuntimeTransition};

mod command_promotion;
mod header_previews;
mod history_reconstruction;
mod intervening_writes;
mod peer_messages;
mod pending_messages;
mod pending_steering;
mod resize_admission;
mod resize_surfaces;
mod streaming_identity;
mod transient_restore;

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
        rows.iter().all(|line| line.text.starts_with("│ ")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|line| line.text.starts_with("│      recovery")),
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
    assert!(rows.iter().all(|line| line.starts_with("│ ")), "{rows:?}");
    assert!(
        rows.iter()
            .any(|line| line.starts_with("│      structured")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|line| line == "│ thinking: alpha beta"),
        "{rows:?}"
    );
    assert!(rows.iter().any(|line| line == "│      gamma"), "{rows:?}");
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
    assert!(pane_text.contains("│ mez> alpha beta gamma"), "{pane_text}");
    assert!(pane_text.contains("│      delta epsilon"), "{pane_text}");
}

mod source_ordering;

mod event_replay;

mod say_promotion;

mod outbound_messages;

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

mod header_reconciliation;

mod deferred_finals;

/// Accepted command projections share rows with independently owned siblings.
mod command_siblings;

mod preview_interleaving;

mod source_replay;

mod peer_replay;

/// Live resize rebuilds source-backed screens without touching hidden shell surfaces.
mod resize_replay;
