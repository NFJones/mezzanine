//! Runtime tests for session processes behavior.

use super::*;
use crate::runtime::PaneSurfaceKind;
use crate::runtime::processes::terminal_features_with_progress;

/// Verifies process and agent surfaces retain independent rows and that agent
/// visibility selects only the screen bound to the active conversation.
#[test]
fn runtime_pane_screens_are_independent_and_conversation_bound() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let size = Size::new(80, 24).unwrap();
    let mut process_screen = TerminalScreen::new(size, 100).unwrap();
    process_screen.feed(b"process-only\n");
    service.set_process_pane_screen("%1", process_screen);

    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .ensure_agent_pane_screen("%1", &conversation_id, size)
        .unwrap()
        .feed(b"agent-only\n");

    let process_content = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let agent_content = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(process_content.contains("process-only"));
    assert!(!process_content.contains("agent-only"));
    assert!(agent_content.contains("agent-only"));
    assert!(!agent_content.contains("process-only"));
    assert_eq!(
        service
            .agent_pane_screen_state("%1")
            .unwrap()
            .conversation_id(),
        conversation_id
    );
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);
    assert!(
        service
            .presented_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("agent-only")
    );

    service
        .agent_shell_store_mut()
        .request_hide_pending_task_completion("%1")
        .unwrap();
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    assert_eq!(
        service.presented_pane_surface("%1"),
        PaneSurfaceKind::Process
    );
    assert!(
        service
            .presented_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("process-only")
    );

    service
        .ensure_agent_pane_screen("%1", "replacement-conversation", size)
        .unwrap();
    assert_eq!(
        service
            .agent_pane_screen_state("%1")
            .unwrap()
            .conversation_id(),
        "replacement-conversation"
    );
    assert!(
        !service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("agent-only")
    );
}

/// Verifies control capture follows the presented surface without exposing a
/// hidden process screen or accepting an agent screen from another conversation.
#[test]
fn runtime_pane_capture_uses_only_the_presented_conversation_bound_surface() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let size = Size::new(80, 24).unwrap();
    let mut process_screen = TerminalScreen::new(size, 100).unwrap();
    process_screen.feed(b"process-capture-only\r\n");
    service.set_process_pane_screen("%1", process_screen);

    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(size, 100).unwrap();
    agent_screen.feed(b"agent-capture-only\r\n");
    service.set_agent_pane_screen("%1", &conversation_id, agent_screen);

    let visible_capture = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"capture-agent","method":"pane/capture","params":{"target":{"pane_id":"%1"},"range":{"origin":"visible","start":"start","end":"end"}}}"#,
        &primary,
    );
    assert!(
        visible_capture.contains("agent-capture-only"),
        "{visible_capture}"
    );
    assert!(
        !visible_capture.contains("process-capture-only"),
        "{visible_capture}"
    );

    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let hidden_capture = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"capture-process","method":"pane/capture","params":{"target":{"pane_id":"%1"},"range":{"origin":"visible","start":"start","end":"end"}}}"#,
        &primary,
    );
    assert!(
        hidden_capture.contains("process-capture-only"),
        "{hidden_capture}"
    );
    assert!(
        !hidden_capture.contains("agent-capture-only"),
        "{hidden_capture}"
    );

    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_pane_screen("%1", "stale-conversation", {
        let mut screen = TerminalScreen::new(Size::new(80, 2).unwrap(), 100).unwrap();
        screen.feed(b"stale one\r\nstale two\r\nstale three\r\n");
        screen
    });
    let stale_history_before = service.agent_pane_screen("%1").unwrap().history().len();
    let stale_capture = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"capture-stale","method":"pane/capture","params":{"target":{"pane_id":"%1"},"range":{"origin":"visible","start":"start","end":"end"}}}"#,
        &primary,
    );
    assert!(
        !stale_capture.contains("process-capture-only"),
        "stale agent ownership must not fall back to hidden process content: {stale_capture}"
    );
    let stale_clear = service
        .execute_terminal_command(&primary, "clear-history --confirm")
        .unwrap();
    assert!(
        stale_clear.contains("cleared=false:reason=terminal-screen-unavailable"),
        "{stale_clear}"
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().history().len(),
        stale_history_before
    );
}

/// Verifies pane output reaches exactly one retained display surface for
/// verbose, trace, and ordinary post-agent process traffic.
#[test]
fn runtime_pane_output_routes_each_visible_class_to_one_surface() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let size = Size::new(80, 24).unwrap();
    let mut process_screen = TerminalScreen::new(size, 100).unwrap();
    process_screen.feed(b"process seed\r\n");
    service.set_process_pane_screen("%1", process_screen);
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(size, 100).unwrap();
    agent_screen.feed(b"agent seed\r\n");
    service.set_agent_pane_screen("%1", &conversation_id, agent_screen);
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "verbose-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-verbose".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-verbose".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf verbose".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid: 7,
                bytes: b"\x1b[2J\x1b[Hverbose agent output\r\n".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();
    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid: 7,
                bytes: b"\x1b[?1049h\x1b[4;20Htrace agent output\r\n\x1b[?1049l".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();

    let process_during_agent = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let agent_during_agent = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        process_during_agent.contains("process seed"),
        "{process_during_agent}"
    );
    assert!(
        !process_during_agent.contains("verbose agent output"),
        "{process_during_agent}"
    );
    assert!(
        !process_during_agent.contains("trace agent output"),
        "{process_during_agent}"
    );
    assert!(
        agent_during_agent.contains("agent seed"),
        "{agent_during_agent}"
    );
    assert!(
        agent_during_agent.contains("verbose agent output"),
        "{agent_during_agent}"
    );
    assert!(
        agent_during_agent.contains("trace agent output"),
        "{agent_during_agent}"
    );

    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Normal)
        .unwrap();
    let observed_before_hidden = service
        .running_shell_transactions_for_tests()
        .get("verbose-marker")
        .unwrap()
        .observed_output_bytes;
    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid: 7,
                bytes: b"hidden agent output\r\n".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    let process_after_hidden = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let agent_after_hidden = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let observed_after_hidden = service
        .running_shell_transactions_for_tests()
        .get("verbose-marker")
        .unwrap()
        .observed_output_bytes;
    assert_eq!(process_after_hidden, process_during_agent);
    assert_eq!(agent_after_hidden, agent_during_agent);
    assert!(observed_after_hidden > observed_before_hidden);

    service.running_shell_transactions_mut_for_tests().clear();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();
    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid: 7,
                bytes: b"delayed process output\r\n".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    let process_after_settlement = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let agent_after_settlement = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        process_after_settlement.contains("delayed process output"),
        "{process_after_settlement}"
    );
    assert!(
        !agent_after_settlement.contains("delayed process output"),
        "{agent_after_settlement}"
    );

    service.agent_shell_store_mut().request_exit("%1").unwrap();
    service.clear_shell_output_filters_for_foreground_input("%1");
    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid: 7,
                bytes: b"process output after exit\r\n".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();

    let process_after_exit = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let agent_after_exit = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        process_after_exit.contains("process output after exit"),
        "{process_after_exit}"
    );
    assert!(
        !agent_after_exit.contains("process output after exit"),
        "{agent_after_exit}"
    );
}

/// Verifies terminal retention settings and pane cleanup apply to both screen
/// stores without allowing one surface to outlive its pane.
#[test]
fn runtime_pane_screen_configuration_and_cleanup_cover_both_surfaces() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let size = Size::new(80, 24).unwrap();
    service.set_process_pane_screen(
        "%1",
        TerminalScreen::new_with_history_config(size, 100, 10).unwrap(),
    );
    let conversation_id = service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .ensure_agent_pane_screen("%1", &conversation_id, size)
        .unwrap();
    service
        .presentation
        .seed_agent_presentation_state_for_tests("%1", &conversation_id, size);
    assert!(
        service
            .presentation
            .has_agent_presentation_state_for_tests("%1")
    );

    service.configure_pane_screen_history(17, 3).unwrap();
    assert_eq!(
        service.process_pane_screen("%1").unwrap().history_limit(),
        17
    );
    assert_eq!(
        service
            .process_pane_screen("%1")
            .unwrap()
            .history_rotate_lines(),
        3
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap().history_limit(), 17);
    assert_eq!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .history_rotate_lines(),
        3
    );

    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    assert!(service.process_pane_screen("%1").is_none());
    assert!(service.agent_pane_screen("%1").is_none());
    assert!(
        !service
            .presentation
            .has_agent_presentation_state_for_tests("%1")
    );
}

/// Verifies closing a pane immediately removes its durable active-session
/// metadata instead of waiting for a later checkpoint-triggering event.
#[test]
fn runtime_pane_close_immediately_checkpoints_remaining_agent_sessions() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("pane-close-agent-checkpoint"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    assert_eq!(
        transcript_store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()
            .len(),
        2
    );

    service
        .execute_terminal_command(&primary, "kill-pane --force -t %2")
        .unwrap();

    let metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(metadata.len(), 1, "{metadata:#?}");
    assert_eq!(metadata[0].pane_id, "%1");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies terminal-generated response bytes are forwarded back to the pane.
///
/// CSI 6n is a pane application query, not visible output. When the terminal
/// parser emits a cursor-position report, the runtime must write that reply to
/// the pane input path so full-screen applications waiting on CPR can continue.
#[test]
fn runtime_pane_output_device_status_report_is_forwarded_to_pane_input() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let _process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    service
        .apply_pane_output_bytes(pane_id.clone(), b"\x1b[3;5H\x1b[6n".to_vec())
        .unwrap();

    let deferred = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].pane_input_parts().0, pane_id);
    assert_eq!(deferred[0].pane_input_parts().1, b"\x1b[3;5R");
}

/// Verifies native shell context inference reads the root-process working
/// directory directly for adapter-owned panes after the synchronous process
/// record has been detached. This guards native mode against failing with
/// "requires a readable root-process working directory" after adapter handoff
/// while permitting the independently best-effort environment reader to be
/// empty during a process-exec race.
#[test]
fn runtime_native_shell_context_resolves_adapter_owned_pane_metadata() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let _process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    let primary_pid = service.primary_pid_for_live_pane_process(&pane_id).unwrap();
    let expected_working_directory =
        mez_mux::process::current_working_directory_for_pid(primary_pid).unwrap();

    let context = service.native_shell_context_for_pane(&pane_id).unwrap();
    assert_eq!(
        context.working_directory(),
        expected_working_directory.as_path(),
        "adapter-owned pane working directory should come from the live root process"
    );
    assert!(
        !context.shell_path().as_os_str().is_empty(),
        "adapter-owned pane shell path should resolve from host metadata"
    );
}

