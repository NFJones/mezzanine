//! Runtime tests for agent prompt readiness behavior.

use super::*;

/// Verifies restored panes wait for event-driven prompt readiness.
///
/// Restarting the process must not infer a completed prompt from process
/// creation or arbitrary initial PTY activity. The actor-owned output path
/// supplies the prompt bytes and OSC boundary before bootstrap can run.
#[test]
fn runtime_service_restarts_restored_panes_without_assuming_prompt_readiness() {
    let original = test_session();
    let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-prompt-readiness.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("sleep 30"))
        .unwrap();

    assert_eq!(starts.len(), 1);
    assert_eq!(
        service.pane_readiness_state(&starts[0].pane_id),
        PaneReadinessState::Unknown
    );
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 0);
    assert!(service.pane_bootstrap_is_pending_for_tests(&starts[0].pane_id));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies only the completed-prompt OSC 133 marker authorizes passive shell
/// readiness. Prompt-start and ordinary command-finished markers can precede
/// visible PS1 bytes, so treating either as actionable would let hidden
/// bootstrap output suppression consume a separately delivered prompt.
#[test]
fn runtime_passive_readiness_waits_for_prompt_end_marker() {
    let mut service = test_runtime_service();

    let prompt_start = service
        .observe_agent_shell_transaction_events("%1", &[TerminalOscEvent::ShellPromptStart])
        .unwrap();
    assert_eq!(prompt_start, 0);
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Unknown
    );

    service.set_pane_readiness("%1", PaneReadinessState::Busy);
    let command_finished = service
        .observe_agent_shell_transaction_events(
            "%1",
            &[TerminalOscEvent::ShellCommandFinished { exit_code: Some(0) }],
        )
        .unwrap();
    assert_eq!(command_finished, 0);
    assert_eq!(service.pane_readiness_state("%1"), PaneReadinessState::Busy);

    let prompt_end = service
        .observe_agent_shell_transaction_events("%1", &[TerminalOscEvent::ShellPromptEnd])
        .unwrap();
    assert_eq!(prompt_end, 1);
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
}

/// Verifies a degraded pane can recover from later prompt-boundary evidence.
///
/// A failed probe or bootstrap can leave a pane `degraded` even after the user
/// returns it to an idle shell prompt. Prompt markers should restore the pane
/// to the probeable prompt-candidate path unless foreground metadata proves a
/// non-shell interactive program is active.
#[test]
fn runtime_passive_prompt_recovers_degraded_readiness() {
    let mut service = test_runtime_service();
    service.set_pane_readiness("%1", PaneReadinessState::Degraded);

    let observed = service
        .observe_passive_shell_prompt_candidate("%1", "osc133-prompt")
        .unwrap();

    assert_eq!(observed, 1);
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
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
    service.terminate_all_pane_processes().unwrap();
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

/// Verifies runtime provider execution completes running prompt turn.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_provider_execution_completes_running_prompt_turn() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
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
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeEchoProvider,
            ModelProfile {
                provider: "runtime-echo".to_string(),
                model: "echo-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert!(execution.final_turn);
    assert_eq!(execution.response.raw_text, "done");
    assert!(
        execution
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("summarize the pane"))
    );
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Completed)
    );
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let assistant_entry = entries
        .iter()
        .find(|entry| entry.role == mez_agent::transcript::TranscriptRole::Assistant)
        .expect("assistant transcript entry should be persisted");
    assert_eq!(
        assistant_entry.content,
        concat!(
            "rationale: test action batch rationale\n",
            "done\n",
            "action rationale say-1 (say): report completion\n",
            "Done."
        )
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.content.contains("summarize the pane"))
    );
    assert_eq!(
        service.persistence.pane_transcript_refs("%1"),
        vec![format!("transcript:%1:{conversation_id}")]
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""state":"completed""#), "{tasks}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(
        audit.contains(r#""event_type":"external_integration""#),
        "{audit}"
    );
    assert!(audit.contains(r#""action":"provider_request""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""provider":"runtime-echo""#), "{audit}");
    assert!(audit.contains(r#""model":"echo-model""#), "{audit}");
    assert!(audit.contains(r#""turn_id":"turn-1""#), "{audit}");

    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}
