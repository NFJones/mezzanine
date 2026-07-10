//! Runtime tests for agent conversations behavior.

use super::*;

/// Verifies terminal snapshot commands use the live runtime snapshot repository.
///
/// The command prompt should no longer return command-layer placeholders for
/// `save-layout` or `load-layout` when a daemon has configured snapshot
/// storage. This protects the bridge from parsed colon commands to the same
/// runtime control paths used by JSON-RPC snapshot clients, and verifies the
/// primary client can keep using the session immediately after `load-layout`.
#[test]
fn runtime_terminal_snapshot_commands_create_and_resume_snapshots() {
    let root = temp_root("terminal-snapshot-commands");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut service = test_runtime_service();
    service.set_snapshot_repository(snapshots);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let old_pane_start = service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let old_pane_id = old_pane_start.pane_id.clone();
    assert!(service.pane_processes().contains_pane(&old_pane_id));

    let create = service
        .execute_terminal_command(&primary, "save-layout --name checkpoint")
        .unwrap();
    assert!(create.contains(r#""command":"save-layout""#), "{create}");
    assert!(create.contains(r#""kind":"display""#), "{create}");
    assert!(
        create.contains(r#""body":"saved layout checkpoint""#),
        "{create}"
    );
    assert!(!create.contains(r#"\"snapshot\""#), "{create}");

    let resume = service
        .execute_terminal_command(&primary, "load-layout --latest")
        .unwrap();
    assert!(resume.contains(r#""command":"load-layout""#), "{resume}");
    assert!(
        resume.contains(r#""body":"loaded latest layout""#),
        "{resume}"
    );
    assert!(!resume.contains(r#"\"resumed\":true"#), "{resume}");
    assert!(!service.pane_processes().contains_pane(&old_pane_id));
    let tracked_pane_ids = service.pane_processes().tracked_pane_ids();
    assert_eq!(tracked_pane_ids.len(), 1);
    assert_ne!(tracked_pane_ids[0], old_pane_id);
    let live_pane_ids = service
        .session()
        .windows()
        .iter()
        .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
        .collect::<Vec<_>>();
    assert!(!live_pane_ids.contains(&old_pane_id));
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""layout":"resized""#)),
        "{events:?}"
    );

    let create_after_resume = service
        .execute_terminal_command(&primary, "save-layout --name checkpoint-after-load")
        .unwrap();
    assert!(
        create_after_resume.contains(r#""body":"saved layout checkpoint-after-load""#),
        "{create_after_resume}"
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies unscoped terminal snapshot resume selects the newest restorable snapshot.
///
/// A user-visible `:load-layout --latest` command should be able to restore
/// the snapshot most recently created by `:save-layout` even when the live
/// daemon has a different session id after restart. Scoping `--latest` to the
/// current session id made the command unable to find persisted snapshots from
/// previous daemon sessions, so this regression uses two runtime services that
/// share one repository root. Resume also keeps the receiving runtime's session
/// and primary client identity because it only recreates the saved topology and
/// fresh pane shells rather than adopting snapshotted connection state.
#[test]
fn runtime_terminal_snapshot_resume_latest_uses_repository_latest_across_sessions() {
    let root = temp_root("terminal-snapshot-latest-cross-session");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut creating_service = test_runtime_service();
    creating_service.set_snapshot_repository(snapshots.clone());
    let creating_primary = creating_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = creating_service
        .execute_terminal_command(&creating_primary, "save-layout --name restart-point")
        .unwrap();
    assert!(
        create.contains(r#""body":"saved layout restart-point""#),
        "{create}"
    );

    let mut resuming_service = test_runtime_service();
    resuming_service.set_snapshot_repository(snapshots);
    let resuming_primary = resuming_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let live_session_id = resuming_service.session.id.to_string();

    let resume = resuming_service
        .execute_terminal_command(&resuming_primary, "load-layout --latest")
        .unwrap();
    assert!(
        resume.contains(r#""body":"loaded latest layout""#),
        "{resume}"
    );
    assert_eq!(resuming_service.session.id.to_string(), live_session_id);

    let _ = fs::remove_dir_all(root);
}

/// Verifies `:load-layout --latest` revives a detached snapshot into a live
/// running session before restored pane restart begins.
///
/// Snapshot payloads preserve detached lifecycle state so users can resume a
/// saved detached daemon later. The live resume path must still mark the
/// restored session running before it restarts panes, otherwise the hierarchy
/// installs and the first restart step crashes on the live-session guard.
#[test]
fn runtime_terminal_snapshot_resume_latest_revives_detached_snapshot_session() {
    let root = temp_root("terminal-snapshot-resume-detached-state");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut creating_service = test_runtime_service();
    creating_service.set_snapshot_repository(snapshots.clone());
    let creating_primary = creating_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = creating_service
        .execute_terminal_command(&creating_primary, "save-layout --name detached-restart")
        .unwrap();
    assert!(
        create.contains(r#""body":"saved layout detached-restart""#),
        "{create}"
    );

    creating_service
        .detach_primary(&creating_primary, Size::new(80, 24).unwrap())
        .unwrap();

    let mut resuming_service = test_runtime_service();
    resuming_service.set_snapshot_repository(snapshots);
    let resuming_primary = resuming_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let resume = resuming_service
        .execute_terminal_command(&resuming_primary, "load-layout --latest")
        .unwrap();
    assert!(
        resume.contains(r#""body":"loaded latest layout""#),
        "{resume}"
    );
    assert_eq!(resuming_service.session.state, SessionState::Running);

    let _ = fs::remove_dir_all(root);
}

/// Verifies `/resume` completion includes saved conversation ids supplied by
/// the runtime transcript store.
#[test]
fn runtime_agent_prompt_resume_autocompletes_saved_session_uuid() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-complete"));
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "018f6b3a-1b2c-7000-9000-cafebabefeed".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/resume 018f".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed "
    );
}

/// Verifies `/resume <session>` replays saved transcript context into the pane
/// buffer after rebinding the pane-local agent shell. A resumed task should
/// show enough prior conversation content for the user to continue without
/// opening a separate transcript file.
#[test]
fn runtime_agent_prompt_resume_displays_saved_transcript_context() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-display"));
    let conversation_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    for (sequence, role, content) in [
        (1, crate::transcript::TranscriptRole::User, "aGVsbG8K"),
        (
            2,
            crate::transcript::TranscriptRole::Assistant,
            "I inspected the repo and started the change",
        ),
        (
            3,
            crate::transcript::TranscriptRole::Tool,
            r#"action_id=action-1 action_type=say status=succeeded content: ignored structured_content: {"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Implemented the change"}"#,
        ),
    ] {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation_id}"))
        .unwrap();

    assert!(response.contains("resumed=true"), "{response}");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Resumed Agent Session"), "{pane_text}");
    assert!(
        pane_text.contains(&format!("Conversation ID: {conversation_id}")),
        "{pane_text}"
    );
    assert!(pane_text.contains("Entries: 3"), "{pane_text}");
    assert!(pane_text.contains("Resumed:\n▐ yes"), "{pane_text}");
    assert!(pane_text.contains("user> hello"), "{pane_text}");
    assert!(
        pane_text.contains("mez> I inspected the repo and started the change"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: Implemented the change"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("aGVsbG8K"), "{pane_text}");
    assert!(!pane_text.contains("structured_content"), "{pane_text}");
    assert!(!pane_text.contains("[1 turn=turn-1]"), "{pane_text}");
}

/// Verifies that `/new` is a live agent-shell mutation rather than a generic
/// runtime-required placeholder. A fresh conversation id with zero transcript
/// entries must replace the active pane's completed conversation while keeping
/// the shell visible for the next prompt.
#[test]
fn runtime_agent_shell_new_command_starts_fresh_conversation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .start_turn("%1", "turn-previous")
        .unwrap();
    service
        .agent_shell_store_mut()
        .finish_turn("%1", "turn-previous")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-new","method":"agent/shell/command","params":{"idempotency_key":"agent-new","input":"/new"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"new""#), "{response}");
    assert!(response.contains("new=true"), "{response}");
    assert!(response.contains("transcript_entries=0"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_ne!(session.session_id, old_session);
    assert_eq!(session.transcript_entries, 0);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
}

/// Verifies default `/loop` reuses the current pane conversation for the first
/// work iteration.
///
/// In-place iteration is the default mode, so the first loop work turn should
/// prompt the model in the already-active session instead of rebinding to a
/// forked transcript.
#[test]
fn runtime_agent_loop_reuses_current_conversation_by_default() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-reuse"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "review this document".to_string(),
        })
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop review this document")
        .unwrap();

    assert!(matches!(
        outcome,
        crate::runtime::AgentShellCommandOutcome::Mutated { .. }
    ));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, old_session);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> /loop review this document"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/loop --fork` rotates the pane to a fresh ephemeral conversation
/// before the first work iteration starts.
///
/// Fork-mode loop attempts need isolated model context without creating saved
/// conversations. This regression keeps the work conversation runtime-only and
/// checkpoints the parent conversation as the resumable pane binding.
#[test]
fn runtime_agent_loop_fork_option_starts_first_iteration_in_ephemeral_conversation() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-fork"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "review this document".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --fork review this document")
        .unwrap();

    assert!(matches!(
        outcome,
        crate::runtime::AgentShellCommandOutcome::Mutated { .. }
    ));
    let loop_session = {
        let session = service.agent_shell_store().get("%1").unwrap();
        assert_ne!(session.session_id, old_session);
        assert!(session.ephemeral);
        assert_eq!(
            session
                .ephemeral_transcript_source_conversation_id
                .as_deref(),
            Some(old_session.as_str())
        );
        assert_eq!(session.ephemeral_transcript_source_entries, 1);
        assert_eq!(session.transcript_entries, 0);
        assert_eq!(session.visibility, AgentShellVisibility::Visible);
        session.session_id.clone()
    };
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    let saved = transcript_store.list().unwrap();
    assert!(
        saved
            .iter()
            .any(|summary| summary.conversation_id == old_session)
    );
    assert!(
        !saved
            .iter()
            .any(|summary| summary.conversation_id == loop_session)
    );
    service.checkpoint_agent_session_metadata().unwrap();
    let metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(metadata.len(), 1, "{metadata:#?}");
    assert_eq!(metadata[0].conversation_id, old_session);
    assert_eq!(metadata[0].transcript_entries, 1);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> /loop --fork review this document"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/loop --fork` can start from a pane conversation that has no
/// persisted transcript entries yet.
///
/// The fork-mode loop controller forks each iteration from the parent pane conversation.
/// A brand-new pane may not have any saved transcript rows, so the first loop
/// iteration still needs a fresh conversation id instead of failing the fork.
#[test]
fn runtime_agent_loop_fork_option_starts_when_parent_conversation_has_no_saved_entries() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-empty-parent"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --fork review this document")
        .unwrap();

    assert!(matches!(
        outcome,
        crate::runtime::AgentShellCommandOutcome::Mutated { .. }
    ));
    let loop_session = {
        let session = service.agent_shell_store().get("%1").unwrap();
        assert_ne!(session.session_id, old_session);
        assert!(session.ephemeral);
        assert_eq!(
            session
                .ephemeral_transcript_source_conversation_id
                .as_deref(),
            Some(old_session.as_str())
        );
        assert_eq!(session.ephemeral_transcript_source_entries, 0);
        assert_eq!(session.transcript_entries, 0);
        assert_eq!(session.visibility, AgentShellVisibility::Visible);
        session.session_id.clone()
    };
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    service.checkpoint_agent_session_metadata().unwrap();
    let metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(metadata.len(), 1, "{metadata:#?}");
    assert_eq!(metadata[0].conversation_id, old_session);
    assert_eq!(metadata[0].transcript_entries, 0);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/loop --new` starts the first iteration in a fresh ephemeral
/// conversation with no parent transcript source and honors a per-command
/// loop-limit override.
///
/// New-mode loop attempts must isolate each work iteration from both the
/// current pane conversation and any parent transcript fork while still
/// restoring the parent conversation as the durable pane binding.
#[test]
fn runtime_agent_loop_new_option_starts_first_iteration_in_fresh_ephemeral_conversation() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-new"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "review this document".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --new --limit 3 review this document")
        .unwrap();

    assert!(matches!(
        outcome,
        crate::runtime::AgentShellCommandOutcome::Mutated { .. }
    ));
    let loop_state = service.agent_loops_by_pane.get("%1").unwrap();
    assert_eq!(
        loop_state.mode,
        crate::runtime::agent_state::RuntimeAgentLoopMode::NewEachIteration
    );
    assert_eq!(loop_state.max_iterations, 3);
    let loop_session = {
        let session = service.agent_shell_store().get("%1").unwrap();
        assert_ne!(session.session_id, old_session);
        assert!(session.ephemeral);
        assert!(
            session
                .ephemeral_transcript_source_conversation_id
                .is_none()
        );
        assert_eq!(session.ephemeral_transcript_source_entries, 0);
        assert_eq!(session.transcript_entries, 0);
        assert_eq!(session.visibility, AgentShellVisibility::Visible);
        session.session_id.clone()
    };
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    let saved = transcript_store.list().unwrap();
    assert!(
        saved
            .iter()
            .any(|summary| summary.conversation_id == old_session)
    );
    assert!(
        !saved
            .iter()
            .any(|summary| summary.conversation_id == loop_session)
    );
    service.checkpoint_agent_session_metadata().unwrap();
    let metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(metadata.len(), 1, "{metadata:#?}");
    assert_eq!(metadata[0].conversation_id, old_session);
    assert_eq!(metadata[0].transcript_entries, 1);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> /loop --new --limit 3 review this document"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/clear` follows the spec-level behavior of clearing the live
/// viewport while preserving pane logs and starting a fresh visible
/// conversation.
#[test]
fn runtime_agent_shell_clear_command_resets_conversation_and_terminal_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 3).unwrap(), 10).unwrap();
    screen.feed(b"old visible text");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .start_turn("%1", "turn-previous")
        .unwrap();
    service
        .agent_shell_store_mut()
        .finish_turn("%1", "turn-previous")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-clear","method":"agent/shell/command","params":{"idempotency_key":"agent-clear","input":"/clear"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"clear""#), "{response}");
    assert!(response.contains("new=true"), "{response}");
    assert!(
        response.contains("terminal_view_cleared=true"),
        "{response}"
    );
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_ne!(session.session_id, old_session);
    assert_eq!(session.transcript_entries, 0);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .iter()
            .all(|line| line.trim().is_empty()),
        "{:?}",
        service.pane_screen("%1").unwrap().visible_lines()
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old visible text")
    );
}

/// Verifies pane token usage is accumulated across conversations in one pane.
///
/// The `/status` pane token section is labeled as pane-scoped user-visible
/// accounting. Starting a fresh conversation in the same pane must not hide
/// earlier provider usage from that pane-lifetime total.
#[test]
fn runtime_agent_shell_status_pane_tokens_survive_conversation_switch() {
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
    let profile = runtime_model_profile("openai", "gpt-fast");
    let first_usage = crate::agent::ModelTokenUsage {
        input_tokens: 120,
        output_tokens: 34,
        reasoning_tokens: 9,
        cached_input_tokens: Some(80),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        first_usage,
        first_usage,
        Some(&profile),
    );
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "status-pane-session-2", 0)
        .unwrap();
    let second_usage = crate::agent::ModelTokenUsage {
        input_tokens: 40,
        output_tokens: 0,
        reasoning_tokens: 0,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        second_usage,
        second_usage,
        Some(&profile),
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-status-pane-tokens","method":"agent/shell/command","params":{"idempotency_key":"agent-status-pane-tokens","input":"/status"}}"#,
        &primary,
    );

    assert!(
        response.contains("### Pane Agent Token Usage"),
        "{response}"
    );
    assert!(
        response.contains("| openai | gpt-fast | 160 | unknown | 34 | 9 | unknown |"),
        "{response}"
    );
}

/// Verifies that saved agent conversations can be listed, resumed into the
/// current pane, exposed to prompt context, and forked while keeping readline
/// prompt history available through the shared prompt-history file.
#[test]
fn runtime_agent_shell_resume_and_fork_manage_saved_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-fork"));
    let cwd = temp_root("runtime-agent-resume-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::System,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: format!("cwd={}", cwd.display()),
        })
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 2,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 3,
            created_at_unix_seconds: 2,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-new".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "latest saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_prompt_history("saved", "find files")
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::transcript::AgentPresentationEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 3,
            pane_id: "%9".to_string(),
            turn_id: Some("turn-old".to_string()),
            terminal_width: 80,
            style_names: vec!["assistant".to_string(), "status".to_string()],
            display_lines: vec![
                "mez> rendered saved response".to_string(),
                "agent: rendered saved status".to_string(),
            ],
            copy_lines: vec![
                "mez> copy saved response".to_string(),
                "agent: copy saved status".to_string(),
            ],
            ansi_text: Some(
                "\r▐ mez> rendered saved response\r\n▐ agent: rendered saved status\r\n▐ ansi-only replay marker\r\n"
                    .to_string(),
            ),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-list","method":"agent/shell/command","params":{"idempotency_key":"resume-list","input":"/resume"}}"#,
        &primary,
    );
    assert!(picker.contains("mez-agent:/resume%20saved"), "{picker}");
    assert!(picker.contains("mez-agent:/resume%20latest"), "{picker}");
    let saved_section = picker
        .split("\n\n")
        .find(|section| section.contains("mez-agent:/resume%20saved"))
        .expect("saved session section should exist");
    assert!(saved_section.contains("  - Prompt: latest s"), "{picker}");
    assert!(!saved_section.contains("  - Prompt: saved p"), "{picker}");

    let latest = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-latest","method":"agent/shell/command","params":{"idempotency_key":"resume-latest","input":"/resume --latest"}}"#,
        &primary,
    );
    assert!(latest.contains("conversation_id=latest"), "{latest}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("latest")
    );

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume","method":"agent/shell/command","params":{"idempotency_key":"resume","input":"/resume saved"}}"#,
        &primary,
    );
    assert!(resumed.contains("conversation_id=saved"), "{resumed}");
    assert_eq!(
        service.pane_current_working_directory("%1").as_deref(),
        Some(cwd.as_path())
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    let resumed_pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        resumed_pane_text.contains("rendered sa") && resumed_pane_text.contains("response"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("agent: rendered sa")
            && resumed_pane_text.contains("ved status"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("ansi-only") && resumed_pane_text.contains("arker"),
        "{resumed_pane_text}"
    );
    assert!(
        !resumed_pane_text.contains("Resumed Agent Session"),
        "{resumed_pane_text}"
    );
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved")
        ]
    );
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == crate::agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved prompt")
    }));

    let forked = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork","method":"agent/shell/command","params":{"idempotency_key":"fork","input":"/fork saved-fork"}}"#,
        &primary,
    );
    assert!(forked.contains("source=saved"), "{forked}");
    assert!(forked.contains("conversation_id=saved-fork"), "{forked}");
    assert!(forked.contains("source_pane=%1"), "{forked}");
    assert_eq!(transcript_store.inspect("saved-fork").unwrap().len(), 3);
    assert_eq!(
        transcript_store.inspect_presentation("saved-fork").unwrap()[0].display_lines[0],
        "mez> rendered saved response"
    );
    let forked_pane = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "saved-fork")
        .map(|session| session.pane_id.clone())
        .expect("forked conversation should be bound to a pane");
    assert_ne!(forked_pane, "%1");
    assert_eq!(
        transcript_store.prompt_history("saved-fork").unwrap(),
        vec![
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved"),
            String::from("/fork saved-fork")
        ]
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&forked_pane)
            .map(|session| session.session_id.as_str()),
        Some("saved-fork")
    );
    assert_eq!(
        service
            .agent_prompt_inputs
            .get(&forked_pane)
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume saved"
    );
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(cwd);
}