/// Verifies hidden retained shell traffic still updates the incremental
/// terminal protocol observer and emits replies without reaching either
/// retained display surface.
#[test]
fn runtime_hidden_retained_shell_output_preserves_terminal_protocol() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let _process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .process_pane_screen_mut(&pane_id)
        .unwrap()
        .feed(b"process-visible");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap()
        .session_id
        .clone();
    service
        .ensure_agent_pane_screen(&pane_id, &conversation_id, Size::new(80, 24).unwrap())
        .unwrap()
        .feed(b"agent-visible");
    service.remember_hidden_shell_render_suppression(&pane_id);

    service
        .apply_pane_output_bytes(pane_id.clone(), b"\x1b[?1000;1006;2004".to_vec())
        .unwrap();
    service
        .apply_pane_output_bytes(
            pane_id.clone(),
            b"h\x1b[?1004h\x1b=\x1b[3;5H\x1b[6n".to_vec(),
        )
        .unwrap();

    let observer = service
        .pane_transaction_osc_screens_for_tests()
        .get(&pane_id)
        .expect("hidden protocol observer should be retained");
    assert!(observer.application_mouse_enabled());
    assert!(observer.application_sgr_mouse_enabled());
    assert!(observer.bracketed_paste_enabled());
    assert!(observer.focus_events_enabled());
    assert!(observer.application_keypad_enabled());
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .matches("process-visible")
            .count(),
        1
    );
    assert_eq!(
        service
            .agent_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .matches("agent-visible")
            .count(),
        1
    );
    let deferred = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].pane_input_parts().0, pane_id);
    assert_eq!(deferred[0].pane_input_parts().1, b"\x1b[3;5R");
}

/// Verifies hidden agent-action traffic remains invisible while fragmented
/// terminal modes and required terminal replies are processed incrementally.
#[test]
fn runtime_hidden_agent_action_output_preserves_terminal_protocol() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let _process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .process_pane_screen_mut(&pane_id)
        .unwrap()
        .feed(b"process-visible");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap()
        .session_id
        .clone();
    service
        .ensure_agent_pane_screen(&pane_id, &conversation_id, Size::new(80, 24).unwrap())
        .unwrap()
        .feed(b"agent-visible");
    service.running_shell_transactions_mut_for_tests().insert(
        "hidden-action-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-hidden-action".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-hidden-action".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "printf hidden".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    service
        .apply_pane_output_bytes(pane_id.clone(), b"\x1b[?1000;1006;2004".to_vec())
        .unwrap();
    service
        .apply_pane_output_bytes(
            pane_id.clone(),
            b"h\x1b[?1004h\x1b=\x1b[3;5H\x1b[6n".to_vec(),
        )
        .unwrap();

    let observer = service
        .pane_transaction_osc_screens_for_tests()
        .get(&pane_id)
        .expect("hidden agent-action protocol observer should be retained");
    assert!(observer.application_mouse_enabled());
    assert!(observer.application_sgr_mouse_enabled());
    assert!(observer.bracketed_paste_enabled());
    assert!(observer.focus_events_enabled());
    assert!(observer.application_keypad_enabled());
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .matches("process-visible")
            .count(),
        1
    );
    assert_eq!(
        service
            .agent_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .matches("agent-visible")
            .count(),
        1
    );
    let deferred = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].pane_input_parts().0, pane_id);
    assert_eq!(deferred[0].pane_input_parts().1, b"\x1b[3;5R");
}

/// Verifies fragmented private shell-output frames retain protocol modes while
/// preserving the authoritative process screen's shell cursor.
#[test]
fn runtime_hidden_encoded_agent_action_updates_authoritative_process_protocol() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let _process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .process_pane_screen_mut(&pane_id)
        .unwrap()
        .feed(b"process-visible");
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap()
        .session_id
        .clone();
    service
        .ensure_agent_pane_screen(&pane_id, &conversation_id, Size::new(80, 24).unwrap())
        .unwrap()
        .feed(b"agent-visible");
    service.running_shell_transactions_mut_for_tests().insert(
        "encoded-action-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-encoded-action".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-encoded-action".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "printf encoded".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    service.register_encoded_shell_output_transaction("encoded-action-marker");
    let framed = b"__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\nG1s/MTAwMGgbWz8xMDA2aBtbPzIwMDRoG1s/MTAwNGgbPRtbMzs1SBtbNm4bWz8xMDQ5aGhpZGRlbhtbPzEwNDls\n__MEZ_SHELL_OUTPUT_BASE64_END__\n";
    for fragment in [&framed[..23], &framed[23..79], &framed[79..]] {
        service
            .apply_pane_output_bytes(pane_id.clone(), fragment.to_vec())
            .unwrap();
    }

    let process_screen = service.process_pane_screen(&pane_id).unwrap();
    assert!(process_screen.application_mouse_enabled());
    assert!(process_screen.application_sgr_mouse_enabled());
    assert!(process_screen.bracketed_paste_enabled());
    assert!(process_screen.focus_events_enabled());
    assert!(process_screen.application_keypad_enabled());
    assert!(!process_screen.alternate_screen_active());
    assert_eq!(process_screen.cursor_state().row, 0);
    assert_eq!(process_screen.cursor_state().column, 15);
    assert_eq!(
        process_screen
            .normal_content_lines()
            .join("\n")
            .matches("process-visible")
            .count(),
        1
    );
    assert!(
        !process_screen
            .normal_content_lines()
            .join("\n")
            .contains("hidden")
    );
    assert_eq!(
        service
            .agent_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .matches("agent-visible")
            .count(),
        1
    );
    let deferred = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].pane_input_parts().0, pane_id);
    assert_eq!(deferred[0].pane_input_parts().1, b"\x1b[3;5R");
}

/// Verifies hidden PTY output retains the process screen's unreserved
/// presentation geometry while the PTY and agent screen use prompt-reserved
/// interaction geometry.
///
/// Output ingestion must not undo the independent sizing contract established
/// by layout synchronization. Shrinking the retained process screen on every
/// hidden chunk causes repeated row movement and history work when later layout
/// or prompt updates restore the presentation size.
#[test]
fn runtime_hidden_output_preserves_process_presentation_geometry() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service
        .reload_agent_prompt_history_for_pane(&pane_id)
        .unwrap();
    service.sync_tracked_pty_sizes().unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "hidden-geometry-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-hidden-geometry".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-hidden-geometry".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "printf hidden".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let window = service.session().active_window().unwrap();
    let presentation_size = service
        .pane_presentation_size_for(window, &pane_id)
        .unwrap();
    let interaction_size = service.pane_process_size_for(window, &pane_id).unwrap();
    assert_ne!(presentation_size, interaction_size);
    assert_eq!(
        service.process_pane_screen(&pane_id).unwrap().size(),
        presentation_size
    );

    service
        .apply_pane_output_bytes(pane_id.clone(), b"hidden output\r\n".to_vec())
        .unwrap();

    assert_eq!(
        service.process_pane_screen(&pane_id).unwrap().size(),
        presentation_size
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that runtime frame context sources `pane.process_name` from the
/// live host process metadata instead of only echoing the configured shell path.
#[cfg(target_os = "linux")]
/// Verifies runtime frame context uses host process name when available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_frame_context_uses_host_process_name_when_available() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(Some("sleep 2")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();

    let mut process_name = None;
    for _ in 0..10_000 {
        process_name = service.pane_processes().process_name(&pane_id);
        if process_name.as_deref() == Some("sleep") {
            break;
        }
        thread::yield_now();
    }

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(process_name.as_deref(), Some("sleep"));
    assert_eq!(pane_context.process_name.as_deref(), Some("sleep"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a failed new-window process spawn is transactional. The window
/// is inserted before the PTY spawn path runs, so a spawn-layer failure must
/// restore the previous window list and active-window selection instead of
/// leaving a processless pane behind for later rendering or input dispatch.
#[test]
fn runtime_new_window_spawn_failure_rolls_back_window_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let active_window_id = service.session().active_window().unwrap().id.clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-new-window"),
        ShellSource::FallbackBinSh,
    )
    .into();

    let error = service
        .create_window_with_pane_process(&primary, "bad", true, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert_eq!(service.session().windows().len(), 1);
    assert_eq!(
        service.session().active_window().unwrap().id,
        active_window_id
    );
    assert!(service.pane_processes().is_empty());
}

/// Verifies that a failed split process spawn restores the pre-split layout.
/// Existing panes are resized before the new pane process is started, so the
/// rollback must also return the active pane geometry to its original size and
/// leave only the already-running process tracked by the runtime.
#[test]
fn runtime_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-split"),
        ShellSource::FallbackBinSh,
    )
    .into();

    let error = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that terminal-command splits use the same transactional runtime
/// helper as direct mux/control splits. A failed process spawn must restore the
/// pre-split layout instead of leaving a processless command-created pane with
/// stale geometry behind.
#[test]
fn runtime_terminal_command_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-command-split"),
        ShellSource::FallbackBinSh,
    )
    .into();

    let error = service
        .execute_terminal_command(&primary, "split-window")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies pane-move terminal commands synchronize screens through the
/// runtime-owned resize adapters. Break and join must apply the explicit
/// session effects instead of relying on generic post-dispatch rediscovery.
#[test]
fn runtime_terminal_pane_move_commands_apply_resize_effects() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap();

    service
        .execute_terminal_command(&primary, "swapp -U")
        .unwrap();
    service
        .execute_terminal_command(&primary, "breakp -n moved")
        .unwrap();
    assert_eq!(service.session().windows().len(), 2);
    let broken_size = service.pane_screen("%2").unwrap().size();

    service
        .execute_terminal_command(&primary, "joinp -t 0 --select")
        .unwrap();
    assert_eq!(service.session().windows().len(), 1);
    assert!(service.pane_screen("%2").unwrap().size().columns < broken_size.columns);

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies ordinary pane creation defaults to home and a live policy refresh
/// makes later panes inherit the source pane's tracked working directory.
#[test]
fn runtime_pane_spawn_directory_policy_applies_to_future_spawns() {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert!(home.is_dir());
    let source = temp_root("runtime-pane-spawn-same-directory");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let home_started = service
        .create_window_with_pane_process(&primary, "home", true, Some("true"))
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&home_started.pane_id)
            .as_deref(),
        Some(home.as_path())
    );

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\npane_spawn_directory = \"same-directory\"\n".to_string(),
        }])
        .unwrap();
    service.set_pane_current_working_directory(home_started.pane_id.clone(), source.clone());
    let inherited = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("true"))
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&inherited.pane_id)
            .as_deref(),
        Some(source.as_path())
    );

    poll_until_exit(&mut service);
    let _ = fs::remove_dir_all(source);
}

