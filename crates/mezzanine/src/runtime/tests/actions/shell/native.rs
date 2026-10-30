//! Native shell output, worker ownership, deadlines and patch projection.
//!
//! Real generated workers exercise the existing native execution owner; explicit
//! redispatch remains a control distinct from completion-driven sibling handoff.

use super::*;

/// Verifies native shell output reaches the transient pane tail before completion.
///
/// Native execution drains stdout and stderr outside the pane PTY. A command
/// that emits one line and then sleeps must publish that line through the
/// worker progress relay while its action result is still running. Progress
/// carrying a stale marker must be rejected without changing the pane. On a
/// full pane each received row must displace only one row until the cap, and
/// final settlement must reuse that window even when the final tail shrinks.
/// Subsequent log rows consume the vacated space without rewinding the viewport.
#[test]
fn runtime_native_agent_shell_command_shows_transient_output_before_completion() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-live-output","input":"print native progress"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let release_path = std::env::temp_dir().join(format!(
        "mez-native-live-output-release-{}-{}",
        std::process::id(),
        crate::runtime::current_unix_millis()
    ));
    let _ = fs::remove_file(&release_path);
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native shell live output".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test native progress".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),

                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Print native progress".to_string(),
                        command: format!(
                            "printf \"%s%s\\n\" \"native-live-\" \"first\"; while [ ! -e \"{}\" ]; do sleep 0.01; done; printf \"%s%s\\n\" \"native-live-\" \"last\"",
                            release_path.display()
                        ),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(5_000),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_status.as_deref(), Some("executing"));

    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-1")
        .unwrap()
        .expect("native shell action should be queued for a worker");
    let marker = dispatch.marker.clone();
    let (progress_sender, mut progress_receiver) = tokio::sync::watch::channel(None);
    let worker = thread::spawn(move || {
        crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress_sender)
    });
    // Start at the bottom margin so every artificial reservation is observable.
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 40).unwrap();
    screen.feed(
        (0..12)
            .map(|row| format!("durable-{row}"))
            .collect::<Vec<_>>()
            .join("\r\n")
            .as_bytes(),
    );
    service.set_agent_pane_screen("%1", conversation_id, screen);
    assert_eq!(service.agent_pane_screen("%1").unwrap().history().len(), 0);
    let progress_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let progress = loop {
        if let Some(progress) = progress_receiver.borrow_and_update().clone() {
            break progress;
        }
        assert!(
            std::time::Instant::now() < progress_deadline,
            "native worker did not publish output before completion"
        );
        thread::sleep(Duration::from_millis(5));
    };
    let (revision, output_preview) = progress
        .output
        .expect("native shell worker progress must include output");
    assert!(
        !worker.is_finished(),
        "native worker completed before progress was observed"
    );

    assert!(
        service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                presentation: mez_agent::ActionPresentationProgress::new(
                    "turn-1",
                    "shell-1",
                    mez_agent::ActionPresentationExecutionIdentity::Attempt(marker.clone()),
                    revision,
                    mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                    output_preview,
                ),
            })
            .unwrap()
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("native-live-first"), "{pane_text}");
    let assert_live_output_style = |service: &RuntimeSessionService, text: &str| {
        let styled_lines = service
            .pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines();
        let output_line = styled_lines
            .iter()
            .find(|line| line.text.contains(text))
            .unwrap_or_else(|| panic!("missing native output `{text}` in {styled_lines:?}"));
        let output_column = output_line.text.find(text).unwrap();
        let rendition = styled_line_rendition_at(output_line, output_column);
        assert_eq!(
            rendition.foreground,
            Some(config.ui_theme.colors.agent_transcript_status.foreground)
        );
        assert!(rendition.dim && !rendition.bold, "{output_line:?}");

        let view = service
            .render_client_view(ClientViewRole::Primary, Size::new(80, 24).unwrap(), &config)
            .unwrap()
            .unwrap();
        let row = view
            .lines
            .iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("missing native output `{text}` in {:?}", view.lines));
        let column = view.lines[row].find(text).unwrap();
        let rendition = view.line_style_spans[row]
            .iter()
            .rev()
            .find(|span| column >= span.start && column < span.start.saturating_add(span.length))
            .map(|span| span.rendition)
            .unwrap_or_default();
        assert_eq!(
            rendition.foreground,
            Some(config.ui_theme.colors.agent_transcript_status.foreground)
        );
        assert!(
            rendition.dim && !rendition.bold,
            "{:?}",
            view.line_style_spans[row]
        );
    };
    assert_live_output_style(&service, "native-live-first");

    assert_eq!(
        service.agent_pane_screen("%1").unwrap().history().len(),
        1,
        "one received row must scroll only one row, with no trailing reservation"
    );

    assert!(
        service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                presentation: mez_agent::ActionPresentationProgress::new(
                    "turn-1",
                    "shell-1",
                    mez_agent::ActionPresentationExecutionIdentity::Attempt(marker.clone()),
                    revision.saturating_add(1),
                    mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                    "native-live-first\nnative-live-second",
                ),
            })
            .unwrap()
    );
    assert_live_output_style(&service, "native-live-second");
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (0, 0)
    );
    assert_eq!(service.agent_shell_output_previews_for_tests("%1").len(), 1);

    service
        .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
            presentation: mez_agent::ActionPresentationProgress::new(
                "turn-1",
                "shell-1",
                mez_agent::ActionPresentationExecutionIdentity::Attempt(marker.clone()),
                revision.saturating_sub(1),
                mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                "older-native-tail",
            ),
        })
        .unwrap();
    let stale_revision_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !stale_revision_text.contains("older-native-tail"),
        "{stale_revision_text}"
    );
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (0, 0)
    );

    assert!(
        !service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                presentation: mez_agent::ActionPresentationProgress::new(
                    "turn-1",
                    "shell-1",
                    mez_agent::ActionPresentationExecutionIdentity::Attempt(format!(
                        "{marker}-stale"
                    )),
                    revision.saturating_add(1),
                    mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                    "stale-native-tail",
                ),
            })
            .unwrap()
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("stale-native-tail"), "{pane_text}");

    let cap = service.terminal_shell_output_preview_lines();
    for count in 3..=cap + 2 {
        let source = (1..=count)
            .map(|row| format!("live-row-{row}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            service
                .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                    presentation: mez_agent::ActionPresentationProgress::new(
                        "turn-1",
                        "shell-1",
                        mez_agent::ActionPresentationExecutionIdentity::Attempt(marker.clone()),
                        revision + count as u64,
                        mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                        source,
                    ),
                })
                .unwrap()
        );
        let screen = service.agent_pane_screen("%1").unwrap();
        assert_eq!(screen.history().len(), count.min(cap));
        assert_eq!(
            screen.visible_lines()[0],
            format!("durable-{}", count.min(cap))
        );
        assert_eq!(
            screen
                .visible_lines()
                .iter()
                .filter(|line| line.contains("live-row-"))
                .count(),
            count.min(cap)
        );
    }
    let live_history_len = service.agent_pane_screen("%1").unwrap().history().len();
    fs::write(&release_path, b"release").unwrap();
    let outcome = worker.join().unwrap();
    assert!(service.complete_native_shell_action(outcome).unwrap());
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().history().len(),
        live_history_len,
        "completion must reuse the live window, never compose a second tail"
    );
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (0, 0),
        "native completion must retire transient executor progress"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("native-live-first"), "{pane_text}");
    assert!(pane_text.contains("native-live-last"), "{pane_text}");
    assert!(
        !service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                presentation: mez_agent::ActionPresentationProgress::new(
                    "turn-1",
                    "shell-1",
                    mez_agent::ActionPresentationExecutionIdentity::Attempt(marker),
                    revision.saturating_add(1),
                    mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                    "post-completion-native-tail",
                ),
            })
            .unwrap()
    );
    let settled_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !settled_text.contains("post-completion-native-tail"),
        "{settled_text}"
    );
    for row in 1..cap {
        service
            .append_agent_status_text_to_terminal_buffer("%1", &format!("replacement-{row}"))
            .unwrap();
        let screen = service.agent_pane_screen("%1").unwrap();
        assert_eq!(
            screen.history().len(),
            live_history_len,
            "replacement rows must fill the vacated window before scrolling"
        );
        assert_eq!(screen.visible_lines()[0], format!("durable-{cap}"));
    }
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_file(release_path);
}

