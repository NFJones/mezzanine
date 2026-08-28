//! Agent conversation saved sessions tests.

use super::*;

/// Verifies `/name-session` assigns durable metadata to the current
/// zero-entry conversation without making it visible in `/resume` until it
/// has prompt history, while direct resume still restores the named session.
#[test]
fn runtime_agent_shell_names_and_resumes_zero_entry_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-name-session"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
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

    let named = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"name","method":"agent/shell/command","params":{"idempotency_key":"name","input":"/name-session Release investigation"}}"#,
        &primary,
    );
    assert!(named.contains("name-session"), "{named}");
    assert!(named.contains("named=true"), "{named}");
    assert_eq!(
        transcript_store
            .named_session(&conversation_id)
            .unwrap()
            .map(|session| session.name),
        Some("Release investigation".to_string())
    );

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/shell/command","params":{"idempotency_key":"list","input":"/resume"}}"#,
        &primary,
    );
    assert!(!picker.contains(&conversation_id), "{picker}");
    assert!(!picker.contains("Release investigation"), "{picker}");

    let resumed = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"resume","method":"agent/shell/command","params":{{"idempotency_key":"resume","input":"/resume {conversation_id}"}}}}"#
        ),
        &primary,
    );
    assert!(resumed.contains("entries=0"), "{resumed}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some(conversation_id.as_str())
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/name-session --clear` removes only durable name metadata,
/// preserves the active conversation and transcript, and is idempotent.
#[test]
fn runtime_agent_shell_clears_session_names_without_deleting_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-clear-session-name"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
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
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-clear-name".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "preserve this transcript".to_string(),
        })
        .unwrap();

    let named = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"name-before-clear","method":"agent/shell/command","params":{"idempotency_key":"name-before-clear","input":"/name-session Pinned investigation"}}"#,
        &primary,
    );
    assert!(named.contains("named=true"), "{named}");

    let cleared = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"clear-name","method":"agent/shell/command","params":{"idempotency_key":"clear-name","input":"/name-session --clear"}}"#,
        &primary,
    );
    assert!(cleared.contains("named=false"), "{cleared}");
    assert!(cleared.contains("cleared=true"), "{cleared}");
    assert!(
        transcript_store
            .named_session(&conversation_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(transcript_store.inspect(&conversation_id).unwrap().len(), 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some(conversation_id.as_str())
    );

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list-after-clear","method":"agent/shell/command","params":{"idempotency_key":"list-after-clear","input":"/resume"}}"#,
        &primary,
    );
    assert!(
        picker.contains(&format!("[`{conversation_id}`]")),
        "{picker}"
    );
    assert!(!picker.contains("Pinned investigation"), "{picker}");

    let repeated = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"clear-name-again","method":"agent/shell/command","params":{"idempotency_key":"clear-name-again","input":"/name-session --clear"}}"#,
        &primary,
    );
    assert!(repeated.contains("cleared=false"), "{repeated}");

    for (id, input) in [
        (
            "clear-name-mixed-after",
            "/name-session --clear replacement",
        ),
        (
            "clear-name-mixed-before",
            "/name-session replacement --clear",
        ),
    ] {
        let response = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{id}","method":"agent/shell/command","params":{{"idempotency_key":"{id}","input":"{input}"}}}}"#
            ),
            &primary,
        );
        assert!(response.contains("usage: /name-session"), "{response}");
    }
}

/// Verifies named conversations sort ahead of newer unnamed conversations in
/// the picker while `/resume --latest` remains based only on session activity.
#[test]
fn runtime_agent_shell_sorts_named_sessions_first_without_changing_latest() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-named-order"));
    let mut named_old = TranscriptEntry {
        conversation_id: "named-old".to_string(),
        sequence: 1,
        created_at_unix_seconds: 10,
        role: TranscriptRole::User,
        turn_id: "turn-named".to_string(),
        agent_id: "agent-%9".to_string(),
        pane_id: "%9".to_string(),
        content: "old named prompt".to_string(),
    };
    transcript_store.append(&named_old).unwrap();
    transcript_store
        .name_session("named-old", "Pinned work", 10, None)
        .unwrap();
    named_old.conversation_id = "recent-unnamed".to_string();
    named_old.created_at_unix_seconds = 20;
    named_old.turn_id = "turn-recent".to_string();
    named_old.content = "recent unnamed prompt".to_string();
    transcript_store.append(&named_old).unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list-order","method":"agent/shell/command","params":{"idempotency_key":"list-order","input":"/resume"}}"#,
        &primary,
    );
    let named_position = picker.find("[`named-old`]").unwrap();
    let unnamed_position = picker.find("[`recent-unnamed`]").unwrap();
    assert!(named_position < unnamed_position, "{picker}");

    let latest = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"latest-order","method":"agent/shell/command","params":{"idempotency_key":"latest-order","input":"/resume --latest"}}"#,
        &primary,
    );
    assert!(
        latest.contains("conversation_id=recent-unnamed"),
        "{latest}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the `/resume` browser presents saved-session columns and row
/// values in the requested user-facing order.
#[test]
fn runtime_resume_browser_orders_session_columns() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-column-order"));
    for (sequence, role, content) in [
        (1, TranscriptRole::System, "cwd=/tmp/resume-column-order"),
        (2, TranscriptRole::User, "latest saved prompt"),
    ] {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: "ordered-session".to_string(),
                sequence,
                created_at_unix_seconds: 20,
                role,
                turn_id: "turn-ordered".to_string(),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    transcript_store
        .name_session(
            "ordered-session",
            "Ordered investigation",
            20,
            Some("/tmp/resume-column-order".to_string()),
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store);

    let page = service
        .saved_sessions_record_browser()
        .unwrap()
        .render_page()
        .raw_markdown;
    assert!(
        page.contains(
            "| Conversation | Name | Latest prompt | Last active | Directory | Entries |"
        ),
        "{page}"
    );
    let row = page
        .lines()
        .find(|line| line.contains("ordered-session"))
        .expect("saved-session row should be rendered");
    let cells = row
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(cells.len(), 6, "{row}");
    assert!(cells[0].contains("ordered-session"), "{row}");
    assert_eq!(
        &cells[1..],
        &[
            "Ordered investigation",
            "latest saved prompt",
            "1970-01-01T00:00:20Z",
            "/tmp/resume-column-order",
            "2",
        ],
        "{row}"
    );
}

/// Verifies bare `/resume` initially limits conversations to the active pane
/// directory and that `a` switches between that scoped result and every saved
/// conversation without closing the picker.
#[test]
fn runtime_resume_browser_filters_current_directory_and_toggles_all_sessions() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-directory-scope"));
    for (conversation_id, directory, created_at) in [
        ("current-directory", "/tmp/resume-current", 20),
        ("other-directory", "/tmp/resume-other", 10),
    ] {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: 1,
                created_at_unix_seconds: created_at,
                role: TranscriptRole::System,
                turn_id: format!("turn-{conversation_id}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("cwd={directory}"),
            })
            .unwrap();
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: 2,
                created_at_unix_seconds: created_at,
                role: TranscriptRole::User,
                turn_id: format!("turn-{conversation_id}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("saved prompt for {conversation_id}"),
            })
            .unwrap();
        transcript_store
            .name_session(
                conversation_id,
                conversation_id,
                created_at,
                Some(directory.to_string()),
            )
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .set_pane_current_working_directory(pane_id.clone(), PathBuf::from("/tmp/resume-current"));

    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();
    let record_ids = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("resume picker should open")
        .browser
        .records()
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(record_ids, vec!["current-directory"]);

    service
        .apply_primary_display_overlay_input(&primary, b"a")
        .unwrap();
    let all_record_ids = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("all-sessions picker should remain open")
        .browser
        .records()
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(all_record_ids, vec!["current-directory", "other-directory"]);

    service
        .apply_primary_display_overlay_input(&primary, b"a")
        .unwrap();
    let scoped_record_ids = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("directory-scoped picker should remain open")
        .browser
        .records()
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(scoped_record_ids, vec!["current-directory"]);
}

/// Verifies `/resume` lists only conversations that retain a user prompt.
///
/// Routed workers and presentation-only records can leave durable directories
/// without a user-authored conversation to continue. The picker must exclude
/// those records while retaining the parent conversation that owns prompt
/// history.
#[test]
fn runtime_resume_browser_excludes_promptless_sessions() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-prompt-filter"));
    for (conversation_id, role, content) in [
        (
            "parent-session",
            TranscriptRole::User,
            "continue parent work",
        ),
        (
            "routed-turn-worker",
            TranscriptRole::Assistant,
            "worker output",
        ),
        (
            "system-only",
            TranscriptRole::System,
            "cwd=/tmp/prompt-filter",
        ),
    ] {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: 1,
                created_at_unix_seconds: 10,
                role,
                turn_id: format!("turn-{conversation_id}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    transcript_store
        .name_session("named-empty", "Empty session", 10, None)
        .unwrap();
    service.set_agent_transcript_store(transcript_store);

    let browser = service.saved_sessions_record_browser().unwrap();
    let record_ids = browser
        .records()
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(record_ids, vec!["parent-session"]);
}

/// Verifies `/resume` omits named metadata-only conversations because they do
/// not retain a user prompt to resume.
#[test]
fn runtime_resume_browser_omits_named_zero_entry_sessions() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-delete-named"));
    transcript_store
        .name_session("named-empty", "Pinned work", 10, None)
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());

    let browser = service.saved_sessions_record_browser().unwrap();
    assert!(browser.deletion_enabled());
    assert!(
        browser
            .render_page()
            .raw_markdown
            .contains("No saved agent sessions are available.")
    );
    assert!(
        transcript_store
            .named_session("named-empty")
            .unwrap()
            .is_some()
    );
}

/// Verifies the `/resume` browser `c` hotkey clears only the selected name,
/// preserves its transcript, and keeps that conversation selected after the
/// unnamed activity ordering moves it below a newer conversation.
#[test]
fn runtime_resume_browser_clear_name_hotkey_preserves_session_and_selection() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-clear-name"));
    for (conversation_id, created_at, content) in [
        ("named-old", 10, "named transcript"),
        ("recent-unnamed", 20, "recent transcript"),
    ] {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: 1,
                created_at_unix_seconds: created_at,
                role: TranscriptRole::User,
                turn_id: format!("turn-{conversation_id}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    transcript_store
        .name_session("named-old", "Pinned work", 10, None)
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    assert!(response.contains("`c` clear name"), "{response}");
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    service
        .apply_primary_display_overlay_input(&primary, b"c")
        .unwrap();

    assert!(
        transcript_store
            .named_session("named-old")
            .unwrap()
            .is_none()
    );
    assert_eq!(transcript_store.inspect("named-old").unwrap().len(), 1);
    let browser = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("saved-session browser should remain open");
    assert_eq!(browser.browser.active_record_id(), Some("named-old"));
    assert_eq!(browser.browser.records()[0].id, "recent-unnamed");
    assert_eq!(browser.browser.records()[1].id, "named-old");
    assert!(
        !browser
            .browser
            .render_page()
            .raw_markdown
            .contains("Pinned work")
    );

    service
        .apply_primary_display_overlay_input(&primary, b"c")
        .unwrap();
    assert_eq!(transcript_store.inspect("named-old").unwrap().len(), 1);
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|browser| browser.browser.active_record_id()),
        Some("named-old")
    );
}

/// Verifies the saved-session browser exposes every durable transcript entry
/// through `i` before Enter resumes the focused conversation.
///
/// Other record browsers open details on Enter, but `/resume` must submit the
/// selected conversation to the resume command so users can inspect its full
/// ordered transcript without leaving the picker or truncating its content.
#[test]
fn runtime_resume_browser_enter_resumes_and_i_opens_details() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-browser-keys"));
    let entries = [
        (
            TranscriptRole::User,
            "first user line\nsecond user line".to_string(),
        ),
        (TranscriptRole::Assistant, "assistant response".to_string()),
        (
            TranscriptRole::Tool,
            "structured_content: {\"text\":\"tool result\"}".to_string(),
        ),
        (TranscriptRole::System, "cwd=/tmp/saved-session".to_string()),
        (TranscriptRole::User, "x".repeat(200)),
    ];
    for (index, (role, content)) in entries.into_iter().enumerate() {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: "saved-session".to_string(),
                sequence: (index + 1) as u64,
                created_at_unix_seconds: 10 + index as u64,
                role,
                turn_id: "turn-saved".to_string(),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content,
            })
            .unwrap();
    }
    for sequence in 6..=65 {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: "saved-session".to_string(),
                sequence,
                created_at_unix_seconds: 10 + sequence,
                role: TranscriptRole::Assistant,
                turn_id: "turn-saved".to_string(),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: format!("later transcript entry {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests(&pane_id, &response)
        .unwrap();

    service
        .apply_primary_display_overlay_input(&primary, b"i")
        .unwrap();
    let detail = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .filter(|browser| browser.browser.is_detail_view())
        .map(|browser| browser.browser.render_page().raw_markdown)
        .expect("saved-session detail browser");
    for expected in [
        "## User entry 1",
        "first user line\n    second user line",
        "## Assistant entry 2",
        "assistant response",
        "## Tool entry 3",
        "tool result",
        "## System entry 4",
        "Session directory: /tmp/saved-session",
        "## User entry 5",
        &"x".repeat(200),
        "## Assistant entry 65",
        "later transcript entry 65",
    ] {
        assert!(detail.contains(expected), "{detail}");
    }
    assert!(
        detail.find("## User entry 1").unwrap() < detail.find("## User entry 5").unwrap(),
        "{detail}"
    );

    service
        .apply_primary_display_overlay_input(&primary, b"\x1b")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"\r")
        .unwrap();
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved-session")
    );
}

/// Verifies a named session with no durable transcript entries is omitted from
/// `/resume` rather than rendered as a resumable empty transcript.
#[test]
fn runtime_resume_browser_omits_empty_named_session_transcript() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-empty-detail"));
    transcript_store
        .name_session("empty-session", "Empty investigation", 10, None)
        .unwrap();
    service.set_agent_transcript_store(transcript_store);

    let browser = service.saved_sessions_record_browser().unwrap();
    let page = browser.render_page().raw_markdown;

    assert!(browser.records().is_empty());
    assert!(
        page.contains("No saved agent sessions are available."),
        "{page}"
    );
}

/// Verifies saved conversations bound to a live durable agent pane cannot be
/// deleted from `/resume`, preventing a later transcript append from silently
/// recreating a conversation the picker claimed to remove.
#[test]
fn runtime_resume_browser_rejects_deleting_active_sessions() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-delete-active"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "active-saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-active".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "keep this session".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "active-saved", 1)
        .unwrap();

    let error = service
        .delete_record_browser_entry(
            &crate::runtime::service_state::RuntimeRecordBrowserOverlaySource::SavedSessions {
                directory: None,
                default_directory: None,
            },
            "active-saved",
            0,
        )
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("cannot delete an active agent session")
    );
    assert_eq!(transcript_store.inspect("active-saved").unwrap().len(), 1);
}

/// Verifies presentation-only conversations are excluded from `/resume`.
///
/// Presentation output without a user prompt cannot restore a user-owned
/// conversation, so it must not be shown alongside resumable parent sessions.
#[test]
fn runtime_resume_omits_presentation_only_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-presentation-only"));
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: "presentation-only".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            pane_id: "%9".to_string(),
            turn_id: None,
            terminal_width: 80,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> presentation-only history".to_string()],
            copy_lines: vec!["presentation-only history".to_string()],
            ansi_text: None,
            source_text: Some("presentation-only history".to_string()),
            source_content_type: Some(mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string()),
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
    service.set_process_pane_screen(
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"presentation-picker","method":"agent/shell/command","params":{"idempotency_key":"presentation-picker","input":"/resume"}}"#,
        &primary,
    );
    assert!(!picker.contains("presentation-only"), "{picker}");
    assert!(
        picker.contains("No saved agent sessions are available."),
        "{picker}"
    );
}

/// Verifies a replay ownership failure restores the prior conversation and
/// both retained pane surfaces instead of leaving a partial resume binding.
#[test]
fn runtime_resume_replay_failure_restores_prior_pane_state() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-rollback"));
    let mezzanine_session_id = service.session().id.as_str().to_string();
    let target_usage_key = mez_agent::ModelTokenUsageKey::new("openai", "target-model");
    let target_usage = mez_agent::ModelTokenUsage {
        input_tokens: 800,
        output_tokens: 70,
        reasoning_tokens: 20,
        cached_input_tokens: Some(400),
        cache_write_input_tokens: None,
    };
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "resume-target".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-target".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "target prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: "resume-target".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            pane_id: "%9".to_string(),
            turn_id: None,
            terminal_width: 80,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["target presentation".to_string()],
            copy_lines: vec!["target presentation".to_string()],
            ansi_text: None,
            source_text: None,
            source_content_type: None,
        })
        .unwrap();
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[mez_agent::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%9".to_string(),
                conversation_id: "resume-target".to_string(),
                prompt_cache_lineage_id: "target-lineage".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                running_turn_kind: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: Some("target-profile".to_string()),
                planning_enabled: true,
                response_style: Some("concise".to_string()),
                directive: Some("Use the target directive.".to_string()),
                routing_enabled: Some(true),
                root_routing_policy: Some("in-place".to_string()),
                approval_policy: None,
                pane_permission_preset_override: Some("read-only".to_string()),
                pane_approval_policy_override: Some("full-access".to_string()),
                working_directory: None,
                project_root: None,
                token_usage: target_usage,
                token_usage_by_model: std::collections::BTreeMap::from([(
                    target_usage_key.clone(),
                    target_usage,
                )]),
                context_usage: Some("80%".to_string()),
                context_usage_snapshot: Some(mez_agent::AgentContextUsageSnapshot {
                    input_tokens: 800,
                    context_window_tokens: 1_000,
                    cached_input_tokens: Some(400),
                }),
                latest_request_usage: Some(mez_agent::LatestModelRequestUsage {
                    model: target_usage_key,
                    usage: target_usage,
                }),
            }],
        )
        .unwrap();
    let presentation_path = transcript_store.presentation_path("resume-target").unwrap();
    let corrupt = fs::read_to_string(&presentation_path).unwrap().replacen(
        "resume-target",
        "wrong-conversation",
        1,
    );
    fs::write(&presentation_path, corrupt).unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let prior_conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut process_screen = TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap();
    process_screen.feed(b"prior-process-surface");
    service.set_process_pane_screen("%1", process_screen);
    let mut agent_screen = TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap();
    agent_screen.feed(b"prior-agent-surface");
    service.set_agent_pane_screen("%1", &prior_conversation, agent_screen);
    let process_before = service.process_pane_screen("%1").unwrap().clone();
    let agent_before = service.agent_pane_screen("%1").unwrap().clone();
    let session_before = service.agent_shell_store().get("%1").unwrap().clone();
    let transcript_refs_before = service.persistence.pane_transcript_refs("%1");

    let failed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-failure","method":"agent/shell/command","params":{"idempotency_key":"resume-failure","input":"/resume resume-target"}}"#,
        &primary,
    );
    assert!(failed.contains("error"), "{failed}");
    assert!(
        failed.contains("presentation replay target does not match"),
        "{failed}"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some(prior_conversation.as_str())
    );
    assert_eq!(service.process_pane_screen("%1").unwrap(), &process_before);
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &agent_before);
    assert_eq!(service.agent_shell_store().get("%1"), Some(&session_before));
    assert_eq!(
        service.persistence.pane_transcript_refs("%1"),
        transcript_refs_before
    );
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .pane_profiles
            .contains_key("%1")
    );
    assert!(!service.agent_planning_enabled("%1"));
    assert_eq!(service.agent_response_style("%1"), None);
    assert_eq!(service.agent_routing_override("%1"), None);
    assert_eq!(service.agent_root_routing_policy_override("%1"), None);
    assert_eq!(service.integration.pane_permission_override("%1"), None);
    assert!(
        service
            .agent_token_usage_for_conversation("resume-target")
            .is_empty()
    );
    assert!(service.agent_token_usage_for_pane("%1").is_empty());
    assert_eq!(service.agent_context_usage_display("resume-target"), None);
    assert_eq!(service.agent_context_usage_snapshot("resume-target"), None);
    assert_eq!(service.agent_latest_request_usage("resume-target"), None);
}

/// Verifies that saved agent conversations can be listed, resumed into the
/// current pane, exposed to prompt context, and forked while keeping readline
/// prompt history shared across conversation bindings.
#[test]
fn runtime_agent_shell_resume_and_fork_manage_saved_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-fork"));
    let cwd = temp_root("runtime-agent-resume-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::System,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: format!("cwd={}", cwd.display()),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 2,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 3,
            created_at_unix_seconds: 2,
            role: mez_agent::transcript::TranscriptRole::User,
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
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::System,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: format!("cwd={}", cwd.display()),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 2,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
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
            source_text: None,
            source_content_type: None,
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: "saved".to_string(),
            sequence: 2,
            created_at_unix_seconds: 4,
            pane_id: "%9".to_string(),
            turn_id: Some("turn-old".to_string()),
            terminal_width: 80,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale cached presentation".to_string()],
            copy_lines: vec!["stale cached presentation".to_string()],
            ansi_text: None,
            source_text: Some("# Rebuilt heading\n\n- source replay uses active width".to_string()),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_current_working_directory("%1", cwd.clone());

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-list","method":"agent/shell/command","params":{"idempotency_key":"resume-list","input":"/resume"}}"#,
        &primary,
    );
    assert!(
        picker.contains("[`saved`](mez-agent:%2Fresume%20saved)"),
        "{picker}"
    );
    assert!(
        picker.contains("[`latest`](mez-agent:%2Fresume%20latest)"),
        "{picker}"
    );
    let saved_row = picker
        .lines()
        .find(|line| line.contains("[`saved`]"))
        .expect("saved session table row should exist");
    assert!(saved_row.contains("latest saved prompt"), "{picker}");

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

    let latest_conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let process_size = service.process_pane_screen("%1").unwrap().size();
    service
        .ensure_agent_pane_screen("%1", &latest_conversation_id, process_size)
        .unwrap()
        .feed(b"pre-resume stale cells\r\n");

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
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !resumed_pane_text.contains("pre-resume stale cells"),
        "{resumed_pane_text}"
    );
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
    let resumed_without_whitespace = resumed_pane_text
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        resumed_without_whitespace.contains("Rebuiltheading")
            && resumed_without_whitespace.contains("sourcereplayusesactivewidth")
            && !resumed_without_whitespace.contains("stalecachedpresentation"),
        "{resumed_pane_text}"
    );
    assert!(
        !resumed_pane_text.contains("Resumed Agent Session"),
        "{resumed_pane_text}"
    );
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved"),
        ]
    );
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == mez_agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved prompt")
    }));
    context.validate_placement_order().unwrap();
    let (_, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "saved-context-validation".to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 3,
        deadline_at_unix_millis: 0,
        policy_profile: "runtime".to_string(),
        model_profile: "test".to_string(),
        parent_turn_id: None,
        state: AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    };
    let request =
        crate::integrations::agent::context::assemble_model_request(&profile, &turn, &context)
            .unwrap();
    let replayed_user_messages = request
        .messages
        .iter()
        .filter(|message| message.source == ContextSourceKind::TranscriptUser)
        .map(|message| (message.role, message.content.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(replayed_user_messages.len(), 2);
    assert_eq!(
        replayed_user_messages[0].0,
        mez_agent::ModelMessageRole::User
    );
    assert!(replayed_user_messages[0].1.contains("saved prompt"));
    assert!(replayed_user_messages[1].1.contains("latest saved prompt"));

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
            String::from("/fork saved-fork"),
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
            .agent_prompt_inputs_for_tests()
            .get(&forked_pane)
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume saved"
    );
    service.terminate_all_pane_processes().unwrap();
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
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: session_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 11,
            role: mez_agent::transcript::TranscriptRole::User,
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
    let _ = service.drain_pane_io_transition().side_effects;

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
    assert!(service.primary_display_overlay().is_some());

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
            .primary_display_overlay()
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
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker link foreground shifted left in live view: {view:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left in live view: {view:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "resume picker active background shifted left in live view: {view:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker first session-id cell lost link foreground: {view:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline: {view:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
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
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: session_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 11,
            role: mez_agent::transcript::TranscriptRole::User,
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
    let _ = service.drain_pane_io_transition().side_effects;

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

    let modes = mez_mux::presentation::AttachedTerminalOutputModes {
        cursor_visible: current_view.cursor_visible,
        cursor_blink: current_view.cursor_blink,
        cursor_blink_interval_ms: current_view.cursor_blink_interval_ms,
        cursor_row: current_view.cursor_row,
        cursor_column: current_view.cursor_column,
        application_keypad: current_view.application_keypad,
        bracketed_paste: current_view.bracketed_paste,
        host_mouse_reporting: current_view.host_mouse_reporting,
        ..mez_mux::presentation::AttachedTerminalOutputModes::default()
    };
    let (previous_lines, previous_spans) =
        mez_mux::presentation::compose_client_presentation_with_styles(&previous_view, None);
    let (current_lines, current_spans) =
        mez_mux::presentation::compose_client_presentation_with_styles(&current_view, None);
    let previous_frame =
        mez_mux::attached_client::encode_attached_terminal_output_update_frame_with_styles(
            &previous_lines,
            &previous_spans,
            None,
            modes,
            None,
        );
    let previous_state = mez_mux::attached_client::AttachedTerminalOutputFrameState::new_with_modes(
        &previous_lines,
        &previous_spans,
        modes,
    );
    let update_frame =
        mez_mux::attached_client::encode_attached_terminal_output_update_frame_with_styles(
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
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker link foreground shifted left after attached frame update: {styled_lines:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left after attached frame update: {styled_lines:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "resume picker active background shifted left after attached frame update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker first session-id cell lost link foreground after attached frame update: {styled_lines:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline after attached frame update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "resume picker first session-id cell lost active background after attached frame update: {styled_lines:?}"
    );
}
