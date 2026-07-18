//! Runtime tests for events terminal behavior.

use super::*;

/// Builds one typed terminal clipboard write event for runtime policy tests.
///
/// Keeping construction local makes each test focus on product effect behavior
/// rather than repeating terminal-protocol wrapper details.
fn terminal_clipboard_write_event(selection: &str, content: &str) -> TerminalOscEvent {
    TerminalOscEvent::Clipboard(mez_terminal::TerminalClipboardRequest::Write {
        selection: mez_terminal::TerminalClipboardSelection::new(selection),
        content: mez_terminal::TerminalClipboardContent::new(content),
    })
}

/// Verifies external terminal clipboard policy stores OSC 52 writes internally,
/// attempts the host effect, ignores host failure, and rejects queries.
///
/// This integration guard proves the product adapter executes mux effect
/// intents in order without allowing a failed host transport or unsupported
/// query to erase internal state or disclose host clipboard data.
#[test]
fn runtime_external_terminal_clipboard_executes_typed_effect_plan_best_effort() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_failed_host_clipboard_copy, empty_host_clipboard_read);
    let events = [
        TerminalOscEvent::Clipboard(mez_terminal::TerminalClipboardRequest::Query {
            selection: mez_terminal::TerminalClipboardSelection::new("c"),
        }),
        terminal_clipboard_write_event("c", "secret-text"),
    ];

    let applied = service.apply_terminal_osc_events(&events).unwrap();

    assert_eq!(applied, 1);
    assert_eq!(service.paste_buffers().get("osc52"), Some("secret-text"));
    assert_eq!(
        TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().as_slice(),
        ["secret-text"]
    );
}

/// Verifies internal terminal clipboard policy updates only mux paste-buffer
/// state and never invokes the product host clipboard adapter.
///
/// The former raw-string branch accepted the internal mode but reused a helper
/// that always attempted host I/O. Typed policy must keep that mode truly local.
#[test]
fn runtime_internal_terminal_clipboard_omits_host_effect() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nclipboard = \"internal\"\n".to_string(),
        }])
        .unwrap();

    let applied = service
        .apply_terminal_osc_events(&[terminal_clipboard_write_event("p", "internal-text")])
        .unwrap();

    assert_eq!(applied, 1);
    assert_eq!(service.paste_buffers().get("osc52"), Some("internal-text"));
    assert!(TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().is_empty());
}

/// Verifies disabled terminal clipboard policy rejects OSC 52 writes before
/// either internal storage or product host effects occur.
///
/// Pane-originated clipboard bytes are untrusted input, so an explicit product
/// denial must result in a stable no-effect outcome rather than a partial copy.
#[test]
fn runtime_disabled_terminal_clipboard_rejects_all_write_effects() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nclipboard = \"disabled\"\n".to_string(),
        }])
        .unwrap();

    let applied = service
        .apply_terminal_osc_events(&[terminal_clipboard_write_event("c", "denied-text")])
        .unwrap();

    assert_eq!(applied, 0);
    assert_eq!(service.paste_buffers().get("osc52"), None);
    assert!(TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().is_empty());
}

/// Verifies product host-read unavailability feeds the mux fallback decision
/// and selects the most recent non-empty internal paste buffer.
///
/// The mux owns deterministic precedence, but the product must still perform
/// host I/O and internal-buffer lookup before routing the selected content to a
/// prompt or pane effect.
#[test]
fn runtime_clipboard_paste_source_falls_back_after_host_read_failure() {
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    service
        .paste_buffers_mut()
        .set_with_origin("recent", "fallback-text", Some("test".to_string()))
        .unwrap();

    let source = service.clipboard_or_most_recent_paste_source().unwrap();

    assert_eq!(
        source.kind(),
        &ClipboardPasteSourceKind::PasteBuffer {
            name: "recent".to_string()
        }
    );
    assert_eq!(source.content(), "fallback-text");
}

/// Verifies pane environment accepts explicit term selection.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_environment_accepts_explicit_term_selection() {
    let mut ids = IdFactory::default();
    let env = pane_environment_with_term(
        Path::new("/tmp/mez-1000/default.sock"),
        &ids.session(),
        &ids.window(),
        &ids.pane(),
        "screen-256color",
    )
    .unwrap();

    assert_eq!(env.term, "screen-256color");
}