/// Verifies native shell execution bypasses occupied-pane readiness and presents
/// captured command output through the shell-view renderer rather than
/// suppressing it with the normal result-preview policy.
///
/// Pane execution writes child output through the PTY, while native execution
/// captures it directly. An alternate-screen program may leave the pane in
/// `interactive-blocked`, but native actions must still execute without pane
/// input and expose cleaned stdout and stderr in shell view without wrapper
/// traffic or a duplicate action-result header.
#[test]
fn runtime_native_agent_shell_command_output_is_visible_in_shell_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-visible-output","input":"print native markers"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native shell output".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test action batch rationale".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),

                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Print native markers".to_string(),
                        command: "printf 'native-stdout\\n'; printf 'native-stderr\\n' >&2"
                            .to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-1")
        .unwrap()
        .expect("native shell action should be queued for a worker");
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    assert!(service.complete_native_shell_action(outcome).unwrap());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("native-stdout"), "{pane_text}");
    assert!(pane_text.contains("native-stderr"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_STATUS"), "{pane_text}");
    assert!(
        !pane_text.contains("mez> Print native markers"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a redispatch skips a native action already owned by a worker while
/// later running siblings remain eligible for their own native dispatch.
#[test]
fn runtime_native_shell_redispatch_skips_owned_sibling_and_dispatches_later_actions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-redispatch-owned-sibling","input":"run native sibling batch"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let actions = ["first", "second", "third"]
        .into_iter()
        .map(|label| mez_agent::AgentAction {
            id: format!("shell-{label}"),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: format!("Run native {label} sibling"),
                command: format!("printf 'native-redispatch-{label}\\n'"),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        })
        .collect::<Vec<_>>();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native redispatch siblings".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "exercise native redispatch ownership".to_string(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);

    let first = service
        .claim_native_shell_action("turn-1", "shell-first")
        .unwrap()
        .expect("first sibling should be queued");
    let redispatched = service
        .dispatch_stored_running_shell_actions("turn-1")
        .unwrap()
        .expect("later sibling should keep redispatch eligible");
    assert_eq!(redispatched.terminal_state, AgentTurnState::Running);
    let second = service
        .claim_native_shell_action("turn-1", "shell-second")
        .unwrap()
        .expect("second sibling should be queued after skipping the owned first sibling");
    service
        .dispatch_stored_running_shell_actions("turn-1")
        .unwrap()
        .expect("third sibling should remain eligible after two owned siblings");
    let third = service
        .claim_native_shell_action("turn-1", "shell-third")
        .unwrap()
        .expect("third sibling should be queued after skipping owned predecessors");

    let before_settlement = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let preview_positions = ["first", "second", "third"].map(|label| {
        before_settlement
            .find(&format!("$ printf 'native-redispatch-{label}"))
            .expect("each dispatched sibling must show its preview before settlement")
    });
    assert!(
        preview_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{before_settlement}"
    );

    for dispatch in [first, second, third] {
        assert!(
            service
                .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                    dispatch
                ))
                .unwrap()
        );
    }

    assert!(service.native_shell_progress_turn_ids().is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    for label in ["first", "second", "third"] {
        assert_eq!(
            pane_text
                .matches(&format!("│ native-redispatch-{label}\n"))
                .count(),
            1,
            "native sibling {label} should execute exactly once: {pane_text}"
        );
    }
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies native shell actions use the configured deadline snapshotted when
/// their owning turn starts, rather than a later live configuration value.
#[test]
fn runtime_native_shell_command_uses_snapshotted_configured_timeout() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);
    service.set_agent_native_shell_timeout_ms(1_234);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-snapshotted-timeout","input":"run a bounded native command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.set_agent_native_shell_timeout_ms(5);
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native timeout snapshot".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test native timeout snapshot".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),

                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a bounded native command".to_string(),
                        command: "true".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
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
    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-1")
        .unwrap()
        .expect("native shell action should be queued for a worker");
    assert_eq!(dispatch.request.timeout_ms, Some(1_234));
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(dispatch))
            .unwrap()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies native `apply_patch` hunk failures leave recovery shadow text as