/// Verifies the live `/resume` picker view starts selected-link styling on the
/// first visible session-id cell rather than the preceding list separator.
///
/// Helper-level overlay span tests can still miss attached-client regressions
/// if the visible picker row shifts styling after command submission. This
/// regression opens the real `/resume` picker through the agent-shell prompt
/// and inspects the rendered client-view row the user actually sees.
#[test]
fn runtime_resume_picker_view_keeps_selected_link_styling_off_previous_cell() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-picker-view"));
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: session_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 11,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let visibility = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    let show = if visibility.contains("visibility=visible") {
        visibility
    } else {
        assert!(visibility.contains("visibility=hidden"), "{visibility}");
        service
            .execute_terminal_command(&primary, "agent-shell")
            .unwrap()
    };
    assert!(show.contains("visibility=visible"), "{show}");
    let _ = service.drain_deferred_pane_inputs();

    let submitted = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/resume\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(submitted.forwarded_bytes, 0);
    assert!(submitted.view_refresh_required);
    assert!(service.primary_display_overlay.is_some());

    let moved = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(moved.forwarded_bytes, 0);
    assert!(moved.view_refresh_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.active_selection_index),
        Some(1)
    );

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = view
        .lines
        .iter()
        .position(|line| line.contains(session_id))
        .expect("resume picker should render the saved session id");
    let line = &view.lines[row];
    let start = display_column_for_fragment(line, session_id);
    let previous_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: line.clone(),
            style_spans: view.line_style_spans[row].clone(),
            copy_text: None,
        },
        start.saturating_sub(1),
    );
    let first_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: line.clone(),
            style_spans: view.line_style_spans[row].clone(),
            copy_text: None,
        },
        start,
    );

    assert_ne!(
        previous_rendition.foreground,
        Some(service.ui_theme.colors.agent_transcript_command.foreground),
        "resume picker link foreground shifted left in live view: {view:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left in live view: {view:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(service.ui_theme.colors.agent_model.background),
        "resume picker active background shifted left in live view: {view:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(service.ui_theme.colors.agent_transcript_command.foreground),
        "resume picker first session-id cell lost link foreground: {view:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline: {view:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(service.ui_theme.colors.agent_model.background),
        "resume picker first session-id cell lost active background: {view:?}"
    );
}