/// Verifies stale source directories fall back to home and an explicit spawn
/// directory takes precedence over either configured policy.
#[test]
fn runtime_pane_spawn_directory_policy_falls_back_and_preserves_explicit_precedence() {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert!(home.is_dir());
    let root = temp_root("runtime-pane-spawn-explicit-directory");
    let explicit = root.join("explicit");
    fs::create_dir_all(&explicit).unwrap();
    let stale = root.join("missing");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\npane_spawn_directory = \"same-directory\"\n".to_string(),
        }])
        .unwrap();
    service.set_pane_current_working_directory("%1".to_string(), stale);

    let fallback = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("true"))
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&fallback.pane_id)
            .as_deref(),
        Some(home.as_path())
    );
    let explicit_started = service
        .create_window_with_pane_process_with_options(
            &primary,
            "explicit",
            true,
            Some("true"),
            Some(&explicit),
            None,
        )
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&explicit_started.pane_id)
            .as_deref(),
        Some(explicit.as_path())
    );

    poll_until_exit(&mut service);
    let _ = fs::remove_dir_all(root);
}

/// Verifies ordinary pane creation retains shell view by default and a live
/// policy refresh makes only later panes enter the agent surface. The process
/// screen must remain present so hiding the agent restores the shell view.
#[test]
fn runtime_pane_spawn_view_policy_applies_to_future_spawns() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let shell_started = service
        .create_window_with_pane_process(&primary, "shell", true, Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .process_pane_screen(&shell_started.pane_id)
            .is_some()
    );
    assert!(service.agent_pane_screen(&shell_started.pane_id).is_none());
    assert!(
        service
            .agent_shell_store()
            .get(&shell_started.pane_id)
            .is_none()
    );

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\npane_spawn_view = \"agent\"\n".to_string(),
        }])
        .unwrap();
    let agent_started = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .process_pane_screen(&agent_started.pane_id)
            .is_some()
    );
    assert!(service.agent_pane_screen(&agent_started.pane_id).is_some());
    assert_eq!(
        service
            .agent_shell_store()
            .get(&agent_started.pane_id)
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert_eq!(
        service.presented_pane_surface(&agent_started.pane_id),
        PaneSurfaceKind::Agent
    );

    service
        .agent_shell_store_mut()
        .request_exit(&agent_started.pane_id)
        .unwrap();
    assert_eq!(
        service.presented_pane_surface(&agent_started.pane_id),
        PaneSurfaceKind::Process
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the agent spawn-view policy covers ordinary window and group
/// creation without stealing focus when selection is disabled. Initial pane
/// startup remains shell-view because it is outside ordinary creation.
#[test]
fn runtime_pane_spawn_view_policy_covers_ordinary_creation_only() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\npane_spawn_view = \"agent\"\n".to_string(),
        }])
        .unwrap();

    let initial = service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(service.agent_shell_store().get(&initial.pane_id).is_none());
    let active_window_id = service.session().active_window().unwrap().id.clone();

    let window_started = service
        .create_window_with_pane_process(&primary, "agent-window", false, Some("cat >/dev/null"))
        .unwrap();
    assert_eq!(
        service.session().active_window().unwrap().id,
        active_window_id
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&window_started.pane_id)
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );

    let group_started = service
        .create_group_with_pane_process(
            &primary,
            "agent-group",
            false,
            Some("cat >/dev/null"),
            None,
        )
        .unwrap();
    assert_eq!(
        service.session().active_window().unwrap().id,
        active_window_id
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&group_started.pane_id)
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the initial pane process starts in the caller's launch directory.
///
/// This regression protects the detached-daemon startup handoff: the process
/// must use the directory captured at launch rather than an incidental later
/// working directory.
#[test]
fn runtime_initial_pane_process_preserves_explicit_launch_directory() {
    let root = std::env::temp_dir().join(format!(
        "mez-initial-pane-launch-directory-{}",
        std::process::id()
    ));
    let launch_directory = root.join("launch");
    let output = root.join("pwd.txt");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&launch_directory).unwrap();

    let mut service = test_runtime_service();
    let command = format!("pwd > {}", output.display());
    let started = service
        .start_initial_pane_process_with_start_directory(Some(&command), &launch_directory)
        .unwrap();

    assert_eq!(
        service
            .pane_current_working_directory(&started.pane_id)
            .as_deref(),
        Some(launch_directory.as_path())
    );
    poll_until_exit(&mut service);
    assert_eq!(
        Path::new(fs::read_to_string(&output).unwrap().trim())
            .canonicalize()
            .unwrap(),
        launch_directory.canonicalize().unwrap()
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime service starts initial pane process through resolved shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_initial_pane_process_through_resolved_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let started = service.start_initial_pane_process(Some("true")).unwrap();

    assert_eq!(started.session_id, service.session().id.to_string());
    assert_eq!(started.window_id, "@1");
    assert_eq!(started.pane_id, "%1");
    assert!(started.primary_pid > 0);
    assert_eq!(
        service.pane_processes().primary_pid("%1"),
        Some(started.primary_pid)
    );
    assert!(matches!(
        started.registry_update,
        RuntimeRegistryUpdatePlan::Upsert(_)
    ));

    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::PaneChanged
                && event.payload.contains(r#""process_state":"running""#))
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::Diagnostic
                && event.payload.contains("fell back to /bin/sh"))
    );

    let _ = primary;
    poll_until_exit(&mut service);
}

/// Verifies that runtime services can hand a running pane process to an async
/// owner and restore it if the handoff is cancelled. The service keeps session
/// and terminal metadata while only the process/PTY handle leaves the
/// synchronous manager.
#[test]
fn runtime_service_can_handoff_running_pane_process_to_async_owner() {
    let mut service = test_runtime_service();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();

    let process = service
        .take_running_pane_process_for_adapter(&started.pane_id)
        .unwrap();

    assert!(!service.pane_processes().contains_pane(&started.pane_id));
    let window = service.session().active_window().unwrap();
    let pane_state = service.runtime_control_pane_state_json(window, window.active_pane());
    assert!(
        pane_state.contains(&format!(r#""primary_pid":{}"#, started.primary_pid)),
        "{pane_state}"
    );
    assert!(
        pane_state.contains(r#""process_state":"running""#),
        "{pane_state}"
    );
    service
        .apply_pane_foreground_process_event(
            &started.pane_id,
            "vim",
            started.primary_pid.saturating_add(1),
            Some("/tmp/mez-async-cwd".to_string()),
        )
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&started.pane_id)
            .as_deref(),
        Some(Path::new("/tmp/mez-async-cwd"))
    );
    assert_eq!(
        service
            .restore_running_pane_process_from_adapter(&started.pane_id, process)
            .unwrap(),
        started.primary_pid
    );
    assert_eq!(
        service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    service
        .pane_processes_mut()
        .terminate_pane_with_grace(&started.pane_id, Duration::from_millis(50))
        .unwrap();
}

/// Verifies control pane state reports the actual PTY size after pane-frame
/// and agent-prompt reservations while retaining the unreserved layout size.
/// Terminal clients use the reported PTY dimensions for wrapping, so exposing
/// layout geometry here would make their width and height disagree with the
/// running pane process.
#[test]
fn runtime_control_pane_state_reports_frame_and_prompt_adjusted_pty_size() {
    let mut service = test_runtime_service_with_size(Size::new(80, 24).unwrap());
    service.set_frame_visibility_for_tests(false, true);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    let pty_size = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap()
        .size;
    let window = service.session().active_window().unwrap();
    let layout_size = window.active_pane().size;
    let pane_state = service.runtime_control_pane_state_json(window, window.active_pane());

    assert!(pty_size.rows < layout_size.rows);
    assert!(pane_state.contains(&format!(
        r#""size":{{"columns":{},"rows":{}}}"#,
        pty_size.columns, pty_size.rows
    )));
    assert!(pane_state.contains(&format!(
        r#""columns":{},"rows":{}"#,
        pty_size.columns, pty_size.rows
    )));
    assert!(pane_state.contains(&format!(
        r#""layout_size":{{"columns":{},"rows":{}}}"#,
        layout_size.columns, layout_size.rows
    )));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies stale async process-exit events cannot close a pane after its id is reused.
///
/// `load-layout` can restart a fresh process for a restored pane id while an
/// older async watcher still holds a late exit event for the previous process.
/// The runtime must compare the event's primary PID with the currently live
/// primary PID and ignore mismatches so the new pane generation remains live.
#[test]
fn runtime_service_ignores_stale_process_exit_with_mismatched_primary_pid() {
    let mut service = test_runtime_service();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    let stale_primary_pid = started.primary_pid.saturating_add(1);

    let update = service
        .apply_pane_process_exit_event(
            &started.pane_id,
            stale_primary_pid,
            mez_mux::process::PaneExitStatus {
                code: Some(0),
                signal: None,
                success: true,
            },
        )
        .unwrap();

    assert_eq!(update, None);
    assert_eq!(
        service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.id.as_str() == started.pane_id.as_str() && pane.live)
    );
    service
        .pane_processes_mut()
        .terminate_pane_with_grace(&started.pane_id, Duration::from_millis(50))
        .unwrap();
}

/// Verifies late pane output after a pane exit event is ignored.
///
/// Full-screen and alternate-screen applications commonly emit shutdown bytes
/// while the PTY is exiting. Once runtime teardown removes the pane from the
/// session, those late bytes must be treated as a normal shutdown race instead
/// of a fatal missing-pane error.
#[test]
fn runtime_service_ignores_late_pane_output_after_exit_event() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.process.pane_terminal_progress.insert(
        started.pane_id.clone(),
        mez_terminal::TerminalProgressState::Normal { percent: 42 },
    );

    let update = service
        .apply_pane_process_exit_event(
            &started.pane_id,
            started.primary_pid,
            mez_mux::process::PaneExitStatus {
                code: Some(0),
                signal: None,
                success: true,
            },
        )
        .unwrap();

    assert!(update.is_some());
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.id.as_str() != started.pane_id.as_str())
    );
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.id.as_str() == second_pane.as_str())
    );
    assert!(
        !service
            .process
            .pane_terminal_progress
            .contains_key(started.pane_id.as_str())
    );

    let late_output = service
        .apply_pane_output_bytes(started.pane_id.clone(), b"\x1b[?1004l\x1b[?1049l".to_vec())
        .unwrap();

    assert_eq!(late_output, None);
}

