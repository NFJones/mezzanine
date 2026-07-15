//! Runtime tests for agent presentation terminal ui behavior.

use super::*;

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
            text: "[terminal]\ncursor_style = \"bar\"\ncursor_blink = false\ncursor_blink_interval_ms = 250\nresize_debounce_ms = 125\nrender_rate_limit_fps = 8\nreduced_motion = true\n"
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
    assert!(config.frame_context.reduced_motion);
    assert_eq!(config.frame_context.animation_tick_ms, 0);
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
            .primary_error_status_overlay
            .as_deref()
            .is_some_and(|message| message.contains("cannot split vertically")),
        "{:?}",
        service.primary_error_status_overlay
    );

    let dismiss = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(dismiss.mux_actions_applied, 0);
    assert!(dismiss.view_refresh_required);
    assert!(dismiss.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(service.primary_error_status_overlay.is_none());

    let retried = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(retried.mux_actions_applied, 0);
    assert!(service.primary_error_status_overlay.is_some());
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
    assert!(pane_text.contains("▐ mez> alpha beta gamma"), "{pane_text}");
    assert!(pane_text.contains("▐      delta epsilon"), "{pane_text}");
}