/// Verifies that synchronized panes fan out primary foreground input to every
/// pane in the active window. The deferred pane-I/O path is the foreground
/// async terminal path, so it must preserve one ordered input payload per pane
/// instead of only writing to the active pane.
#[test]
fn runtime_deferred_foreground_input_synchronizes_active_window_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .execute_terminal_command(&primary, "split-window; synchronize-panes on")
        .unwrap();

    let (report, deferred) = service
        .apply_attached_terminal_step_transition(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"a".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 2);
    let pane_inputs = pane_input_effects(&deferred.side_effects);
    assert_eq!(pane_inputs.len(), 2);
    assert_eq!(pane_inputs[0].pane_input_parts().0, "%1");
    assert_eq!(pane_inputs[0].pane_input_parts().1, b"a");
    assert_eq!(pane_inputs[1].pane_input_parts().0, "%2");
    assert_eq!(pane_inputs[1].pane_input_parts().1, b"a");
}

/// Verifies repeated runtime `terminal/step` requests with the same
/// idempotency key replay the completed response without reapplying pane input.
///
/// Foreground attach clients may retry a completed step request after a local
/// transport interruption. The runtime must return the cached JSON-RPC result
/// and avoid queueing the same pane input bytes a second time.
#[test]
fn runtime_control_terminal_step_replays_completed_response_without_reapplying_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let handed_off = service.take_running_pane_processes_for_adapter(1).unwrap();
    assert_eq!(handed_off.len(), 1);
    assert!(service.drain_pane_io_transition().side_effects.is_empty());

    let first = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-step-first","method":"terminal/step","params":{"idempotency_key":"terminal-step-replay","input_bytes":[97],"render":false}}"#,
        &primary,
    );
    let first_inputs = service.drain_pane_io_transition().side_effects;
    assert_eq!(first_inputs.len(), 1);
    assert_eq!(first_inputs[0].pane_input_parts().0, "%1");
    assert_eq!(first_inputs[0].pane_input_parts().1, b"a");

    let second = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-step-second","method":"terminal/step","params":{"idempotency_key":"terminal-step-replay","input_bytes":[97],"render":false}}"#,
        &primary,
    );
    assert_eq!(second, first);
    assert!(service.drain_pane_io_transition().side_effects.is_empty());
    assert_eq!(service.control_idempotency().len(), 1);
}

/// Verifies runtime keeps a lone escape key as pending prefix state until the
/// next terminal action consumes it.
///
/// This regression scenario protects the split between entering prefix-key
/// state and explicitly requesting the command prompt through the prefix table.
#[test]
fn runtime_applies_lone_prefix_key_as_pending_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();

    let prefix_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::EnterPrefixKeyMode],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(prefix_report.view_refresh_required);
    assert!(service.primary_prefix_key_pending());
    assert!(service.primary_prompt_input().is_none());
    assert!(
        service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap()
            .prefix_key_pending
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(
                    MuxAction::EnterCommandPrompt,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(!service.primary_prefix_key_pending());
    assert!(service.primary_prompt_input().is_some());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that destructive default prefix bindings open command prompts with
/// the explicit force flag required by live target shutdown semantics. The user
/// still has to submit the prompt, but the generated command no longer fails
/// the confirmation gate for live pane and window targets.
#[test]
fn runtime_destructive_prefix_prompts_include_explicit_force() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::KillWindowAfterConfirmation),
            TerminalClientLoopAction::ExecuteMux(MuxAction::KillPaneAfterConfirmation),
        ],
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
    assert!(!report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("kill-pane --force ")),
        "{:?}",
        view.lines.last()
    );
}