/// their sole pane-visible failure presentation.
///
/// Native patch execution first snapshots the target, then dispatches a
/// generated write phase. A failing write already queues the recovery shadow;
/// the generic failed-action outcome must not add a second red error row.
#[test]
fn runtime_native_apply_patch_failure_shows_only_recovery_shadow_text() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-patch-failure","input":"patch Cargo.toml"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native patch failure".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {

                rationale: "test action batch rationale".to_string(),


                actions: vec![mez_agent::AgentAction {
                    id: "patch-1".to_string(),

                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Update File: Cargo.toml\n@@\n-__MEZ_NATIVE_PATCH_MISSING_CONTEXT__\n+updated\n*** End Patch".to_string(),
                        strip: None,
                    },
                }],

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

    let read_dispatch = service
        .claim_native_shell_action("turn-1", "patch-1")
        .unwrap()
        .expect("native apply-patch read should be queued");
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                read_dispatch
            ))
            .unwrap()
    );
    let write_dispatch = service
        .claim_native_shell_action("turn-1", "patch-1")
        .unwrap()
        .expect("native apply-patch write should be queued");
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                write_dispatch
            ))
            .unwrap()
    );

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover (patch hunk mismatch)"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent: apply patch (apply_patch failed"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies native semantic-patch writes publish confirmed diffs before the
/// worker reaches terminal completion.
///
/// The generated write phase performs the real filesystem mutation and emits
/// its authenticated ordinal/path/length record before a test-only barrier.
/// Native progress must carry that confirmation through the coalescing relay,
/// promote it exactly once, and retain the same visible diff after settlement.
#[test]
fn runtime_native_apply_patch_shows_confirmed_diff_before_completion() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let unique = crate::runtime::current_unix_millis();
    let target_rel = format!(
        "target/mez-native-patch-progress-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let release_path = std::env::temp_dir().join(format!(
        "mez-native-patch-progress-release-{}-{unique}",
        std::process::id()
    ));
    let _ = fs::remove_file(&release_path);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-patch-progress","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native patch progress".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test native patch progress".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "patch-1".to_string(),

                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: format!(
                            "*** Begin Patch\n*** Add File: {target_rel}\n+native-confirmed-progress\n*** End Patch"
                        ),
                        strip: None,
                    },
                }],
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

    let read_dispatch = service
        .claim_native_shell_action("turn-1", "patch-1")
        .unwrap()
        .expect("native apply-patch read should be queued");
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                read_dispatch
            ))
            .unwrap()
    );
    service
        .append_pending_native_phase_command_for_tests(
            "turn-1",
            "patch-1",
            &format!(
                "while [ ! -e '{}' ]; do sleep 0.01; done\n",
                release_path.display()
            ),
        )
        .unwrap();
    let write_dispatch = service
        .claim_native_shell_action("turn-1", "patch-1")
        .unwrap()
        .expect("native apply-patch write should be queued");
    let marker = write_dispatch.marker.clone();
    let (progress_sender, mut progress_receiver) = tokio::sync::watch::channel(None);
    let worker = thread::spawn(move || {
        crate::runtime::execute_native_shell_dispatch_with_progress(write_dispatch, progress_sender)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let section = loop {
        if let Some(progress) = progress_receiver.borrow_and_update().clone()
            && let Some(section) = progress.confirmed_patch_sections.first().cloned()
        {
            break section;
        }
        assert!(
            Instant::now() < deadline,
            "native patch confirmation timed out"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert!(
        !worker.is_finished(),
        "native patch worker settled before progress"
    );
    assert_eq!(section.path, target_rel);
    assert!(
        service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                presentation: mez_agent::ActionPresentationProgress::new(
                    "turn-1",
                    "patch-1",
                    mez_agent::ActionPresentationExecutionIdentity::Attempt(marker),
                    u64::try_from(section.ordinal)
                        .unwrap_or(u64::MAX)
                        .saturating_add(1),
                    mez_agent::ActionPresentationComponentIdentity::confirmed_mutation(
                        section.ordinal,
                        section.path,
                    ),
                    section.diff,
                ),
            })
            .unwrap()
    );
    let early_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        early_text.contains("native-confirmed-progress"),
        "{early_text}"
    );
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (0, 1)
    );

    fs::write(&release_path, b"release").unwrap();
    assert!(
        service
            .complete_native_shell_action(worker.join().unwrap())
            .unwrap()
    );
    let settled_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        settled_text.matches("native-confirmed-progress").count(),
        1,
        "{settled_text}"
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "native-confirmed-progress\n"
    );

    let _ = fs::remove_file(release_path);
    let _ = fs::remove_dir_all(target.parent().unwrap());
    service.terminate_all_pane_processes().unwrap();
}
