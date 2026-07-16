//! Agent conversation lifecycle tests.

use super::*;

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
    service.terminate_all_pane_processes().unwrap();
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
    service.terminate_all_pane_processes().unwrap();
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
    service.terminate_all_pane_processes().unwrap();
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
    service.terminate_all_pane_processes().unwrap();
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
    let first_usage = mez_agent::ModelTokenUsage {
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
    let second_usage = mez_agent::ModelTokenUsage {
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