/// Verifies runtime service restarts restored panes with fresh primary pids.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_restarts_restored_panes_with_fresh_primary_pids() {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    assert!(
        restored
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| !pane.live)
    );
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();

    assert_eq!(starts.len(), 2);
    assert!(starts.iter().all(|start| start.primary_pid > 0));
    assert_ne!(starts[0].primary_pid, starts[1].primary_pid);
    assert_eq!(service.pane_processes().len(), 2);
    assert!(starts.iter().all(|start| {
        service.pane_readiness_state(&start.pane_id) == PaneReadinessState::Unknown
    }));
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.live)
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""restarted":true"#))
    );
    poll_until_exit(&mut service);
}

/// Verifies a restored pane that still owns a durable pane-to-agent binding is
/// re-created through the agent-owned creation path, while a restored pane with
/// no durable binding keeps inheriting the daemon environment.
///
/// Snapshot restore restores durable agent session metadata before pane
/// processes are restarted, so the durable binding decides the restored pane's
/// purpose and a bound restored pane root starts from the documented
/// pane-creation allowlist instead of the daemon environment. The mocked daemon
/// sentinel cannot discriminate the purpose on its own because a user shell
/// pane never consults that snapshot, so the ambient probe that the agent-owned
/// boundary drops is the discriminating evidence. The cleared base covers the
/// launch environment only: a bound restored pane still owns an unvalidated
/// native startup surface until its first agent entry.
#[test]
fn runtime_restored_agent_bound_pane_clears_daemon_environment_while_unbound_restored_pane_inherits_it()
 {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let restored_pane_ids = restored
        .windows()
        .iter()
        .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
        .collect::<Vec<_>>();
    assert_eq!(restored_pane_ids.len(), 2);
    let bound_pane_id = restored_pane_ids[0].clone();
    let unbound_pane_id = restored_pane_ids[1].clone();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-agent-binding.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let transcript_root = temp_root("runtime-restored-agent-binding");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[mez_agent::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: bound_pane_id.clone(),
                conversation_id: "restored-agent-bound".to_string(),
                prompt_cache_lineage_id: "lineage-restored-agent-bound".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                running_turn_kind: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                directive: None,
                routing_enabled: None,
                root_routing_policy: None,
                approval_policy: None,
                pane_permission_preset_override: None,
                pane_approval_policy_override: None,
                working_directory: None,
                project_root: None,
                context_usage: None,
                context_usage_snapshot: None,
                latest_request_usage: None,
                token_usage: Default::default(),
                token_usage_by_model: Default::default(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store);

    let mut daemon_environment = crate::runtime::processes::native_ambient_environment();
    daemon_environment.push(mez_mux::process::RawEnvironmentEntry {
        key: b"MEZ_AGENT_OWNED_DAEMON_SENTINEL".to_vec(),
        value: b"daemon-only".to_vec(),
    });
    let probe = crate::runtime::processes::daemon_only_probe_key_for_tests(
        &daemon_environment,
        Path::new("/bin/sh"),
    )
    .expect("the injected daemon environment must expose a daemon-only variable");
    service.inject_agent_owned_pane_daemon_environment_for_tests(daemon_environment);

    assert_eq!(
        service
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        1
    );
    assert_eq!(
        service.restored_pane_process_purpose(&bound_pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::AgentOwned {
            shell_mode: crate::runtime::config::ShellMode::Native,
        },
        "the workspace default selects native mode for a bound restored pane"
    );
    assert_eq!(
        service.restored_pane_process_purpose(&unbound_pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::UserShell,
        "a restored pane without a durable binding remains a user shell"
    );

    let starts = service.restart_restored_pane_processes(None).unwrap();
    assert_eq!(starts.len(), 2);

    let bound_process = service
        .take_running_pane_process_for_adapter(&bound_pane_id)
        .unwrap();
    let unbound_process = service
        .take_running_pane_process_for_adapter(&unbound_pane_id)
        .unwrap();
    let bound_environment = pane_root_exec_environment(&bound_process);
    let unbound_environment = pane_root_exec_environment(&unbound_process);

    assert!(
        environment_forwards(&unbound_environment, &probe),
        "an unbound restored pane must keep inheriting the daemon environment"
    );
    assert!(
        !environment_forwards_key(&bound_environment, &probe.key),
        "daemon-only {} must not reach a bound restored pane root",
        String::from_utf8_lossy(&probe.key)
    );
    assert!(
        !environment_forwards_key(&bound_environment, b"MEZ_AGENT_OWNED_DAEMON_SENTINEL"),
        "the mocked daemon sentinel must not reach a bound restored pane root"
    );
    assert!(
        environment_forwards_key(&bound_environment, b"PATH"),
        "the bound restored pane root must still receive a working PATH"
    );

    let native_environment = service
        .native_shell_context_for_pane(&bound_pane_id)
        .unwrap()
        .launch_environment()
        .to_vec();
    assert!(
        !environment_forwards_key(&native_environment, &probe.key),
        "native evidence must not carry the daemon-only {}",
        String::from_utf8_lossy(&probe.key)
    );
    assert!(
        !environment_forwards_key(&native_environment, b"MEZ_AGENT_OWNED_DAEMON_SENTINEL"),
        "native evidence must not carry the mocked daemon sentinel"
    );

    assert_eq!(
        service.presented_pane_surface(&bound_pane_id),
        PaneSurfaceKind::Agent
    );
    assert_eq!(
        service.presented_pane_surface(&unbound_pane_id),
        PaneSurfaceKind::Process
    );
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&bound_pane_id),
        Some("native-validating"),
        "a bound restored pane must stay unvalidated until its first agent entry"
    );
    assert!(
        !service.pane_environment_authority_is_certified_for_tests(&bound_pane_id),
        "native mode must not certify a restored pane's environment authority"
    );
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&unbound_pane_id),
        None
    );

    drop(bound_process);
    drop(unbound_process);
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies runtime service restarts restored panes at the rendered PTY size
/// instead of the raw saved layout pane size.
///
/// Restored shells must start with the same content-area dimensions used by
/// normal pane creation so cursor placement and shell redraws stay aligned with
/// framed and split layouts immediately after `load-layout`.
#[test]
fn runtime_service_restarts_restored_panes_with_rendered_process_sizes() {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let restored_pane_sizes: Vec<Size> = restored
        .windows()
        .iter()
        .flat_map(|window| window.panes().iter().map(|pane| pane.size))
        .collect();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-size-alignment.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();
    let started_sizes: Vec<Size> = starts.iter().map(|start| start.size).collect();
    let tracked_sizes: Vec<Size> = service
        .tracked_pane_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.size)
        .collect();

    assert_eq!(started_sizes.len(), restored_pane_sizes.len());
    assert_eq!(started_sizes, tracked_sizes);
    assert_ne!(started_sizes, restored_pane_sizes);
    poll_until_exit(&mut service);
}

/// Verifies runtime snapshot resume treats saved pane working directories as
/// best-effort metadata when fresh pane process startup cannot use them.
///
/// A snapshot can contain a directory that existed during restore planning but
/// becomes unusable when the fresh pane process starts. Resume must keep the
/// restored layout and names, retry the pane from the user's home directory,
/// and leave the restored session usable instead of unwinding after topology
/// installation.
#[test]
fn runtime_service_restarts_restored_panes_from_home_when_saved_cwd_fails() {
    let root = temp_root("runtime-restored-pane-cwd-fallback");
    let inaccessible_cwd = root.join("inaccessible-cwd");
    fs::create_dir_all(&inaccessible_cwd).unwrap();
    let original_permissions = fs::metadata(&inaccessible_cwd).unwrap().permissions();
    fs::set_permissions(&inaccessible_cwd, fs::Permissions::from_mode(0o000)).unwrap();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert!(home.is_dir());

    let original = test_session();
    let mut payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    payload.name = "restored-name".to_string();
    payload.windows[0].name = "saved-window".to_string();
    payload.windows[0].panes[0].title = "saved-pane".to_string();
    payload.windows[0].panes[0].current_working_directory =
        Some(inaccessible_cwd.to_string_lossy().into_owned());
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let pane_id = restored.active_window().unwrap().active_pane().id.clone();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-cwd-fallback.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();

    assert_eq!(starts.len(), 1);
    assert_eq!(service.session().name, "restored-name");
    assert_eq!(
        service.session().active_window().unwrap().name,
        "saved-window"
    );
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "saved-pane"
    );
    assert_eq!(
        service
            .pane_current_working_directory(pane_id.as_str())
            .as_deref(),
        Some(home.as_path())
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event
            .payload
            .contains("snapshot resume pane cwd unavailable; retrying from home")
    }));

    poll_until_exit(&mut service);
    fs::set_permissions(&inaccessible_cwd, original_permissions).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Clipboard children must receive the session proxy environment both when