/// Verifies the full attached-terminal presentation path preserves the
/// selected-link boundary on the live `/resume` picker row.
///
/// The picker's rendered client view is only half the path shown to the user.
/// The attached client converts that view into presentation rows and row-diff
/// frames before a terminal screen applies the result. This regression covers
/// that full round trip using the real previous/current picker views so a
/// one-cell-left shift in the attached output path cannot hide behind helper
///-level overlay tests.
#[test]
fn runtime_resume_picker_attached_frame_keeps_selected_link_styling_off_previous_cell() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-picker-frame"));
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: session_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 11,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let visibility = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    let show = if visibility.contains("visibility=visible") {
        visibility
    } else {
        assert!(visibility.contains("visibility=hidden"), "{visibility}");
        service
            .execute_terminal_command(&primary, "agent-shell")
            .unwrap()
    };
    assert!(show.contains("visibility=visible"), "{show}");
    let _ = service.drain_deferred_pane_inputs();

    let submitted = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/resume\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(submitted.forwarded_bytes, 0);
    assert!(submitted.view_refresh_required);
    let previous_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();

    let moved = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(moved.forwarded_bytes, 0);
    assert!(moved.view_refresh_required);
    let current_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();

    let modes = crate::terminal::AttachedTerminalOutputModes {
        cursor_visible: current_view.cursor_visible,
        cursor_blink: current_view.cursor_blink,
        cursor_blink_interval_ms: current_view.cursor_blink_interval_ms,
        cursor_row: current_view.cursor_row,
        cursor_column: current_view.cursor_column,
        application_keypad: current_view.application_keypad,
        bracketed_paste: current_view.bracketed_paste,
        host_mouse_reporting: current_view.host_mouse_reporting,
        ..crate::terminal::AttachedTerminalOutputModes::default()
    };
    let (previous_lines, previous_spans) =
        crate::terminal::compose_client_presentation_with_styles(&previous_view, None);
    let (current_lines, current_spans) =
        crate::terminal::compose_client_presentation_with_styles(&current_view, None);
    let previous_frame = crate::terminal::encode_attached_terminal_output_update_frame_with_styles(
        &previous_lines,
        &previous_spans,
        None,
        modes,
        None,
    );
    let previous_state = crate::terminal::AttachedTerminalOutputFrameState::new_with_modes(
        &previous_lines,
        &previous_spans,
        modes,
    );
    let update_frame = crate::terminal::encode_attached_terminal_output_update_frame_with_styles(
        &current_lines,
        &current_spans,
        None,
        modes,
        Some(&previous_state),
    );
    let mut screen = TerminalScreen::new(Size::new(120, 24).unwrap(), 10).unwrap();
    screen.feed(&previous_frame);
    screen.feed(&update_frame);

    let styled_lines = screen.visible_styled_lines();
    let row = styled_lines
        .iter()
        .find(|line| line.text.contains(session_id))
        .unwrap();
    let start = display_column_for_fragment(&row.text, session_id);
    let previous_rendition = styled_line_rendition_at(row, start.saturating_sub(1));
    let first_rendition = styled_line_rendition_at(row, start);

    assert_ne!(
        previous_rendition.foreground,
        Some(service.ui_theme.colors.agent_transcript_command.foreground),
        "resume picker link foreground shifted left after attached frame update: {styled_lines:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left after attached frame update: {styled_lines:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(service.ui_theme.colors.agent_model.background),
        "resume picker active background shifted left after attached frame update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(service.ui_theme.colors.agent_transcript_command.foreground),
        "resume picker first session-id cell lost link foreground after attached frame update: {styled_lines:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline after attached frame update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(service.ui_theme.colors.agent_model.background),
        "resume picker first session-id cell lost active background after attached frame update: {styled_lines:?}"
    );
}