/// Verifies that default prefix mux actions that do not open a command prompt
/// still perform a runtime side effect instead of being reported as unsupported.
#[test]
fn runtime_applies_default_prefix_mux_actions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat"))
        .unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat"))
        .unwrap();
    let active_before = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();

    let cycle_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(MuxAction::CyclePane)],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(cycle_report.mux_actions_applied, 1);
    assert!(cycle_report.view_refresh_required);
    assert!(!cycle_report.full_redraw_required);
    assert_ne!(
        service.session().active_window().unwrap().active_pane().id,
        active_before
    );

    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::SendPrefixToPane),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ListKeyBindings),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ShowPaneIndexes),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ShowMessages),
            TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCopyModeAndPageUp),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SwapPaneNext),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SwapPanePrevious),
        ],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 7);
    assert!(report.unsupported_actions.is_empty());
    assert!(!service.active_copy_modes().is_empty());
    assert_eq!(service.session().active_window().unwrap().panes().len(), 3);
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains("attached_display_command")),
        "{events:?}"
    );
    let display_panes_event = events
        .iter()
        .find(|event| {
            event
                .payload
                .contains(r#""attached_display_command":"display-panes""#)
        })
        .expect("display-panes binding should emit attached display output");
    assert!(
        display_panes_event
            .payload
            .contains("chooser=select-pane-index"),
        "{display_panes_event:?}"
    );
    assert!(
        display_panes_event
            .payload
            .contains("action=select-pane -t"),
        "{display_panes_event:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies keyboard pane-focus actions use a diff redraw.
///
/// Focus changes move the global terminal cursor and restyle pane ownership
/// surfaces even when pane text stays unchanged. They need a fresh view, but
/// they should keep the retained output frame so the attached renderer can
/// update only changed rows and cursor state instead of clearing the viewport.
#[test]
fn runtime_keyboard_focus_pane_requests_diff_redraw() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneHorizontal)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(
                    PaneFocusDirection::Down,
                ))],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies alternate-screen exit moves stale interactive readiness into prompt recovery.
///
/// Full-screen terminal applications enter the alternate screen and mark pane
/// readiness as interactive-blocked. When the application exits and process
/// metadata already proves the primary shell is foreground again, the pane must
/// move to prompt-candidate recovery immediately instead of leaving later shell
/// actions blocked behind stale interactive readiness.
#[test]
fn runtime_alternate_screen_exit_recovers_interactive_blocked_readiness() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");

    service
        .apply_pane_output_bytes("%1", b"\x1b[?1049hfullscreen".to_vec())
        .unwrap();
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::InteractiveBlocked
    );

    service
        .apply_pane_output_bytes("%1", b"\x1b[?1049l$ ".to_vec())
        .unwrap();

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a pre-dispatch pane-readiness failure stops the current shell batch
/// after the first failed action.
///
/// Later shell siblings were never sent to the pane, so the runtime should
/// preserve them as untouched running siblings for same-turn correction rather
/// than failing every remaining shell action with the same readiness error.
#[test]
fn runtime_pane_not_ready_stops_shell_batch_after_first_failure() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-pane-not-ready".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "runtime".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: AgentTurnState::Running,
    };
    service
        .agent_turn_ledger_mut()
        .start_turn(turn.clone())
        .unwrap();
    let first = mez_agent::AgentAction {
        id: "shell-a".to_string(),
        rationale: "inspect owner one".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect owner one.".to_string(),
            command: "rg -n \"status pager\" src".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let second = mez_agent::AgentAction {
        id: "shell-b".to_string(),
        rationale: "inspect owner two".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect owner two.".to_string(),
            command: "sed -n '1,120p' src/runtime/render/mod.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "inspect with shell".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![first.clone(), second.clone()],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            mez_agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: first.id.clone(),
                action_type: "shell_command",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
            mez_agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: second.id.clone(),
                action_type: "shell_command",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution);
    service.agent_turn_contexts_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentContext::new(vec![mez_agent::ContextBlock {
            source: ContextSourceKind::Configuration,
            placement: mez_agent::ContextPlacement::StablePrefix,
            label: "test context".to_string(),
            content: "present".to_string(),
        }])
        .unwrap(),
    );
    let stored_execution = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    append_test_execution_assistant_context(&mut service, &turn, &stored_execution);
    let execution = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap()
        .expect("execution should still be present");

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("pane_not_ready")
    );
    assert_eq!(execution.action_results[1].status, ActionStatus::Running);
    assert!(!execution.action_results[1].is_error);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text
            .contains("agent: pane %1 is not ready for agent shell input: interactive-blocked"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("Inspect owner two."), "{pane_text}");
}