/// the proxy is installed and when configuration replaces the adapter. Shell
/// helpers observe copy and paste without accessing a desktop clipboard, and
/// a separate client adapter must retain its original inherited environment.
#[tokio::test(flavor = "current_thread")]
async fn runtime_x11_environment_reaches_clipboard_after_install_and_reload() {
    use crate::host::terminal::{
        HostClipboard, HostClipboardCommand, read_host_clipboard_plan_async,
    };

    let root = temp_root("x11-clipboard-environment");
    let output = root.join("clipboard.txt");
    let command = HostClipboardCommand::new("sh", vec![
        "-c".to_string(),
        "cat >/dev/null; printf '%s\\n%s\\n' \"${DISPLAY-unset}\" \"${XAUTHORITY-unset}\" > \"$1\"".to_string(),
        "clipboard-test".to_string(),
        output.to_string_lossy().into_owned(),
    ]);
    let read = HostClipboardCommand::new(
        "sh",
        vec![
            "-c".to_string(),
            "printf '%s\\n%s\\n' \"${DISPLAY-unset}\" \"${XAUTHORITY-unset}\"".to_string(),
        ],
    );
    let clipboard = HostClipboard::configured(Some(command), Some(read))
        .with_read_limits(Duration::from_secs(2), 4096);
    let client_before = read_host_clipboard_plan_async(clipboard.read_plan()).await;
    let proxy = crate::runtime::x11::RuntimeX11Proxy::prepare(&root).unwrap();
    let handle = proxy.handle();
    let expected = format!(
        "{}\n{}\n",
        handle.display(),
        handle.authority_path().display()
    );
    let mut service = test_runtime_service();
    service.set_host_clipboard(clipboard.clone());
    service.set_runtime_x11_proxy(handle);

    for replacement in [false, true] {
        if replacement {
            fs::remove_file(&output).unwrap();
            service.set_host_clipboard(clipboard.clone());
        }
        service
            .copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                "payload".to_string(),
                "test".to_string(),
                false,
            )
            .unwrap();
        assert_eq!(wait_for_x11_environment_file(&output), expected);
        assert_eq!(
            read_host_clipboard_plan_async(service.host_clipboard_for_tests().read_plan()).await,
            Some(expected.clone())
        );
    }
    assert_eq!(
        read_host_clipboard_plan_async(clipboard.read_plan()).await,
        client_before
    );
    fs::remove_file(&output).unwrap();
    service
        .copy_text_to_buffer_and_host_clipboard(
            "clipboard",
            "client-routed".to_string(),
            "test".to_string(),
            true,
        )
        .unwrap();
    assert!(
        !output.exists(),
        "client-routed copies must suppress the server helper"
    );
    drop(service);
    drop(proxy);
    let _ = fs::remove_dir_all(root);
}

/// An installed clipboard helper can fail after spawning (for example xclip
/// without a usable display). The asynchronous copy worker must then execute
/// the next helper rather than treating process creation as completed copy.
#[test]
fn runtime_clipboard_copy_falls_back_after_command_failure() {
    use crate::host::terminal::{HostClipboard, HostClipboardCommand};

    let root = temp_root("clipboard-copy-fallback");
    let output = root.join("copied.txt");
    let clipboard = HostClipboard::commands(
        vec![
            HostClipboardCommand::new(
                "sh",
                vec!["-c".to_string(), "cat >/dev/null; exit 1".to_string()],
            ),
            HostClipboardCommand::new(
                "sh",
                vec![
                    "-c".to_string(),
                    "cat > \"$1\"".to_string(),
                    "clipboard-test".to_string(),
                    output.to_string_lossy().into_owned(),
                ],
            ),
        ],
        Vec::new(),
    );
    assert!(clipboard.copy("first\nsecond\n"));
    assert_eq!(wait_for_x11_environment_file(&output), "first\nsecond\n");
    let _ = fs::remove_dir_all(root);
}

/// Protected X11 environment values must reach every later window and split
/// through the same centralized pane-start boundary as the initial process.
#[tokio::test(flavor = "current_thread")]
async fn runtime_x11_environment_reaches_later_windows_and_splits() {
    let root = temp_root("x11-later-pane-environment");
    let window_output = root.join("window-x11.txt");
    let split_output = root.join("split-x11.txt");
    let proxy = crate::runtime::x11::RuntimeX11Proxy::prepare(&root).unwrap();
    let proxy_handle = proxy.handle();
    let expected_display = proxy_handle.display().to_string();
    let expected_authority = proxy_handle.authority_path().to_path_buf();
    let mut service = test_runtime_service();
    service.set_runtime_x11_proxy(proxy_handle);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    service
        .create_window_with_pane_process(
            &primary,
            "x11-window",
            true,
            Some(&format!(
                "printf '%s\\n%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" > {}; sleep 30",
                window_output.to_string_lossy()
            )),
        )
        .unwrap();
    service
        .split_pane_with_process(
            &primary,
            SplitDirection::Vertical,
            Some(&format!(
                "printf '%s\\n%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" > {}; sleep 30",
                split_output.to_string_lossy()
            )),
        )
        .unwrap();

    for output in [&window_output, &split_output] {
        let observed = wait_for_x11_environment_file(output);
        let values = observed.lines().collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                expected_display.as_str(),
                expected_authority.to_string_lossy().as_ref()
            ]
        );
    }

    service.terminate_all_pane_processes().unwrap();
    drop(proxy);
    let _ = fs::remove_dir_all(root);
}

/// Snapshot-restored panes must inherit the same stable X11 environment as
/// fresh panes instead of restarting without the session proxy contract.
#[tokio::test(flavor = "current_thread")]
async fn runtime_x11_environment_reaches_restored_panes() {
    let root = temp_root("x11-restored-pane-environment");
    let output = root.join("restored-x11.txt");
    let original = test_session();
    let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let mut service =
        RuntimeSessionService::with_event_log(restored, root.join("restored.sock"), 100, 10, 1024)
            .unwrap();
    let proxy = crate::runtime::x11::RuntimeX11Proxy::prepare(&root).unwrap();
    let proxy_handle = proxy.handle();
    let expected_display = proxy_handle.display().to_string();
    let expected_authority = proxy_handle.authority_path().to_path_buf();
    service.set_runtime_x11_proxy(proxy_handle);

    let starts = service
        .restart_restored_pane_processes(Some(&format!(
            "printf '%s\\n%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" > {}; sleep 30",
            output.to_string_lossy()
        )))
        .unwrap();

    assert_eq!(starts.len(), 1);
    let observed = wait_for_x11_environment_file(&output);
    let values = observed.lines().collect::<Vec<_>>();
    assert_eq!(
        values,
        [
            expected_display.as_str(),
            expected_authority.to_string_lossy().as_ref()
        ]
    );

    service.terminate_all_pane_processes().unwrap();
    drop(proxy);
    let _ = fs::remove_dir_all(root);
}

/// Waits boundedly for one pane command to publish its two-line X11
/// environment observation.
fn wait_for_x11_environment_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(value) = fs::read_to_string(path)
            && value.lines().count() >= 2
        {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for X11 environment observation"
        );
        thread::yield_now();
    }
}

/// Verifies runtime service starts processes for created windows and panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_processes_for_created_windows_and_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let window_start = service
        .create_window_with_pane_process(&primary, "build", true, Some("true"))
        .unwrap();
    assert_eq!(window_start.window_id, "@2");
    assert_eq!(window_start.pane_id, "%2");
    assert_eq!(
        service.pane_processes().primary_pid(&window_start.pane_id),
        Some(window_start.primary_pid)
    );

    let split_start = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("true"))
        .unwrap();
    assert_eq!(split_start.window_id, "@2");
    assert_eq!(split_start.pane_id, "%3");
    assert_eq!(
        service.pane_processes().primary_pid(&split_start.pane_id),
        Some(split_start.primary_pid)
    );

    let mut exited = poll_until_exit(&mut service).len();
    while exited < 2 {
        exited += poll_until_exit(&mut service).len();
    }
    assert_eq!(exited, 2);
}

/// Verifies a later foreground-process event recovers interactive-blocked
/// readiness after alternate-screen exit missed the prompt-candidate transition.
///
/// Some full-screen programs leave the alternate screen before the async
/// foreground-process update reports that the shell owns the PTY again. The
/// cached foreground-process event should reopen prompt-candidate recovery so
/// later shell actions do not stay stranded in interactive-blocked state.
#[test]
fn runtime_foreground_process_event_recovers_after_alternate_screen_exit() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", None);

    service
        .apply_pane_output_bytes("%1", b"[?1049hfullscreen".to_vec())
        .unwrap();
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::InteractiveBlocked
    );

    service
        .apply_pane_output_bytes("%1", b"[?1049l$ ".to_vec())
        .unwrap();
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::InteractiveBlocked
    );

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a newly spawned pane remains unmanaged until explicit agent entry.
///
/// Foreground metadata and prompt output may advance passive terminal readiness,
/// but neither event may arm bootstrap, register a hidden shell transaction, or
/// hide ordinary process output. This keeps startup and user input independent
/// from agent shell discovery until the user shows the agent surface.
#[test]
fn runtime_pane_start_stays_unmanaged_until_agent_entry() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 0);
    assert!(!service.pane_bootstrap_is_pending_for_tests("%1"));
    assert!(service.running_shell_transactions_for_tests().is_empty());

    service
        .apply_pane_output_bytes("%1", b"typed-before-agent".to_vec())
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("typed-before-agent"), "{pane_text}");
    assert!(!service.pane_bootstrap_is_pending_for_tests("%1"));
    assert!(service.running_shell_transactions_for_tests().is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies foreground-process changes do not arm agent bootstrap by themselves.
///
/// A newly spawned shell can temporarily yield the PTY to SSH or another
/// foreground process before returning. Neither transition is an agent-entry
/// request, so the restored shell must remain free of discovery transactions.
#[test]
fn runtime_foreground_shell_return_does_not_arm_bootstrap_before_agent_entry() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", None);

    service
        .apply_pane_foreground_process_event("%1", "ssh", primary_pid.saturating_add(1), None)
        .unwrap();
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Unknown
    );

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 0);
    assert!(!service.pane_bootstrap_is_pending_for_tests("%1"));
    assert!(service.running_shell_transactions_for_tests().is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an uncertified foreign foreground process creates a fresh shell
/// interaction epoch and prevents callers from falling back to host identity.
///
/// SSH, container clients, full-screen programs, and password prompts are all
/// untrusted at this boundary. Their process names must not authorize input,
/// while return to the primary shell must leave identity rediscovery pending
/// instead of silently restoring stale environment authority.
#[test]
fn runtime_foreign_foreground_starts_fail_closed_shell_epoch() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", None);

    assert!(service.shell_execution_identity_for_pane("%1").is_ok());
    service
        .apply_pane_foreground_process_event("%1", "ssh", primary_pid.saturating_add(1), None)
        .unwrap();

    assert!(service.begin_uncertified_foreign_shell_boundary_for_current_foreground("%1"));
    assert!(service.pane_has_uncertified_foreign_shell_boundary("%1"));
    let diagnostic = service.pane_foreground_process_diagnostic("%1").json();
    assert!(diagnostic["shell_interaction_generation"].is_u64());
    let error = service.shell_execution_identity_for_pane("%1").unwrap_err();
    assert!(error.message().contains("uncertified foreign"), "{error}");

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();

    assert!(!service.pane_has_uncertified_foreign_shell_boundary("%1"));
    let error = service.shell_execution_identity_for_pane("%1").unwrap_err();
    assert!(error.message().contains("has not been probed"), "{error}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies stale readiness recovery can use async foreground metadata when
/// synchronous PTY foreground queries are temporarily unavailable.
///
/// New panes and panes recreated by `load-layout` can have a valid shell prompt
/// while a direct `tcgetpgrp` style foreground query is still unavailable. The
/// async pane worker reports foreground process groups separately, and readiness
/// recovery should use that cached observation instead of leaving shell actions
/// stranded behind stale interactive-blocked state.
/// Verifies program-emitted pane titles stay sticky across automatic foreground
/// title refreshes, then restore the previous title mode when the foreground
/// program changes. This protects pane status pill titles from rapidly flipping
/// between OSC titles and auto-generated process titles.
#[test]
fn runtime_pane_program_title_stays_sticky_until_foreground_process_changes() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();

    service
        .apply_pane_foreground_process_event("%1", "vim", 4242, None)
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "vim"
    );

    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]2;editing notes\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "editing notes"
    );

    service
        .apply_pane_foreground_process_event("%1", "vim", 4242, None)
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "editing notes"
    );

    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]2;editing tests\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "editing tests"
    );

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "shell"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies ordinary output cannot reapply a retained OSC title after its
/// foreground program exits.
///
/// A terminal parser retains the most recent OSC title, but that retained state
/// is not a new title mutation. After foreground metadata restores the shell
/// title, prompt output must therefore leave that restored title unchanged.
#[test]
fn runtime_pane_title_does_not_reapply_stale_osc_title_after_foreground_exit() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();

    service
        .apply_pane_foreground_process_event("%1", "ssh", 4242, None)
        .unwrap();
    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]2;remote-host\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();
    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"shell$ ".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();

    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "shell"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies async pane write failures settle shell-backed file actions.