/// Verifies `/resume` reloads saved provider token totals for the rebound
/// conversation.
///
/// Active-session metadata is the durable source for pane-level provider
/// accounting. A manual resume path must hydrate the same in-memory usage map
/// as daemon startup restore so `/status` does not reset token counts to zero.
#[test]
fn runtime_resume_restores_provider_token_usage_from_session_metadata() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-tokens"));
    let mut service = test_runtime_service();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved-tokens".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "resume with prior token totals".to_string(),
        })
        .unwrap();
    let saved_token_usage_key = crate::agent::ModelTokenUsageKey::new("openai", "gpt-saved");
    let saved_token_usage = crate::agent::ModelTokenUsage {
        input_tokens: 900,
        output_tokens: 80,
        reasoning_tokens: 33,
        cached_input_tokens: Some(450),
        cache_write_input_tokens: None,
    };
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[crate::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%1".to_string(),
                conversation_id: "saved-tokens".to_string(),
                prompt_cache_lineage_id: "lineage-saved-tokens".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                directive: Some("Prefer focused tests.".to_string()),
                routing_enabled: Some(true),
                approval_policy: Some("full-access".to_string()),
                working_directory: None,
                project_root: None,
                context_usage: Some("42%".to_string()),
                context_usage_snapshot: Some(crate::agent::AgentContextUsageSnapshot {
                    input_tokens: 420,
                    context_window_tokens: 1000,
                    cached_input_tokens: Some(450),
                }),
                token_usage: saved_token_usage,
                token_usage_by_model: std::collections::BTreeMap::from([(
                    saved_token_usage_key.clone(),
                    saved_token_usage,
                )]),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-tokens","method":"agent/shell/command","params":{"idempotency_key":"resume-tokens","input":"/resume saved-tokens"}}"#,
        &primary,
    );
    assert!(
        resumed.contains("conversation_id=saved-tokens"),
        "{resumed}"
    );
    assert_eq!(
        service.agent_routing_overrides.get("%1").copied(),
        Some(true)
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service
            .agent_context_usage_by_conversation
            .get("saved-tokens")
            .map(String::as_str),
        Some("42%")
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-token-status","method":"agent/shell/command","params":{"idempotency_key":"resume-token-status","input":"/status"}}"#,
        &primary,
    );

    assert!(
        status.contains(
            "| Pane agent tokens | gpt-saved via openai: input=450 cached_input=450 cache_hit=50.00% output=80 reasoning=33 total=980 |"
        ),
        "{status}"
    );
    assert!(
        status.contains("| openai | gpt-saved | 450 | 450 | 80 | 33 | 50.00% |"),
        "{status}"
    );
    let restored_metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(restored_metadata.len(), 1, "{restored_metadata:#?}");
    let restored_metadata = &restored_metadata[0];
    assert_eq!(
        restored_metadata.conversation_id, "saved-tokens",
        "{restored_metadata:#?}"
    );
    assert_eq!(
        restored_metadata.token_usage_by_model,
        std::collections::BTreeMap::from([(saved_token_usage_key.clone(), saved_token_usage,)]),
        "{restored_metadata:#?}"
    );

    let (_, mut profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    profile.provider = "openai".to_string();
    profile.model = "gpt-saved".to_string();
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(25),
            cache_write_input_tokens: None,
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(25),
            cache_write_input_tokens: None,
        },
        Some(&profile),
    );
    let resumed_status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-token-status-after-usage","method":"agent/shell/command","params":{"idempotency_key":"resume-token-status-after-usage","input":"/status"}}"#,
        &primary,
    );
    assert!(
        resumed_status.contains("| openai | gpt-saved | 525 | 475 | 100 | 38 | 47.50% |"),
        "{resumed_status}"
    );
    let resumed_metadata = transcript_store
        .load_agent_session_metadata(&mezzanine_session_id)
        .unwrap();
    assert_eq!(resumed_metadata.len(), 1, "{resumed_metadata:#?}");
    assert_eq!(
        resumed_metadata[0].token_usage_by_model,
        std::collections::BTreeMap::from([(
            saved_token_usage_key,
            crate::agent::ModelTokenUsage {
                input_tokens: 1000,
                output_tokens: 100,
                reasoning_tokens: 38,
                cached_input_tokens: Some(475),
                cache_write_input_tokens: None,
            },
        )]),
        "{resumed_metadata:#?}"
    );
}

/// Verifies that `/fork` returns a concrete runtime diagnostic when no
/// transcript store is attached instead of falling back to a generic
/// runtime-required placeholder.
#[test]
fn runtime_agent_shell_fork_reports_missing_transcript_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork-missing-store","method":"agent/shell/command","params":{"idempotency_key":"fork-missing-store","input":"/fork branch"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"fork""#), "{response}");
    assert!(
        response.contains("forked=false reason=transcript-store-unavailable"),
        "{response}"
    );
    assert!(response.contains("source=runtime-fork"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}
