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
    service.set_pane_screen(
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
    service.set_pane_screen(
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

/// Verifies an ephemeral fork replays the complete parent projection, including
/// typed routed handoff context, captured at fork time without leaking later
/// parent records.
///
/// A routed or loop worker must receive both the summarized handoff and visible
/// parent answer. A recent-tail read would replace captured records with later
/// parent content instead of honoring the fork high-water mark.
#[test]
fn runtime_agent_loop_fork_context_honors_captured_parent_high_water_mark() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-fork-replay"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let parent_conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent_conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "captured-parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "captured parent message".to_string(),
        })
        .unwrap();
    let handoff = r#"{"version":1,"result_summary":"captured routed summary"}"#;
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent_conversation_id.clone(),
            sequence: 2,
            created_at_unix_seconds: 1,
            role: TranscriptRole::System,
            turn_id: "captured-parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: mez_agent::TranscriptContextEvent::RoutedHandoff {
                content: handoff.to_string(),
            }
            .to_transcript_content(),
        })
        .unwrap();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent_conversation_id.clone(),
            sequence: 3,
            created_at_unix_seconds: 1,
            role: TranscriptRole::Assistant,
            turn_id: "captured-parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "captured parent presentation".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 3)
        .unwrap();
    service
        .execute_agent_shell_loop_command("%1", "/loop --fork continue")
        .unwrap();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent_conversation_id,
            sequence: 4,
            created_at_unix_seconds: 2,
            role: TranscriptRole::User,
            turn_id: "later-parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "later parent message".to_string(),
        })
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    let replay = context
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::TranscriptUser)
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(replay.contains("captured parent message"), "{replay}");
    assert!(!replay.contains("later parent message"), "{replay}");
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker handoff context"
            && block.content == handoff
    }));
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block.content == "captured parent presentation"
    }));
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
    service.set_pane_screen(
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
    service.set_pane_screen(
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
    let loop_state = service.agent_loop_state("%1").unwrap();
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

/// Verifies ephemeral loop modes restore the exact parent agent screen and
/// agent-surface copy state when the logical loop returns to its parent.
#[test]
fn runtime_agent_loop_ephemeral_modes_restore_parent_projection() {
    for command in [
        "/loop --fork review this document",
        "/loop --new review this document",
    ] {
        let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
        let pane_id = service.active_pane_id().unwrap().to_string();
        let parent_conversation = service
            .agent_shell_store_mut()
            .enter_or_resume(&pane_id)
            .unwrap()
            .session_id
            .clone();
        let mut parent_screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 20).unwrap();
        parent_screen
            .feed(b"parent one\r\nparent two\r\nparent three\r\nparent four\r\nparent five");
        service.set_agent_pane_screen(&pane_id, &parent_conversation, parent_screen);
        let parent_screen = service.agent_pane_screen(&pane_id).unwrap().clone();
        let parent_copy_state = {
            let copy_mode = service.ensure_active_copy_mode(&pane_id).unwrap();
            copy_mode.scroll_to_top();
            copy_mode
                .select_range(
                    CopyPosition { line: 0, column: 0 },
                    CopyPosition { line: 0, column: 5 },
                )
                .unwrap();
            (copy_mode.scroll_top(), copy_mode.selection())
        };

        service
            .execute_agent_shell_loop_command(&pane_id, command)
            .unwrap();
        assert_ne!(
            service
                .agent_shell_store()
                .get(&pane_id)
                .unwrap()
                .session_id,
            parent_conversation
        );
        let stopped = service.stop_agent_turn_for_pane(&pane_id).unwrap();
        assert!(!stopped.turn_id.is_empty());

        assert_eq!(
            service
                .agent_shell_store()
                .get(&pane_id)
                .unwrap()
                .session_id,
            parent_conversation
        );
        assert_eq!(service.agent_pane_screen(&pane_id).unwrap(), &parent_screen);
        let agent_key = service.copy_mode_key(&pane_id, crate::runtime::PaneSurfaceKind::Agent);
        assert_eq!(
            service
                .active_copy_modes()
                .get(&agent_key)
                .map(|copy_mode| (copy_mode.scroll_top(), copy_mode.selection())),
            Some(parent_copy_state)
        );
    }
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
    service.set_pane_screen("%1".to_string(), screen);
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
            .process_pane_screen("%1")
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

/// Verifies stopping an active turn durably records its prompt before cleanup,
/// so a later continuation sees the interrupted task rather than only older
/// completed conversation history.
#[test]
fn runtime_interrupted_turn_prompt_is_persisted_for_continuation_context() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-interrupted-turn"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
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

    let interrupted = service
        .start_agent_prompt_turn("%1", "repair the interrupted defect")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();

    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert_eq!(entries.len(), 1, "{entries:#?}");
    assert_eq!(entries[0].role, TranscriptRole::System);
    assert_eq!(entries[0].turn_id, interrupted.turn_id);
    assert!(
        entries[0].content.contains("interrupted_turn"),
        "{entries:#?}"
    );
    assert!(entries[0].content.contains("repair the interrupted defect"));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == interrupted.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );

    let continuation = service.start_agent_prompt_turn("%1", "Continue").unwrap();
    let context = service
        .agent_turn_contexts()
        .get(&continuation.turn_id)
        .unwrap();
    let interrupted_context = context
        .blocks()
        .iter()
        .find(|block| block.label == "interrupted turn context")
        .unwrap();
    assert_eq!(interrupted_context.source, ContextSourceKind::RuntimeHint);
    assert!(
        interrupted_context
            .content
            .contains("repair the interrupted defect"),
        "{}",
        interrupted_context.content
    );
    assert!(
        interrupted_context
            .content
            .contains("must not be resumed as active execution"),
        "{}",
        interrupted_context.content
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies immediate continuation context includes an interrupted prompt while
/// adapter-owned transcript persistence is still queued.
///
/// Attached runtime commands can submit the next prompt before the persistence
/// worker appends the interruption record, so history hydration must include
/// canonical pending entries without treating cancelled work as completed.
#[test]
fn runtime_interrupted_turn_pending_transcript_is_visible_to_immediate_continuation() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-interrupted-turn-pending"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.persistence.enable_transcript_adapter();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .start_agent_prompt_turn("%1", "repair the interrupted deferred defect")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();

    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(transcript_store.inspect(&conversation_id).is_err());

    let continuation = service.start_agent_prompt_turn("%1", "Continue").unwrap();
    let context = service
        .agent_turn_contexts()
        .get(&continuation.turn_id)
        .unwrap();
    let interrupted_context = context
        .blocks()
        .iter()
        .find(|block| block.label == "interrupted turn context")
        .unwrap();

    assert!(
        interrupted_context
            .content
            .contains("repair the interrupted deferred defect"),
        "{}",
        interrupted_context.content
    );
    assert!(
        interrupted_context
            .content
            .contains("no action result was available when the turn stopped"),
        "{}",
        interrupted_context.content
    );
    let side_effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let (presentations, persistence): (Vec<_>, Vec<_>) = side_effects
        .into_iter()
        .partition(|effect| matches!(effect, RuntimeSideEffect::PersistPresentationEntries { .. }));
    assert_eq!(presentations.len(), 3, "{presentations:#?}");
    assert!(
        matches!(
            persistence.as_slice(),
            [
                RuntimeSideEffect::PersistTranscriptEntries { .. },
                RuntimeSideEffect::PersistSavedSessionRetention {
                    protected_conversation_ids,
                    schedule_next: false,
                    ..
                }
            ] if protected_conversation_ids.contains(&conversation_id)
        ),
        "{persistence:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}