///
/// File mutations are sent through the pane shell as generated transactions. If
/// the async pane worker cannot write that transaction input, the action must
/// become a failed action result and queue model recovery instead of remaining
/// in the running-transaction table forever.
#[test]
fn runtime_pane_write_failure_fails_running_file_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-write-failure","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write file".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test action batch rationale".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "patch-fail".to_string(),

                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Add File: note.txt\n+note\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert_eq!(service.drain_pane_io_transition().side_effects.len(), 2);
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-fail"
            ))
    );

    assert!(
        service
            .apply_pane_write_failure_event(&pane_id, "synthetic PTY write failure")
            .unwrap()
    );

    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| !matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { .. }
            ))
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(!service.agent_turn_executions().contains_key("turn-1"));
    let context = runtime_prepared_context_for_turn(&service, "turn-1");
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-fail apply_patch failed]")
            && block.content.contains("pane input write failed")
    }));

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies OSC progress updates are pane-scoped, invalidate OSC-only frames,
/// project only normal percentages, and disappear on an explicit clear.
#[test]
fn runtime_tracks_terminal_progress_per_pane() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let update = service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]9;4;1;42\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert!(update.invalidate_output_frame);
    assert_eq!(
        service.terminal_frame_context().panes["%1"].terminal_progress_percent,
        Some(42)
    );

    let update = service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]133;D;0\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert!(update.invalidate_output_frame);
    assert_eq!(
        service.terminal_frame_context().panes["%1"].terminal_progress_percent,
        None
    );

    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]9;4;3\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(
        service.terminal_frame_context().panes["%1"].terminal_progress_percent,
        None
    );

    let update = service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                // Cargo's anstyle-progress formatter terminates its removal
                // record as `OSC 9;4;0; ST`.
                bytes: b"\x1b]9;4;0;\x1b\\".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert!(update.invalidate_output_frame);
    assert!(!service.process.pane_terminal_progress.contains_key("%1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies progress capability advertising preserves existing TERM_FEATURES
/// and avoids inserting a duplicate marker.
#[test]
fn runtime_terminal_features_progress_advertising_is_additive() {
    assert_eq!(
        terminal_features_with_progress(None),
        std::ffi::OsString::from("P")
    );
    assert_eq!(
        terminal_features_with_progress(Some("A".into())),
        std::ffi::OsString::from("AP")
    );
    assert_eq!(
        terminal_features_with_progress(Some("AP".into())),
        std::ffi::OsString::from("AP")
    );
}

/// Verifies a runtime-owned editor session leases one pane, presents the
/// process surface, bypasses normal key classification, and accepts completion
/// only through the transaction identities returned by the launch operation.
/// The target-specific prompt adapter is deliberately outside this test: it
/// consumes the retained completion in the dependent prompt-editing issue.
#[test]
fn runtime_external_editor_session_routes_input_and_retains_completion() {
    let root = temp_root("external-editor-session");
    let socket_path = root.join("runtime/default.sock");
    let mut service = RuntimeServiceFixture::new()
        .control_socket(&socket_path)
        .build();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "external-editor-test".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode = \"pane\"\n[external_editor]\ncommand = [\"/bin/sh\", \"-c\", \"exit 0\", \"{file}\"]\nfallback = []\n"
                .to_string(),
        }])
        .unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .ensure_agent_pane_screen("%1", &conversation_id, Size::new(80, 24).unwrap())
        .unwrap();
    service.enter_agent_subshell("%1");
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);

    let started = service
        .start_external_editor_session(
            &primary,
            "%1",
            crate::runtime::ExternalEditTarget::AgentPrompt,
            "draft before editor\n".to_string(),
            "draft before editor\n".to_string(),
            true,
        )
        .unwrap();
    assert!(service.external_editor_session_is_active("%1"));
    assert_eq!(
        service.presented_pane_surface("%1"),
        PaneSurfaceKind::Process
    );
    let editor_instance = service
        .external_editor_process_instance_for_tests("%1")
        .unwrap();
    let event_cutoff = service.event_log().unwrap().latest_event_id();
    let editor_output = service
        .apply_external_editor_process_event(
            editor_instance.clone(),
            crate::runtime::PaneProcessEvent::Pane(crate::runtime::PaneEvent::Output {
                pane_id: editor_instance.pane_id.clone(),
                bytes: b"editor-visible\x1b[6n".to_vec(),
            }),
        )
        .unwrap();
    assert!(editor_output.applied);
    assert!(editor_output.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: crate::runtime::PaneProcessIoEffect::WriteInputPriority { bytes },
        } if instance == &editor_instance && bytes == b"\x1b[1;15R"
    )));
    assert!(
        service
            .external_editor_screen("%1")
            .unwrap()
            .visible_lines()[0]
            .contains("editor-visible"),
        "external-editor output must use its independent terminal screen"
    );
    let editor_events = service.event_log().unwrap().replay_after_for(
        &EventAudience::AllPrimaries,
        event_cutoff,
        16,
    );
    assert!(editor_events.iter().any(|event| {
        event.kind == EventKind::PaneChanged
            && event.payload.contains(r#""pane_id":"%1""#)
            && event
                .payload
                .contains(r#""external_editor_output_bytes":18"#)
    }));
    assert!(
        service
            .start_external_editor_session(
                &primary,
                "%1",
                crate::runtime::ExternalEditTarget::AgentPrompt,
                String::new(),
                String::new(),
                true,
            )
            .is_err()
    );

    let input = service
        .apply_client_input_transition(&primary, b"\x01e")
        .unwrap();
    assert_eq!(input.side_effects.len(), 1);
    assert!(matches!(
        &input.side_effects[0],
        RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: crate::runtime::PaneProcessIoEffect::WriteInput { bytes },
        } if instance.pane_id == format!("@external-editor:{}", started.session_id)
            && bytes == b"\x01e"
    ));

    assert_eq!(
        service
            .complete_external_editor_session(
                "%1",
                &started.session_id,
                &started.completion_nonce,
                &started.marker,
                0,
            )
            .unwrap(),
        1
    );
    assert!(!service.external_editor_session_is_active("%1"));
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);

    let completion = service
        .take_external_editor_completion("%1", &started.session_id, &started.completion_nonce)
        .unwrap();
    assert_eq!(completion.pane_id, started.pane_id);
    assert_eq!(completion.exit_code, 0);
    assert_eq!(completion.original_content, "draft before editor\n");
    assert_eq!(
        completion.validated_content.as_deref(),
        Some(completion.original_content.as_str())
    );
    assert!(!completion.draft_path.exists());
    assert!(matches!(
        completion.target,
        crate::runtime::ExternalEditTarget::AgentPrompt
    ));
    assert!(
        service
            .take_external_editor_completion("%1", &started.session_id, &started.completion_nonce,)
            .is_none()
    );

    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime-created agent-owned panes do not inherit the daemon
/// environment as pane evidence, while user-initiated panes keep inheriting it.
///
/// An agent-owned pane root starts from a cleared base plus the documented
/// validated pane-creation allowlist, so a daemon-only credential cannot become
/// that pane's exec-time environment and therefore cannot become native
/// workload evidence. User shell panes are unchanged.
#[test]
fn runtime_agent_owned_pane_clears_daemon_environment_while_user_shell_inherits_it() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let user_started = service.start_initial_pane_process(Some("cat")).unwrap();
    let user_process = service
        .take_running_pane_process_for_adapter(&user_started.pane_id)
        .unwrap();
    let user_environment = pane_root_exec_environment(&user_process);
    let mut daemon = crate::runtime::processes::native_ambient_environment();
    daemon.push(mez_mux::process::RawEnvironmentEntry {
        key: b"MEZ_AGENT_OWNED_DAEMON_SENTINEL".to_vec(),
        value: b"daemon-only".to_vec(),
    });
    let probe =
        crate::runtime::processes::daemon_only_probe_key_for_tests(&daemon, Path::new("/bin/sh"))
            .expect("the injected daemon environment must expose a daemon-only variable");
    assert!(
        environment_forwards(&user_environment, &probe),
        "a user shell pane must keep inheriting the daemon environment"
    );
    service.inject_agent_owned_pane_daemon_environment_for_tests(daemon);

    let window_id = service.session().active_window().unwrap().id.clone();
    let agent_started = service
        .split_pane_in_window_with_process(
            &primary,
            &window_id,
            SplitDirection::Vertical,
            true,
            None,
            crate::runtime::processes::RuntimePaneProcessPurpose::AgentOwned {
                shell_mode: crate::runtime::config::ShellMode::Native,
            },
        )
        .unwrap();
    let agent_process = service
        .take_running_pane_process_for_adapter(&agent_started.pane_id)
        .unwrap();
    let agent_environment = pane_root_exec_environment(&agent_process);

    assert!(
        !environment_forwards_key(&agent_environment, b"MEZ_AGENT_OWNED_DAEMON_SENTINEL"),
        "a daemon-only sentinel must not reach an agent-owned pane root"
    );
    assert!(
        !environment_forwards(&agent_environment, &probe),
        "daemon-only {} must not be forwarded into an agent-owned pane root",
        String::from_utf8_lossy(&probe.key)
    );
    assert!(
        environment_forwards_key(&agent_environment, b"PATH"),
        "the agent-owned pane root must still receive a working PATH"
    );

    drop(user_process);
    drop(agent_process);
}

/// Reads one pane root process's exec-time environment for pane-creation tests.
///
/// Host metadata can briefly lag a freshly spawned child, so the read retries
/// without mutating process-global state or serializing tests.
fn pane_root_exec_environment(
    process: &mez_mux::process::PaneProcess,
) -> Vec<mez_mux::process::RawEnvironmentEntry> {
    for _ in 0..200 {
        if let Some(environment) = process.environment()
            && !environment.is_empty()
        {
            return environment;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the pane root process did not expose an exec-time environment");
}

/// Returns true when one raw environment snapshot forwards an exact key.
fn environment_forwards_key(
    environment: &[mez_mux::process::RawEnvironmentEntry],
    key: &[u8],
) -> bool {
    environment.iter().any(|entry| entry.key == key)
}

/// Returns true when one raw environment snapshot forwards the same mapping.
fn environment_forwards(
    environment: &[mez_mux::process::RawEnvironmentEntry],
    entry: &mez_mux::process::RawEnvironmentEntry,
) -> bool {
    environment
        .iter()
        .any(|candidate| candidate.key == entry.key && candidate.value == entry.value)
}

/// Builds one restored multi-pane service with a single durable agent binding.
///
/// Snapshot restore restores durable agent session metadata before pane
/// processes are restarted, so the injected daemon slice and the durable
/// binding together expose the agent-owned creation boundary without mutating
/// process-global state.
struct RestoredAgentPaneFixture {
    service: RuntimeSessionService,
    transcript_root: PathBuf,
    daemon_environment: Vec<mez_mux::process::RawEnvironmentEntry>,
    bound_pane_id: String,
    unbound_pane_id: String,
    third_pane_id: Option<String>,
}

impl RestoredAgentPaneFixture {
    /// Creates one restored two-pane session with one durable pane-to-agent binding.
    ///
    /// `config_text` selects the effective agent shell mode, and the injected
    /// daemon slice mirrors the ambient environment so the probe the agent-owned
    /// creation boundary drops is the discriminating evidence.
    fn new(name: &str, config_text: &str) -> Self {
        Self::with_pane_count(name, config_text, 2)
    }

    /// Creates one restored session with `pane_count` panes and one durable binding.
    fn with_pane_count(name: &str, config_text: &str, pane_count: usize) -> Self {
        let mut original = test_session();
        let primary = original.attach_primary("primary", true).unwrap();
        for _ in 1..pane_count {
            original
                .split_active_pane(&primary, SplitDirection::Vertical)
                .unwrap();
        }
        let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
        let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
        let restored = Session::from_restore_input(
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            restore_input,
        )
        .unwrap();
        let pane_ids = restored
            .windows()
            .iter()
            .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(pane_ids.len(), pane_count);
        let transcript_root = temp_root(&format!("runtime-restored-agent-pane-{name}"));
        let mut service = RuntimeSessionService::with_event_log(
            restored,
            transcript_root.join("restored.sock"),
            100,
            10,
            1024,
        )
        .unwrap();
        service
            .replace_config_layers(vec![ConfigLayer {
                name: format!("runtime-restored-agent-pane-{name}"),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: config_text.to_string(),
            }])
            .unwrap();
        let mezzanine_session_id = service.session().id.as_str().to_string();
        let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
        transcript_store
            .save_agent_session_metadata(
                &mezzanine_session_id,
                &[restored_agent_session_metadata(
                    &mezzanine_session_id,
                    &pane_ids[0],
                    "restored-agent-bound",
                )],
            )
            .unwrap();
        service.set_agent_transcript_store(transcript_store);
        let mut daemon_environment = crate::runtime::processes::native_ambient_environment();
        daemon_environment.push(mez_mux::process::RawEnvironmentEntry {
            key: b"MEZ_AGENT_OWNED_DAEMON_SENTINEL".to_vec(),
            value: b"daemon-only".to_vec(),
        });
        service.inject_agent_owned_pane_daemon_environment_for_tests(daemon_environment.clone());
        Self {
            service,
            transcript_root,
            daemon_environment,
            bound_pane_id: pane_ids[0].clone(),
            unbound_pane_id: pane_ids[1].clone(),
            third_pane_id: pane_ids.get(2).cloned(),
        }
    }

    /// Returns the daemon-only variable that pane creation must not forward.
    fn daemon_only_probe(&self) -> mez_mux::process::RawEnvironmentEntry {
        crate::runtime::processes::daemon_only_probe_key_for_tests(
            &self.daemon_environment,
            Path::new("/bin/sh"),
        )
        .expect("the injected daemon environment must expose a daemon-only variable")
    }

    /// Completes the durable binding restore expected by every fixture test.
    fn restore_agent_binding(&mut self) {
        assert_eq!(
            self.service
                .restore_agent_sessions_from_transcript_store()
                .unwrap(),
            1,
            "the fixture must restore its one durable pane-to-agent binding"
        );
    }

    /// Terminates restored panes and removes the fixture transcript root.
    fn cleanup(self) {
        let RestoredAgentPaneFixture {
            mut service,
            transcript_root,
            ..
        } = self;
        service.terminate_all_pane_processes().unwrap();
        let _ = fs::remove_dir_all(&transcript_root);
    }
}

/// Builds one durable root agent-session binding record for a restored pane.
fn restored_agent_session_metadata(
    mezzanine_session_id: &str,
    pane_id: &str,
    conversation_id: &str,
) -> mez_agent::transcript::AgentSessionMetadata {
    mez_agent::transcript::AgentSessionMetadata {
        mezzanine_session_id: mezzanine_session_id.to_string(),
        pane_id: pane_id.to_string(),
        conversation_id: conversation_id.to_string(),
        prompt_cache_lineage_id: format!("lineage-{conversation_id}"),
        visibility: "visible".to_string(),
        running_turn_id: None,
        running_turn_kind: None,
        transcript_entries: 1,
        log_level: "normal".to_string(),
        pane_model_profile: None,
        planning_enabled: false,
        response_style: None,
        directive: None,
        routing_enabled: None,
        root_routing_policy: None,
        approval_policy: None,
        pane_permission_preset_override: None,
        pane_approval_policy_override: None,
        working_directory: None,
        project_root: None,
        context_usage: None,
        context_usage_snapshot: None,
        latest_request_usage: None,
        token_usage: Default::default(),
        token_usage_by_model: Default::default(),
    }
}

/// Verifies a bound restored pane in pane shell mode is admitted and certified
/// through the ordinary managed startup contract.
///
/// The durable binding selects the agent-owned creation path; it never
/// fabricates a ready startup. A restored pane in pane mode therefore keeps a
/// pending managed admission owner, a pending pane bootstrap, and a blocked
/// scheduler until the authenticated POSIX adapter publishes availability and
/// its bootstrap transaction settles environment authority.
#[test]
fn runtime_restored_agent_bound_pane_pane_mode_admits_and_certifies_through_managed_handshake() {
    let mut fixture =
        RestoredAgentPaneFixture::new("pane-mode-handshake", "[agents]\nshell_mode = \"pane\"\n");
    let probe = fixture.daemon_only_probe();
    fixture.restore_agent_binding();
    assert_eq!(
        fixture
            .service
            .restored_pane_process_purpose(&fixture.bound_pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::AgentOwned {
            shell_mode: crate::runtime::config::ShellMode::Pane,
        },
        "a bound restored pane follows the configured pane shell mode"
    );
    assert_eq!(
        fixture
            .service
            .restored_pane_process_purpose(&fixture.unbound_pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::UserShell
    );

    let starts = fixture
        .service
        .restart_restored_pane_processes(None)
        .unwrap();
    assert_eq!(starts.len(), 2);

    assert_eq!(
        fixture
            .service
            .runtime_agent_surface_startup_phase_for_tests(&fixture.bound_pane_id),
        Some("managed-admitting"),
        "a bound restored pane must still admit through the managed handshake"
    );
    assert!(
        fixture
            .service
            .pane_bootstrap_is_pending_for_tests(&fixture.bound_pane_id),
        "pane-mode restore must keep the pane bootstrap pending"
    );
    assert!(
        !fixture
            .service
            .pane_environment_authority_is_certified_for_tests(&fixture.bound_pane_id),
        "bootstrap authority must stay uncertified before admission"
    );
    assert!(
        !fixture
            .service
            .agent_surface_allows_scheduler_start(&fixture.bound_pane_id)
    );
    assert!(
        fixture
            .service
            .runtime_agent_surface_blocked_panes()
            .contains(&fixture.bound_pane_id),
        "an unadmitted pane-mode startup must fence the pane"
    );
    assert_eq!(
        fixture
            .service
            .runtime_agent_surface_startup_phase_for_tests(&fixture.unbound_pane_id),
        None
    );

    let token = fixture
        .service
        .posix_startup_token_for_tests(&fixture.bound_pane_id)
        .expect("pane-mode restored startup must install a POSIX admission token")
        .to_string();
    assert_eq!(
        fixture
            .service
            .observe_managed_shell_protocol_event(
                &fixture.bound_pane_id,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Posix,
                &token,
                &mez_terminal::ManagedShellProtocolEvent::AdapterAvailable { trigger: None },
            )
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .service
            .runtime_agent_surface_startup_phase_for_tests(&fixture.bound_pane_id),
        Some("managed-bootstrapping")
    );

    let (marker, bootstrap_turn_id) = fixture
        .service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.pane_id == fixture.bound_pane_id
                && transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("an admitted pane-mode restored pane must own a bootstrap transaction");
    let agent_id = format!("agent-{}", fixture.bound_pane_id);
    fixture
        .service
        .observe_agent_shell_transaction_start(
            &fixture.bound_pane_id,
            &marker,
            &bootstrap_turn_id,
            &agent_id,
            &fixture.bound_pane_id,
        )
        .unwrap();
    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\ttest-host\n\
env\tuser\ttest-user\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tpath\t/usr/bin:/bin\n\
env\tcwd\t/tmp\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t1714500000\n";
    let transaction = fixture
        .service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_bytes = output.len();
    transaction.observed_output_preview = output.to_string();
    fixture
        .service
        .observe_agent_shell_transaction_end(
            &fixture.bound_pane_id,
            &marker,
            &bootstrap_turn_id,
            &agent_id,
            &fixture.bound_pane_id,
            0,
        )
        .unwrap();

    assert_eq!(
        fixture
            .service
            .runtime_agent_surface_startup_phase_for_tests(&fixture.bound_pane_id),
        Some("ready")
    );
    assert!(
        fixture
            .service
            .pane_environment_authority_is_certified_for_tests(&fixture.bound_pane_id),
        "settled bootstrap authority must certify the managed pane"
    );
    assert!(
        fixture
            .service
            .pane_environment_signature(&fixture.bound_pane_id)
            .is_some(),
        "settled bootstrap authority must publish an environment signature"
    );
    assert!(
        fixture
            .service
            .agent_surface_allows_scheduler_start(&fixture.bound_pane_id)
    );
    assert!(
        !fixture
            .service
            .pane_bootstrap_is_pending_for_tests(&fixture.bound_pane_id)
    );

    let bound_process = fixture
        .service
        .take_running_pane_process_for_adapter(&fixture.bound_pane_id)
        .unwrap();
    let bound_environment = pane_root_exec_environment(&bound_process);
    assert!(
        !environment_forwards_key(&bound_environment, &probe.key),
        "daemon-only {} must not reach a bound restored pane root",
        String::from_utf8_lossy(&probe.key)
    );
    assert!(
        environment_forwards_key(&bound_environment, b"PATH"),
        "the bound restored pane root must still receive a working PATH"
    );

    drop(bound_process);
    fixture.cleanup();
}

/// Verifies a pane-mode restored pane restarted with an explicit command is
/// diagnosed as a failed startup instead of reported ready or left pending.
///
/// The restart command replaces the managed pane shell, so the managed fixture
/// is skipped and no authenticated admission can ever arrive. The runtime must
/// settle the pane's startup owner terminally and name the replaced shell in
/// its diagnostic rather than fabricating readiness or stranding the owner.
#[test]
fn runtime_restored_agent_bound_pane_pane_mode_with_restart_command_is_diagnosed_instead_of_false_ready()
 {
    let mut fixture = RestoredAgentPaneFixture::new(
        "pane-mode-restart-command",
        "[agents]\nshell_mode = \"pane\"\n",
    );
    let probe = fixture.daemon_only_probe();
    fixture.restore_agent_binding();

    let starts = fixture
        .service
        .restart_restored_pane_processes(Some("cat >/dev/null"))
        .unwrap();
    assert_eq!(starts.len(), 2);

    assert_eq!(
        fixture
            .service
            .runtime_agent_surface_startup_phase_for_tests(&fixture.bound_pane_id),
        Some("failed"),
        "a restart command replaces the managed shell, so pane-mode startup cannot admit"
    );
    assert!(
        !fixture
            .service
            .pane_bootstrap_is_pending_for_tests(&fixture.bound_pane_id)
    );
    assert!(
        !fixture
            .service
            .pane_environment_authority_is_certified_for_tests(&fixture.bound_pane_id)
    );
    assert!(
        !fixture
            .service
            .agent_surface_allows_scheduler_start(&fixture.bound_pane_id)
    );
    let replayed = fixture
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(
        replayed.iter().any(|event| {
            event
                .payload
                .contains(r#""agent_surface_startup":"failed""#)
                && event.payload.contains("replaces the managed pane shell")
        }),
        "the diagnosed startup failure must name the replaced managed pane shell"
    );
    assert_eq!(
        fixture
            .service
            .recover_expired_runtime_agent_surface_startups(u64::MAX)
            .unwrap(),
        0,
        "a terminally failed startup must not expire into a second failure"
    );

    let bound_process = fixture
        .service
        .take_running_pane_process_for_adapter(&fixture.bound_pane_id)
        .unwrap();
    let bound_environment = pane_root_exec_environment(&bound_process);
    assert!(
        !environment_forwards_key(&bound_environment, &probe.key),
        "daemon-only {} must not reach a bound restored pane root",
        String::from_utf8_lossy(&probe.key)
    );
    assert!(
        environment_forwards_key(&bound_environment, b"PATH"),
        "the bound restored pane root must still receive a working PATH"
    );

    drop(bound_process);
    fixture.cleanup();
}

/// Verifies the restored-pane home-directory retry keeps the agent-owned
/// creation path for a bound pane.
///
/// A snapshot can name a working directory that existed during restore planning
/// but is unusable when the fresh pane process starts. The retry must stay
/// agent-owned, so the retried root still drops the daemon-only probe and still
/// enters the pane mode-specific startup contract.
#[test]
fn runtime_restored_agent_bound_pane_home_directory_retry_keeps_agent_owned_purpose() {
    let root = temp_root("runtime-restored-agent-cwd-retry");
    let inaccessible_cwd = root.join("inaccessible-cwd");
    fs::create_dir_all(&inaccessible_cwd).unwrap();
    let original_permissions = fs::metadata(&inaccessible_cwd).unwrap().permissions();
    fs::set_permissions(&inaccessible_cwd, fs::Permissions::from_mode(0o000)).unwrap();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert!(home.is_dir());

    let original = test_session();
    let mut payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    payload.windows[0].panes[0].current_working_directory =
        Some(inaccessible_cwd.to_string_lossy().into_owned());
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let pane_id = restored
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let transcript_root = temp_root("runtime-restored-agent-cwd-retry-store");
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        transcript_root.join("restored-cwd.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "runtime-restored-agent-cwd-retry".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode = \"pane\"\n".to_string(),
        }])
        .unwrap();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[restored_agent_session_metadata(
                &mezzanine_session_id,
                &pane_id,
                "restored-agent-cwd-retry",
            )],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let mut daemon_environment = crate::runtime::processes::native_ambient_environment();
    daemon_environment.push(mez_mux::process::RawEnvironmentEntry {
        key: b"MEZ_AGENT_OWNED_DAEMON_SENTINEL".to_vec(),
        value: b"daemon-only".to_vec(),
    });
    let probe = crate::runtime::processes::daemon_only_probe_key_for_tests(
        &daemon_environment,
        Path::new("/bin/sh"),
    )
    .expect("the injected daemon environment must expose a daemon-only variable");
    service.inject_agent_owned_pane_daemon_environment_for_tests(daemon_environment);
    assert_eq!(
        service
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        1
    );

    let starts = service.restart_restored_pane_processes(None).unwrap();

    assert_eq!(starts.len(), 1);
    assert_eq!(
        service.pane_current_working_directory(&pane_id).as_deref(),
        Some(home.as_path())
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event
            .payload
            .contains("snapshot resume pane cwd unavailable; retrying from home")
    }));
    assert_eq!(
        service.restored_pane_process_purpose(&pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::AgentOwned {
            shell_mode: crate::runtime::config::ShellMode::Pane,
        },
        "the home-directory retry must preserve the agent-owned creation path"
    );
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&pane_id),
        Some("managed-admitting")
    );

    let process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    let environment = pane_root_exec_environment(&process);
    assert!(
        !environment_forwards_key(&environment, &probe.key),
        "daemon-only {} must not reach a retried agent-owned pane root",
        String::from_utf8_lossy(&probe.key)
    );
    drop(process);

    service.terminate_all_pane_processes().unwrap();
    fs::set_permissions(&inaccessible_cwd, original_permissions).unwrap();
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&transcript_root);
}

/// Verifies the restored-pane creation purpose follows only the durable
/// non-ephemeral root agent binding for that pane.
///
/// The predicate is deliberately in-memory: a pane restored without a durable
/// root binding, and a pane whose live binding is ephemeral or belongs to a
/// subagent conversation, both stay on the user-shell creation path.
#[test]
fn runtime_restored_pane_process_purpose_follows_durable_binding() {
    let mut fixture = RestoredAgentPaneFixture::with_pane_count(
        "purpose-binding",
        "[agents]\nshell_mode = \"pane\"\n",
        3,
    );
    fixture.restore_agent_binding();
    let ephemeral_pane_id = fixture
        .third_pane_id
        .clone()
        .expect("the three-pane fixture must expose its third pane");
    {
        let session = fixture
            .service
            .agent_shell_store_mut()
            .ensure_session(&ephemeral_pane_id)
            .unwrap();
        session.conversation_kind = mez_agent::AgentConversationKind::Subagent;
        session.ephemeral = true;
    }

    assert_eq!(
        fixture
            .service
            .restored_pane_process_purpose(&fixture.bound_pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::AgentOwned {
            shell_mode: crate::runtime::config::ShellMode::Pane,
        }
    );
    assert_eq!(
        fixture
            .service
            .restored_pane_process_purpose(&fixture.unbound_pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::UserShell
    );
    assert_eq!(
        fixture
            .service
            .restored_pane_process_purpose(&ephemeral_pane_id),
        crate::runtime::processes::RuntimePaneProcessPurpose::UserShell,
        "an ephemeral or subagent binding must not select the agent-owned creation path"
    );

    fixture.cleanup();
}
