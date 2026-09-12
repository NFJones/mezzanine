//! Agent conversation saved sessions tests.

use super::*;
use crate::runtime::{PersistenceEvent, SessionArchiveOperation};

/// Returns one record-browser metadata value by key.
fn record_metadata_value(
    record: &mez_mux::record_browser::RecordBrowserRecord,
    key: &str,
) -> String {
    record
        .metadata
        .iter()
        .find(|(metadata_key, _)| metadata_key == key)
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

/// Verifies retention work is duplicate-suppressed while pending, deferred
/// requests rerun after total failure, and partial reports settle without an
/// overlay render when no conversation was deleted.
#[test]
fn runtime_saved_session_retention_settlement_preserves_deferred_work() {
    let root = temp_root("runtime-saved-session-retention-settlement");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store);

    assert!(
        service
            .queue_saved_session_retention_operation(100, false)
            .unwrap()
            .applied
    );
    assert!(
        !service
            .queue_saved_session_retention_operation(101, true)
            .unwrap()
            .applied
    );
    let initial = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert!(matches!(
        initial.as_slice(),
        [RuntimeSideEffect::PersistSavedSessionRetention {
            now_unix_seconds: 100,
            schedule_next: false,
            ..
        }]
    ));

    let rerun = service
        .apply_persistence_transition(PersistenceEvent::SavedSessionRetentionFailed {
            error: "catalog unavailable".to_string(),
            schedule_next: false,
        })
        .unwrap();
    assert!(matches!(
        rerun.side_effects.as_slice(),
        [RuntimeSideEffect::PersistSavedSessionRetention {
            schedule_next: true,
            ..
        }]
    ));

    let partial = service
        .apply_persistence_transition(PersistenceEvent::SavedSessionRetentionCompleted {
            report: crate::storage::transcript::SavedSessionRetentionReport {
                deleted_conversation_ids: Vec::new(),
                failures: vec![crate::storage::transcript::SavedSessionRetentionFailure {
                    conversation_id: "failed-retention".to_string(),
                    error: "conversation lock unavailable".to_string(),
                }],
            },
            schedule_next: true,
        })
        .unwrap();
    assert!(matches!(
        partial.side_effects.as_slice(),
        [RuntimeSideEffect::ScheduleTimer { key, delay_ms }]
            if key.kind == crate::runtime::RuntimeTimerKind::SavedSessionRetention
                && *delay_ms == 24 * 60 * 60 * 1_000
    ));
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies archive planning rejects live durable bindings and suppresses
/// duplicate lifecycle work until the persistence worker settles it.
#[test]
fn runtime_session_archive_planning_rejects_live_and_suppresses_duplicates() {
    let root = temp_root("runtime-session-archive-planning");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let live_conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let error = service
        .queue_session_archive_operation(
            &live_conversation_id,
            SessionArchiveOperation::Archive {
                archived_at_unix_seconds: 20,
            },
        )
        .unwrap_err();
    assert!(error.message().contains("live durable pane"));

    store
        .append(&TranscriptEntry {
            conversation_id: "detached-archive".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-detached".to_string(),
            agent_id: "agent-detached".to_string(),
            pane_id: "%9".to_string(),
            content: "detached archive candidate".to_string(),
        })
        .unwrap();
    let first = service
        .queue_session_archive_operation(
            "detached-archive",
            SessionArchiveOperation::Archive {
                archived_at_unix_seconds: 20,
            },
        )
        .unwrap();
    let duplicate = service
        .queue_session_archive_operation("detached-archive", SessionArchiveOperation::Restore)
        .unwrap();
    assert!(first.applied);
    assert!(!duplicate.applied);
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        effects.as_slice(),
        [RuntimeSideEffect::PersistSessionArchive {
            conversation_id,
            operation: SessionArchiveOperation::Archive { .. },
            ..
        }] if conversation_id == "detached-archive"
    ));

    service
        .apply_persistence_transition(PersistenceEvent::SessionArchiveFailed {
            conversation_id: "detached-archive".to_string(),
            operation: SessionArchiveOperation::Archive {
                archived_at_unix_seconds: 20,
            },
            error: "test failure".to_string(),
        })
        .unwrap();
    assert!(
        service
            .queue_session_archive_operation("detached-archive", SessionArchiveOperation::Delete,)
            .unwrap()
            .applied
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies the resume pager toggles independently between active and archived
/// sessions, queues archive work, refreshes after settlement, and exposes
/// bounded archive metadata and lifecycle-aware deletion.
#[test]
fn runtime_resume_browser_archives_browses_details_and_deletes_sessions() {
    let root = temp_root("runtime-resume-archive-browser");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&TranscriptEntry {
            conversation_id: "browser-archive".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-browser-archive".to_string(),
            agent_id: "agent-browser".to_string(),
            pane_id: "%9".to_string(),
            content: "archive browser prompt".to_string(),
        })
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();

    service
        .apply_primary_display_overlay_input(&primary, b"r")
        .unwrap();
    let archived_empty = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap()
        .browser
        .render_page()
        .raw_markdown;
    assert!(
        archived_empty.contains("Archived Agent Sessions"),
        "{archived_empty}"
    );
    assert!(
        archived_empty.contains("No archived agent sessions are available."),
        "{archived_empty}"
    );

    service
        .apply_primary_display_overlay_input(&primary, b"r")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"A")
        .unwrap();
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let [
        RuntimeSideEffect::PersistSessionArchive {
            conversation_id,
            operation:
                SessionArchiveOperation::Archive {
                    archived_at_unix_seconds,
                },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected one archive persistence effect: {effects:?}");
    };
    assert_eq!(conversation_id, "browser-archive");
    let archived = store
        .archive_session(conversation_id, *archived_at_unix_seconds)
        .unwrap();
    service
        .apply_persistence_transition(PersistenceEvent::SessionArchiveCompleted {
            conversation_id: conversation_id.clone(),
            operation: SessionArchiveOperation::Archive {
                archived_at_unix_seconds: *archived_at_unix_seconds,
            },
            bytes: archived.compressed_bytes as usize,
        })
        .unwrap();
    assert!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .unwrap()
            .browser
            .records()
            .is_empty()
    );
    let settled_page = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap()
        .browser
        .render_page()
        .raw_markdown;
    assert!(
        !settled_page.contains("archive completed for browser-archive"),
        "{settled_page}"
    );

    service
        .apply_primary_display_overlay_input(&primary, b"r")
        .unwrap();
    let archived_page = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap()
        .browser
        .render_page()
        .raw_markdown;
    assert!(archived_page.contains("| Archived at |"), "{archived_page}");
    assert!(archived_page.contains("browser-archive"), "{archived_page}");
    service
        .apply_primary_display_overlay_input(&primary, b"i")
        .unwrap();
    let detail = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap()
        .browser
        .render_page()
        .raw_markdown;
    assert!(detail.contains("## Archived session"), "{detail}");
    assert!(detail.contains("Compressed bytes"), "{detail}");
    assert!(detail.contains("SHA-256"), "{detail}");
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"d")
        .unwrap();
    assert!(
        store
            .inspect_archived_session("browser-archive")
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .unwrap()
            .browser
            .records()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies Enter on an archived resume row queues restore work and resumes
/// the original pane only after the persistence worker reports success.
#[test]
fn runtime_resume_browser_restores_then_resumes_archived_session() {
    let root = temp_root("runtime-resume-restore-enter");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&TranscriptEntry {
            conversation_id: "restore-enter".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-restore-enter".to_string(),
            agent_id: "agent-restore".to_string(),
            pane_id: "%9".to_string(),
            content: "restore then resume".to_string(),
        })
        .unwrap();
    store.archive_session("restore-enter", 20).unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"r")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"\r")
        .unwrap();

    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert!(matches!(
        effects.as_slice(),
        [RuntimeSideEffect::PersistSessionArchive {
            conversation_id,
            operation: SessionArchiveOperation::Restore,
            ..
        }] if conversation_id == "restore-enter"
    ));
    store.restore_archived_session("restore-enter").unwrap();
    service
        .apply_persistence_transition(PersistenceEvent::SessionArchiveCompleted {
            conversation_id: "restore-enter".to_string(),
            operation: SessionArchiveOperation::Restore,
            bytes: 0,
        })
        .unwrap();

    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("restore-enter")
    );
    assert!(service.primary_display_overlay().is_none());
    assert_eq!(store.inspect("restore-enter").unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

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

    let objective = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"objective","method":"agent/shell/command","params":{"idempotency_key":"objective","input":"/objective Preserve the named session"}}"#,
        &primary,
    );
    assert!(objective.contains("source=user"), "{objective}");
    assert_eq!(
        transcript_store
            .catalog_saved_session(&conversation_id)
            .unwrap()
            .and_then(|session| session.name),
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

/// Verifies an objective makes a fresh zero-entry conversation durable enough
/// to survive catalog rebuild, exact resume after `/new`, and archive/restore.
///
/// Objective text remains in versioned metadata rather than catalog columns;
/// the catalog row carries only zero-entry lifecycle eligibility.
#[test]
fn runtime_objective_only_zero_entry_conversation_resumes_and_archives() {
    let root = temp_root("runtime-objective-zero-entry-lifecycle");
    let transcript_store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
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

    let objective = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"objective-zero-entry","method":"agent/shell/command","params":{"idempotency_key":"objective-zero-entry","input":"/objective Preserve zero-entry lifecycle"}}"#,
        &primary,
    );
    assert!(objective.contains("source=user"), "{objective}");
    assert_eq!(
        transcript_store
            .catalog_saved_session(&conversation_id)
            .unwrap()
            .unwrap()
            .summary
            .entries,
        0
    );

    let fresh = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"new-after-objective","method":"agent/shell/command","params":{"idempotency_key":"new-after-objective","input":"/new"}}"#,
        &primary,
    );
    assert!(fresh.contains("new=true"), "{fresh}");
    transcript_store.rebuild_catalog(20).unwrap();

    let resumed = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"resume-objective-zero-entry","method":"agent/shell/command","params":{{"idempotency_key":"resume-objective-zero-entry","input":"/resume {conversation_id}"}}}}"#
        ),
        &primary,
    );
    assert!(resumed.contains("entries=0"), "{resumed}");
    assert_eq!(
        transcript_store
            .user_objective(&conversation_id)
            .unwrap()
            .as_deref(),
        Some("Preserve zero-entry lifecycle")
    );

    service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"new-before-archive","method":"agent/shell/command","params":{"idempotency_key":"new-before-archive","input":"/new"}}"#,
        &primary,
    );
    transcript_store
        .archive_session(&conversation_id, 30)
        .unwrap();
    transcript_store
        .restore_archived_session(&conversation_id)
        .unwrap();
    assert_eq!(
        transcript_store
            .saved_session(&conversation_id)
            .unwrap()
            .unwrap()
            .summary
            .entries,
        0
    );
    assert_eq!(
        transcript_store
            .user_objective(&conversation_id)
            .unwrap()
            .as_deref(),
        Some("Preserve zero-entry lifecycle")
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_dir_all(root);
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
    assert!(picker.contains(&format!("`{conversation_id}`")), "{picker}");
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

/// Verifies `/name-session --ephemeral` assigns a real name that a plain
/// assignment promotes back to durable, that the name still renders, and that
/// `--clear` and flag misuse keep their meaning.
#[test]
fn runtime_agent_shell_assigns_ephemeral_names_without_picker_preference() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-ephemeral-session-name"));
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
            turn_id: "turn-ephemeral-name".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "preserve this transcript".to_string(),
        })
        .unwrap();

    let named = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"ephemeral-name","method":"agent/shell/command","params":{"idempotency_key":"ephemeral-name","input":"/name-session --ephemeral Scoped investigation"}}"#,
        &primary,
    );
    assert!(named.contains("named=true"), "{named}");
    assert!(named.contains("ephemeral=true"), "{named}");
    let stored = transcript_store
        .named_session(&conversation_id)
        .unwrap()
        .unwrap();
    assert!(stored.ephemeral);
    assert_eq!(stored.name, "Scoped investigation");
    let browser = service.saved_sessions_record_browser().unwrap();
    let row = browser
        .records()
        .iter()
        .find(|record| record.id == conversation_id)
        .expect("an ephemeral named row should still be rendered");
    assert_eq!(
        row.title,
        format!("{conversation_id} - Scoped investigation")
    );

    let promoted_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"promote-name","method":"agent/shell/command","params":{"idempotency_key":"promote-name","input":"/name-session Durable investigation"}}"#,
        &primary,
    );
    assert!(
        promoted_response.contains("named=true"),
        "{promoted_response}"
    );
    assert!(
        promoted_response.contains("ephemeral=false"),
        "{promoted_response}"
    );
    let promoted_session = transcript_store
        .named_session(&conversation_id)
        .unwrap()
        .unwrap();
    assert!(!promoted_session.ephemeral);
    assert_eq!(promoted_session.name, "Durable investigation");

    for (id, input) in [
        ("ephemeral-without-name", "/name-session --ephemeral"),
        (
            "ephemeral-unknown-flag",
            "/name-session --unknown investigation",
        ),
        ("ephemeral-with-clear", "/name-session --ephemeral --clear"),
    ] {
        let response = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{id}","method":"agent/shell/command","params":{{"idempotency_key":"{id}","input":"{input}"}}}}"#
            ),
            &primary,
        );
        assert!(response.contains("usage: /name-session"), "{response}");
        assert!(
            transcript_store
                .named_session(&conversation_id)
                .unwrap()
                .is_some(),
            "a rejected invocation must not change the name"
        );
    }

    let cleared = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"clear-ephemeral-name","method":"agent/shell/command","params":{"idempotency_key":"clear-ephemeral-name","input":"/name-session --clear"}}"#,
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
    service.terminate_all_pane_processes().unwrap();
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
        .name_session("named-old", "Pinned work", 10, None, false)
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
    let named_position = picker.find("`named-old`").unwrap();
    let unnamed_position = picker.find("`recent-unnamed`").unwrap();
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
            false,
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

/// Verifies the resume browser shows the derived title for an unnamed
/// conversation, the manual name once `/name-session` sets one, and the derived
/// title again after `/name-session --clear`.
#[test]
fn runtime_resume_browser_renders_derived_and_manual_session_titles() {
    let root = temp_root("runtime-resume-derived-title");
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(root.clone());
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
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 20,
            role: TranscriptRole::User,
            turn_id: "turn-derived-title".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "derived first prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .mirror_session_objective(&conversation_id, "Mirror objective title", 21)
        .unwrap();

    let browser = service.saved_sessions_record_browser().unwrap();
    let derived = browser.records().first().expect("derived row");
    assert_eq!(derived.id, conversation_id);
    assert_eq!(
        derived.title,
        format!("{conversation_id} - Mirror objective title")
    );
    assert_eq!(
        record_metadata_value(derived, "name"),
        "Mirror objective title"
    );
    assert_eq!(
        record_metadata_value(derived, "title"),
        "Mirror objective title"
    );

    let named = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"name","method":"agent/shell/command","params":{"idempotency_key":"name","input":"/name-session Operator name"}}"#,
        &primary,
    );
    assert!(named.contains("named=true"), "{named}");
    let browser = service.saved_sessions_record_browser().unwrap();
    let named = browser.records().first().expect("named row");
    assert_eq!(named.title, format!("{conversation_id} - Operator name"));
    assert_eq!(record_metadata_value(named, "name"), "Operator name");
    assert_eq!(
        record_metadata_value(named, "title"),
        "Mirror objective title"
    );

    let cleared = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"clear","method":"agent/shell/command","params":{"idempotency_key":"clear","input":"/name-session --clear"}}"#,
        &primary,
    );
    assert!(cleared.contains("cleared=true"), "{cleared}");
    let browser = service.saved_sessions_record_browser().unwrap();
    let cleared = browser.records().first().expect("cleared row");
    assert_eq!(
        cleared.title,
        format!("{conversation_id} - Mirror objective title")
    );
    assert_eq!(
        record_metadata_value(cleared, "name"),
        "Mirror objective title"
    );
    assert_eq!(
        record_metadata_value(cleared, "title"),
        "Mirror objective title"
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Returns the open resume picker's scope toggle state and rendered page.
fn open_resume_scope_state(service: &crate::runtime::RuntimeSessionService) -> (bool, String) {
    let browser = &service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("resume picker should stay open")
        .browser;
    (
        browser.scope_toggle_enabled(),
        browser.render_page().markdown,
    )
}

/// Verifies an unfiltered picker keeps its all-directories scope line and stays
/// toggle-free when a derived-title refresh rebuilds the open browser.
#[test]
fn runtime_title_refresh_keeps_unfiltered_resume_scope() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-title-refresh-scope"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "refresh-scope".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-refresh-scope".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "refresh prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();
    let (toggle, rendered) = open_resume_scope_state(&service);
    assert!(!toggle, "an unfiltered picker has no scope toggle");
    assert!(
        rendered.contains("**Scope:** all directories"),
        "{rendered}"
    );

    assert!(service.mirror_runtime_agent_objective("refresh-scope", Some("Refreshed objective")));
    let (toggle, rendered) = open_resume_scope_state(&service);
    assert!(
        !toggle,
        "a title refresh must not enable the scope toggle for an unfiltered picker"
    );
    assert!(
        rendered.contains("**Scope:** all directories"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Refreshed objective"),
        "the refreshed row renders the new derived title: {rendered}"
    );
    let _ = std::fs::remove_dir_all(transcript_store.root());
}

/// Verifies a cleared published objective retires the mirror so resolution
/// falls through to the first prompt.
#[test]
fn runtime_cleared_objective_retires_the_mirrored_title() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-cleared-objective"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "cleared-objective".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-cleared-objective".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "first prompt fallback".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());

    assert!(
        service.mirror_runtime_agent_objective("cleared-objective", Some("Objective to clear"))
    );
    let browser = service.saved_sessions_record_browser().unwrap();
    assert_eq!(
        browser.records()[0].title,
        "cleared-objective - Objective to clear"
    );

    assert!(
        service.mirror_runtime_agent_objective("cleared-objective", None),
        "a cleared objective retires the retained mirror"
    );
    assert!(
        transcript_store
            .session_objective_mirror("cleared-objective")
            .unwrap()
            .is_none()
    );
    let browser = service.saved_sessions_record_browser().unwrap();
    assert_eq!(
        browser.records()[0].title,
        "cleared-objective - first prompt fallback",
        "resolution falls through to the first prompt"
    );
    let _ = std::fs::remove_dir_all(transcript_store.root());
}

/// Verifies ephemeral routed conversations never grow the objective mirror
/// index while a durable conversation still mirrors its objective.
#[test]
fn runtime_ephemeral_conversation_does_not_mirror_an_objective() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-ephemeral-mirror"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let durable_conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage("%2", "routed-parent-1-worker", 0, None)
        .unwrap();

    assert!(
        !service.mirror_runtime_agent_objective("routed-parent-1-worker", Some("Worker objective")),
        "an ephemeral routed conversation must not mirror an objective"
    );
    assert!(
        transcript_store
            .session_objective_mirror("routed-parent-1-worker")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        transcript_store
            .session_objective_mirror_status()
            .index_writes,
        0,
        "an ephemeral conversation must not write the mirror index"
    );

    assert!(
        service.mirror_runtime_agent_objective(&durable_conversation_id, Some("Durable objective"))
    );
    assert_eq!(
        transcript_store
            .session_objective_mirror(&durable_conversation_id)
            .unwrap()
            .map(|mirror| mirror.objective),
        Some("Durable objective".to_string())
    );
    let _ = std::fs::remove_dir_all(transcript_store.root());
}

/// Verifies a named row keeps its manual name and id column while the derived
/// title stays exposed on the detail path without a new table column.
#[test]
fn runtime_named_saved_session_exposes_derived_title_in_detail() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-named-title-detail"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "named-detail-session".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-named-detail".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "named detail prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .mirror_session_objective("named-detail-session", "Mirror objective title", 20)
        .unwrap();
    transcript_store
        .name_session("named-detail-session", "Operator name", 21, None, false)
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());

    let mut browser = service.saved_sessions_record_browser().unwrap();
    let list = browser.render_page().markdown;
    assert!(
        list.contains(
            "| Conversation | Name | Latest prompt | Last active | Directory | Entries |"
        ),
        "the picker keeps its existing column set: {list}"
    );
    let row = &browser.records()[0];
    assert_eq!(row.id, "named-detail-session");
    assert_eq!(row.title, "named-detail-session - Operator name");
    assert_eq!(record_metadata_value(row, "name"), "Operator name");
    assert_eq!(
        record_metadata_value(row, "title"),
        "Mirror objective title"
    );
    assert!(list.contains("Operator name"), "{list}");

    browser.show_first_record_detail();
    let detail = browser.render_page().markdown;
    assert!(
        detail.contains("# named-detail-session - Operator name"),
        "{detail}"
    );
    assert!(
        detail.contains("| title | Mirror objective title |"),
        "a named row still exposes its derived title on the detail path: {detail}"
    );
    let _ = std::fs::remove_dir_all(transcript_store.root());
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
                false,
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

/// Verifies the resume overlay retains only one bounded catalog page and
/// fetches adjacent keyset pages when keyboard focus crosses either edge.
///
/// The test also submits overlay search and confirms the backend query resets
/// pagination while returning only matching metadata rows.
#[test]
fn runtime_resume_browser_pages_and_searches_catalog_results() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-pages"));
    for index in 0..45 {
        let conversation_id = format!("paged-{index:02}");
        transcript_store
            .append(&TranscriptEntry {
                conversation_id,
                sequence: 1,
                created_at_unix_seconds: 100 - index,
                role: TranscriptRole::User,
                turn_id: format!("turn-{index}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: if index == 17 {
                    "unique pagination needle".to_string()
                } else {
                    format!("ordinary paged prompt {index}")
                },
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 12).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();
    let first_ids = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap()
        .browser
        .records()
        .iter()
        .map(|record| record.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(first_ids.len(), 20);

    for _ in 1..first_ids.len() {
        service
            .apply_primary_display_overlay_input(&primary, b"\x1b[B")
            .unwrap();
    }
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    let second_ids = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap()
        .browser
        .records()
        .iter()
        .map(|record| record.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(second_ids.len(), 20);
    assert!(first_ids.iter().all(|id| !second_ids.contains(id)));

    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[A")
        .unwrap();
    let previous_ids = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap()
        .browser
        .records()
        .iter()
        .map(|record| record.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(previous_ids, first_ids);

    service
        .apply_primary_display_overlay_input(&primary, b"/")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"needle")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"\r")
        .unwrap();
    let searched = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .unwrap();
    assert_eq!(searched.browser.records().len(), 1);
    assert_eq!(searched.browser.records()[0].id, "paged-17");
}

/// Verifies delegated conversations are hidden from discovery by default,
/// independently toggleable with `u`, excluded from `--latest`, and still
/// directly resumable by conversation UUID.
///
/// Persisting child work must not make it look like a root conversation, while
/// explicit recovery remains available when the caller already knows its id.
#[test]
fn runtime_resume_hides_subagents_but_allows_toggle_and_direct_resume() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-subagents"));
    for (conversation_id, created_at, content) in [
        ("root-session", 10, "continue root work"),
        ("child-session", 20, "continue delegated work"),
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
        .save_subagent_conversation_contract(
            "child-session",
            mez_agent::SubagentSessionLineage {
                parent_agent_id: "agent-%9".to_string(),
                root_agent_id: "agent-%1".to_string(),
                depth: 2,
                display_name: "resumed child".to_string(),
                terminal: true,
            },
            mez_agent::AllowedActionSet::say_only(),
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();
    let browser = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("resume picker should open");
    assert_eq!(browser.browser.active_record_id(), Some("root-session"));
    assert_eq!(browser.browser.records().len(), 1);

    service
        .apply_primary_display_overlay_input(&primary, b"u")
        .unwrap();
    let browser = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("subagent-inclusive picker should remain open");
    assert_eq!(browser.browser.active_record_id(), Some("root-session"));
    assert_eq!(
        browser
            .browser
            .records()
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        vec!["child-session", "root-session"]
    );

    service
        .apply_primary_display_overlay_input(&primary, b"u")
        .unwrap();
    let browser = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("root-only picker should remain open");
    assert_eq!(browser.browser.active_record_id(), Some("root-session"));
    assert_eq!(browser.browser.records().len(), 1);

    let latest = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"latest-root","method":"agent/shell/command","params":{"idempotency_key":"latest-root","input":"/resume --latest"}}"#,
        &primary,
    );
    assert!(latest.contains("conversation_id=root-session"), "{latest}");

    let direct = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"direct-child","method":"agent/shell/command","params":{"idempotency_key":"direct-child","input":"/resume child-session"}}"#,
        &primary,
    );
    assert!(direct.contains("conversation_id=child-session"), "{direct}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("child-session")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.conversation_kind),
        Some(mez_agent::AgentConversationKind::Subagent)
    );
    assert_eq!(
        service.subagent_lineage("agent-%1"),
        Some(&RuntimeSubagentLineage {
            parent_agent_id: "agent-%9".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 2,
            display_name: "resumed child".to_string(),
            terminal: true,
        })
    );
    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: true,
                read_scopes: Vec::new(),
                read_scopes_defaulted: true,
                write_scopes: Vec::new(),
                write_scopes_defaulted: true,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "attempt terminal delegation".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("restored subagent lineage has no live parent authority"),
        "{error}"
    );
    service.checkpoint_agent_session_metadata().unwrap();
    let mut restarted = test_runtime_service();
    restarted.session.id = service.session().id.clone();
    restarted.set_agent_transcript_store(transcript_store);
    assert_eq!(
        restarted
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        1
    );
    assert_eq!(
        restarted.subagent_lineage("agent-%1"),
        Some(&RuntimeSubagentLineage {
            parent_agent_id: "agent-%9".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 2,
            display_name: "resumed child".to_string(),
            terminal: true,
        })
    );
    assert!(!restarted.subagent_lineage_has_live_parent_authority("agent-%1"));
}

/// Verifies direct resume retains durable delegation ceilings while refusing
/// to treat a reused historical parent pane as current permission or scope
/// authority.
#[test]
fn runtime_direct_resume_keeps_structural_lineage_without_stale_parent_authority() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-stale-parent"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "resumed-child".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-resumed-child".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "resume delegated work".to_string(),
        })
        .unwrap();
    transcript_store
        .save_subagent_conversation_contract(
            "resumed-child",
            mez_agent::SubagentSessionLineage {
                parent_agent_id: "agent-%1".to_string(),
                root_agent_id: "agent-%1".to_string(),
                depth: 2,
                display_name: "resumed child".to_string(),
                terminal: false,
            },
            mez_agent::AllowedActionSet::say_only(),
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let resumed_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume(resumed_pane.as_str())
        .unwrap();
    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::HostAccess));
    service.set_subagent_scope_declaration(
        "agent-%1",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::Unrestricted,
            approval_provenance: mez_agent::SubagentApprovalProvenance::ExplicitUserApproval,
            current_directory: "/stale-parent".to_string(),
            read_scopes: vec!["/stale-parent".to_string()],
            write_scopes: vec!["/stale-parent".to_string()],
            permission_preset: Some(mez_agent::PermissionPreset::Auto),
        },
    );

    service
        .execute_agent_shell_resume_command(resumed_pane.as_str(), "/resume resumed-child")
        .unwrap();

    let resumed_agent_id = format!("agent-{resumed_pane}");
    assert_eq!(
        service.subagent_lineage(&resumed_agent_id),
        Some(&RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 2,
            display_name: "resumed child".to_string(),
            terminal: false,
        })
    );
    assert!(!service.subagent_lineage_has_live_parent_authority(&resumed_agent_id));
    assert!(!service.has_subagent_scope_declaration(&resumed_agent_id));
    assert!(
        service
            .active_subagent_write_scopes_for(&resumed_agent_id)
            .is_empty()
    );
    let policy = service.permission_policy_for_agent(&resumed_agent_id);
    assert_eq!(policy.preset, mez_agent::PermissionPreset::ReadOnly);
    assert_eq!(policy.approval_policy, ApprovalPolicy::Ask);
    let pane_count_before = service
        .session()
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .count();
    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: resumed_agent_id,
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: true,
                read_scopes: Vec::new(),
                read_scopes_defaulted: true,
                write_scopes: Vec::new(),
                write_scopes_defaulted: true,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "attempt terminal delegation".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("restored subagent lineage has no live parent authority"),
        "{error}"
    );
    assert_eq!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .count(),
        pane_count_before,
        "a restored structural-only parent must reject descendants before pane allocation"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies direct resume fails closed when a durable conversation is marked
/// as a subagent but lacks the lineage required to restore delegation limits.
#[test]
fn runtime_resume_rejects_subagent_without_durable_lineage() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-missing-lineage"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "missing-lineage".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-missing-lineage".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "resume delegated work".to_string(),
        })
        .unwrap();
    let metadata_path = transcript_store
        .presentation_path("missing-lineage")
        .unwrap()
        .with_file_name("metadata.json");
    fs::write(
        metadata_path,
        r#"{"version":2,"conversation_kind":"subagent"}"#,
    )
    .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"missing-lineage","method":"agent/shell/command","params":{"idempotency_key":"missing-lineage","input":"/resume missing-lineage"}}"#,
        &primary,
    );

    assert!(
        response.contains("cannot restore without durable lineage"),
        "{response}"
    );
}

/// Verifies malformed durable child lineage fails closed during direct resume
/// rather than restoring a conversation without its delegation constraints.
#[test]
fn runtime_resume_rejects_subagent_with_malformed_lineage() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-malformed-lineage"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "malformed-lineage".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-malformed-lineage".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "resume delegated work".to_string(),
        })
        .unwrap();
    let metadata_path = transcript_store
        .presentation_path("malformed-lineage")
        .unwrap()
        .with_file_name("metadata.json");
    fs::write(
        metadata_path,
        r#"{"version":2,"conversation_kind":"subagent","subagent_lineage":{"parent_agent_id":"","root_agent_id":"agent-%1","depth":1,"display_name":"child","terminal":false}}"#,
    )
    .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"malformed-lineage","method":"agent/shell/command","params":{"idempotency_key":"malformed-lineage","input":"/resume malformed-lineage"}}"#,
        &primary,
    );

    assert!(
        response.contains("persisted subagent lineage parent agent id is empty"),
        "{response}"
    );
}

/// Verifies direct resume rejects a terminal child whose durable catalog still
/// exposes descendant spawning, preserving terminal lineage semantics.
#[test]
fn runtime_resume_rejects_terminal_subagent_with_spawn_catalog() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-terminal-spawn"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "terminal-spawn".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-terminal-spawn".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "resume delegated work".to_string(),
        })
        .unwrap();
    let metadata_path = transcript_store
        .presentation_path("terminal-spawn")
        .unwrap()
        .with_file_name("metadata.json");
    fs::write(
        metadata_path,
        r#"{"version":2,"conversation_kind":"subagent","allowed_actions":{"actions":["Say","SpawnAgent"],"spawn_agent_sizing":{"sizes":[]}},"subagent_lineage":{"parent_agent_id":"agent-%1","root_agent_id":"agent-%1","depth":1,"display_name":"terminal child","terminal":true}}"#,
    )
    .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-spawn","method":"agent/shell/command","params":{"idempotency_key":"terminal-spawn","input":"/resume terminal-spawn"}}"#,
        &primary,
    );

    assert!(
        response.contains("terminal subagent conversation metadata cannot contain spawn_agent"),
        "{response}"
    );
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
        .name_session("named-empty", "Empty session", 10, None, false)
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
        .name_session("named-empty", "Pinned work", 10, None, false)
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
        .name_session("named-old", "Pinned work", 10, None, false)
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
    assert!(!detail.contains("## User entry 1"), "{detail}");
    assert!(
        detail.find("## Assistant entry 2").unwrap() < detail.find("## User entry 5").unwrap(),
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
        .name_session("empty-session", "Empty investigation", 10, None, false)
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
                lifecycle: crate::storage::transcript::SavedSessionLifecycleFilter::Active,
                include_subagents: false,
                search: None,
                anchor: None,
                limit: 20,
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

/// Verifies a late `/resume` failure restores both structural-only restored
/// lineage and live child scope authority without leaking target authority.
#[test]
fn runtime_resume_late_failure_restores_complete_prior_subagent_authority() {
    let mut service = test_runtime_service();
    let transcript_store =
        AgentTranscriptStore::new(temp_root("runtime-resume-authority-rollback"));
    for conversation_id in ["root-target", "child-target"] {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: 1,
                created_at_unix_seconds: 10,
                role: TranscriptRole::User,
                turn_id: format!("turn-{conversation_id}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: "resume target".to_string(),
            })
            .unwrap();
    }
    transcript_store
        .save_subagent_conversation_contract(
            "child-target",
            mez_agent::SubagentSessionLineage {
                parent_agent_id: "agent-%9".to_string(),
                root_agent_id: "agent-%1".to_string(),
                depth: 2,
                display_name: "target child".to_string(),
                terminal: false,
            },
            mez_agent::AllowedActionSet::say_only(),
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let checkpoint_before_failure = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();

    let agent_id = "agent-%1";
    let structural = RuntimeSubagentLineage {
        parent_agent_id: "agent-%8".to_string(),
        root_agent_id: "agent-%1".to_string(),
        depth: 2,
        display_name: "structural prior".to_string(),
        terminal: false,
    };
    service.set_restored_subagent_lineage(agent_id, structural.clone());
    service.fail_next_agent_resume_after_authority_restore_for_tests();
    let error = service
        .execute_agent_shell_resume_command("%1", "/resume root-target")
        .unwrap_err();
    assert!(
        error.message().contains("post-authority restoration"),
        "{error}"
    );
    assert_eq!(service.subagent_lineage(agent_id), Some(&structural));
    assert!(!service.subagent_lineage_has_live_parent_authority(agent_id));
    assert!(!service.has_subagent_scope_declaration(agent_id));
    assert!(
        service
            .active_subagent_write_scopes_for(agent_id)
            .is_empty()
    );
    assert_eq!(
        transcript_store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap(),
        checkpoint_before_failure,
        "late resume failure must not leave the target pane binding checkpointed"
    );

    let live = RuntimeSubagentLineage {
        parent_agent_id: "agent-%7".to_string(),
        root_agent_id: "agent-%1".to_string(),
        depth: 1,
        display_name: "live prior".to_string(),
        terminal: false,
    };
    let scope = mez_agent::SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::OwnedWrite,
        approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["/repo".to_string()],
        write_scopes: vec!["/repo/src".to_string()],
        permission_preset: Some(mez_agent::PermissionPreset::Auto),
    };
    service.set_subagent_lineage(agent_id, live.clone());
    service.set_subagent_scope_declaration(agent_id, scope.clone());
    service
        .register_subagent_write_scopes_for_tests(
            agent_id,
            CooperationMode::OwnedWrite,
            &scope.write_scopes,
            None,
        )
        .unwrap();
    let scopes_before = service.active_subagent_write_scopes_for(agent_id);
    let child_agent_id = "agent-%2";
    service.set_subagent_lineage(
        child_agent_id,
        RuntimeSubagentLineage {
            parent_agent_id: agent_id.to_string(),
            root_agent_id: agent_id.to_string(),
            depth: 2,
            display_name: "live prior child".to_string(),
            terminal: false,
        },
    );
    let child_turn = mez_agent::AgentTurnRecord {
        turn_id: "resume-rollback-fenced-child".to_string(),
        conversation_id: service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone(),
        agent_id: child_agent_id.to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: Some("resume-rollback-parent".to_string()),
        state: AgentTurnState::Queued,
        cooperation_mode: None,
        initial_capability: None,
    };
    service
        .agent_turn_ledger_mut()
        .queue_turn(child_turn.clone())
        .unwrap();
    {
        let overrides = service.integration.model_profile_overrides_mut();
        overrides
            .agent_profiles
            .insert(agent_id.to_string(), "prior-own-agent-profile".to_string());
        overrides.subagent_profiles.insert(
            agent_id.to_string(),
            "prior-own-subagent-profile".to_string(),
        );
        overrides.agent_profiles.insert(
            child_agent_id.to_string(),
            "prior-agent-profile".to_string(),
        );
        overrides.subagent_profiles.insert(
            child_agent_id.to_string(),
            "prior-subagent-profile".to_string(),
        );
    }
    service.fail_next_agent_resume_after_authority_restore_for_tests();
    let error = service
        .execute_agent_shell_resume_command("%1", "/resume child-target")
        .unwrap_err();
    assert!(
        error.message().contains("post-authority restoration"),
        "{error}"
    );
    assert_eq!(service.subagent_lineage(agent_id), Some(&live));
    assert!(service.subagent_lineage_has_live_parent_authority(agent_id));
    assert_eq!(service.subagent_scope_declaration(agent_id), Some(scope));
    assert_eq!(
        service.active_subagent_write_scopes_for(agent_id),
        scopes_before
    );
    assert!(service.subagent_lineage_has_live_parent_authority(child_agent_id));
    assert!(!service.subagent_descendant_is_fenced(child_agent_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Queued),
        "injected resume failure must restore the pre-fence descendant turn"
    );
    let overrides = service.integration.model_profile_overrides();
    assert_eq!(
        overrides.agent_profiles.get(agent_id),
        Some(&"prior-own-agent-profile".to_string())
    );
    assert_eq!(
        overrides.subagent_profiles.get(agent_id),
        Some(&"prior-own-subagent-profile".to_string())
    );
    assert_eq!(
        overrides.agent_profiles.get(child_agent_id),
        Some(&"prior-agent-profile".to_string())
    );
    assert_eq!(
        overrides.subagent_profiles.get(child_agent_id),
        Some(&"prior-subagent-profile".to_string())
    );
}

/// Verifies replacing a profile-overridden child pane clears its own agent and
/// subagent profile selectors for both root and different-child targets.
///
/// Pane-local overrides belong to the prior conversation authority just like
/// descendant overrides. A replacement must therefore remove the pane's own
/// stable agent-id entries, while a later target may establish fresh policy.
#[test]
fn runtime_resume_replacement_clears_overrides_for_own_child_agent_id() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-own-overrides"));
    for conversation_id in ["root-target", "child-target"] {
        transcript_store
            .append(&TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: 1,
                created_at_unix_seconds: 10,
                role: TranscriptRole::User,
                turn_id: format!("turn-{conversation_id}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: "resume target".to_string(),
            })
            .unwrap();
    }
    transcript_store
        .save_subagent_conversation_contract(
            "child-target",
            mez_agent::SubagentSessionLineage {
                parent_agent_id: "agent-%9".to_string(),
                root_agent_id: "agent-%1".to_string(),
                depth: 1,
                display_name: "replacement child".to_string(),
                terminal: false,
            },
            mez_agent::AllowedActionSet::say_only(),
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_restored_subagent_lineage(
        "agent-%1".to_string(),
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%9".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "prior child".to_string(),
            terminal: false,
        },
    );

    for target in ["root-target", "child-target"] {
        let overrides = service.integration.model_profile_overrides_mut();
        overrides
            .agent_profiles
            .insert("agent-%1".to_string(), "prior-agent-profile".to_string());
        overrides
            .subagent_profiles
            .insert("agent-%1".to_string(), "prior-subagent-profile".to_string());

        service
            .execute_agent_shell_resume_command("%1", &format!("/resume {target}"))
            .unwrap();

        let overrides = service.integration.model_profile_overrides();
        assert!(
            !overrides.agent_profiles.contains_key("agent-%1"),
            "{target} replacement must clear the pane agent profile"
        );
        assert!(
            !overrides.subagent_profiles.contains_key("agent-%1"),
            "{target} replacement must clear the pane subagent profile"
        );
    }
}

/// Verifies resuming a pane's already-live conversation preserves detached
/// child authority, result delivery, and delegation capacity relationships.
///
/// A fence is only valid when the pane replaces its conversation. Rebinding A
/// to A must leave its live descendants attached to that same authority.
#[test]
fn runtime_resume_current_conversation_preserves_detached_child_authority() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-current-child"));
    service.set_agent_transcript_store(transcript_store);
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_a = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service.configure_subagent_policy(1, 4, 2, 2, SubagentWaitPolicy::Detach);
    let parent = service
        .start_agent_prompt_turn("%1", "delegate from conversation A")
        .unwrap();
    service.remove_pending_agent_provider_task(&parent.turn_id);
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawned = service
        .execute_spawn_action_for_turn(
            &parent_turn,
            &runtime_spawn_agent_action("spawn-current", "finish conversation A work"),
        )
        .unwrap();
    let child_agent_id = spawned
        .structured_content_json
        .as_deref()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .and_then(|content| {
            content["spawn"]["agent"]["id"]
                .as_str()
                .map(ToOwned::to_owned)
        })
        .expect("detached spawn should report its child agent");
    let child_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == child_agent_id)
        .cloned()
        .expect("detached child turn should remain live");
    service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Completed,
            "detached child parent settled before same-conversation resume",
        )
        .unwrap();
    let direct_children_before = service.active_direct_subagent_count_for("agent-%1");
    {
        let overrides = service.integration.model_profile_overrides_mut();
        overrides
            .agent_profiles
            .insert("agent-%1".to_string(), "same-own-agent-profile".to_string());
        overrides.subagent_profiles.insert(
            "agent-%1".to_string(),
            "same-own-subagent-profile".to_string(),
        );
        overrides
            .agent_profiles
            .insert(child_agent_id.clone(), "same-agent-profile".to_string());
        overrides
            .subagent_profiles
            .insert(child_agent_id.clone(), "same-subagent-profile".to_string());
    }

    service
        .execute_agent_shell_resume_command("%1", &format!("/resume {conversation_a}"))
        .unwrap();

    assert!(!service.subagent_descendant_is_fenced(&child_agent_id));
    assert!(service.subagent_lineage_has_live_parent_authority(&child_agent_id));
    assert_eq!(
        service.active_direct_subagent_count_for("agent-%1"),
        direct_children_before,
        "same-conversation resume must not detach a live child from its parent capacity"
    );
    let overrides = service.integration.model_profile_overrides();
    assert_eq!(
        overrides.agent_profiles.get("agent-%1"),
        Some(&"same-own-agent-profile".to_string())
    );
    assert_eq!(
        overrides.subagent_profiles.get("agent-%1"),
        Some(&"same-own-subagent-profile".to_string())
    );
    assert_eq!(
        overrides.agent_profiles.get(&child_agent_id),
        Some(&"same-agent-profile".to_string())
    );
    assert_eq!(
        overrides.subagent_profiles.get(&child_agent_id),
        Some(&"same-subagent-profile".to_string())
    );
    service
        .integration
        .model_profile_overrides_mut()
        .agent_profiles
        .remove(&child_agent_id);
    service
        .integration
        .model_profile_overrides_mut()
        .subagent_profiles
        .remove(&child_agent_id);
    service
        .execute_spawn_action_for_turn(
            &child_turn,
            &runtime_spawn_agent_action("nested-after-same-resume", "child capacity remains live"),
        )
        .unwrap();
    assert_eq!(service.active_direct_subagent_count_for(&child_agent_id), 1);

    service
        .emit_subagent_task_result_for_state(&child_turn, AgentTurnState::Completed)
        .unwrap();
    let resumed_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join(" ");
    let resumed_text = resumed_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        resumed_text.contains("result delivered:"),
        "same-conversation resume lost live child result delivery: {resumed_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a live child that resumes its own current conversation retains its
/// live lineage, declared and registered scopes, and authority to delegate.
#[test]
fn runtime_resume_live_child_current_conversation_preserves_own_authority() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-live-child"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_a = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let lineage = RuntimeSubagentLineage {
        parent_agent_id: "agent-%9".to_string(),
        root_agent_id: "agent-%1".to_string(),
        depth: 1,
        display_name: "live child A".to_string(),
        terminal: false,
    };
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: conversation_a.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "turn-live-child-a".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "resume live child A".to_string(),
        })
        .unwrap();
    transcript_store
        .save_subagent_conversation_contract(
            &conversation_a,
            mez_agent::SubagentSessionLineage {
                parent_agent_id: lineage.parent_agent_id.clone(),
                root_agent_id: lineage.root_agent_id.clone(),
                depth: lineage.depth,
                display_name: lineage.display_name.clone(),
                terminal: lineage.terminal,
            },
            mez_agent::AllowedActionSet::from_actions([
                mez_agent::AllowedAction::Say,
                mez_agent::AllowedAction::SpawnAgent,
            ])
            .with_spawn_agent_sizing(mez_agent::SpawnAgentSizing { sizes: Vec::new() }),
        )
        .unwrap();
    let scope = mez_agent::SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::OwnedWrite,
        approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["/repo".to_string()],
        write_scopes: vec!["/repo/src".to_string()],
        permission_preset: Some(mez_agent::PermissionPreset::Auto),
    };
    service.set_subagent_lineage("agent-%1", lineage.clone());
    service.set_subagent_scope_declaration("agent-%1", scope.clone());
    service
        .register_subagent_write_scopes_for_tests(
            "agent-%1",
            CooperationMode::OwnedWrite,
            &scope.write_scopes,
            None,
        )
        .unwrap();
    let scopes_before = service.active_subagent_write_scopes_for("agent-%1");

    service
        .execute_agent_shell_resume_command("%1", &format!("/resume {conversation_a}"))
        .unwrap();

    assert_eq!(service.subagent_lineage("agent-%1"), Some(&lineage));
    assert!(service.subagent_lineage_has_live_parent_authority("agent-%1"));
    assert_eq!(service.subagent_scope_declaration("agent-%1"), Some(scope));
    assert_eq!(
        service.active_subagent_write_scopes_for("agent-%1"),
        scopes_before
    );
    let turn = service
        .start_agent_prompt_turn("%1", "delegate from live child A")
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|candidate| candidate.turn_id == turn.turn_id)
        .cloned()
        .unwrap();
    service
        .execute_spawn_action_for_turn(
            &turn,
            &runtime_spawn_agent_action("live-child-descendant", "delegation remains live"),
        )
        .unwrap();
    assert_eq!(service.active_direct_subagent_count_for("agent-%1"), 1);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a successful parent `/resume` fences detached children from the
/// replaced conversation, so their prior authority and terminal bridge result
/// cannot cross into the newly bound conversation.
#[test]
fn runtime_resume_fences_detached_child_authority_and_result_delivery() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.permission_policy_mut().set_approval_bypass(true);
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-fenced-child"));
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: "conversation-b".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "turn-conversation-b".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "resume conversation B".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.configure_subagent_policy(1, 4, 2, 2, SubagentWaitPolicy::Detach);
    let parent = service
        .start_agent_prompt_turn("%1", "delegate from conversation A")
        .unwrap();
    service.remove_pending_agent_provider_task(&parent.turn_id);
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawned = service
        .execute_spawn_action_for_turn(
            &parent_turn,
            &runtime_spawn_agent_action("spawn-detached", "finish conversation A work"),
        )
        .unwrap();
    let child_agent_id = spawned
        .structured_content_json
        .as_deref()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .and_then(|content| {
            content["spawn"]["agent"]["id"]
                .as_str()
                .map(ToOwned::to_owned)
        })
        .expect("detached spawn should report its child agent");
    let child_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == child_agent_id)
        .cloned()
        .expect("detached child turn should remain live");
    let native_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "queue stale child native shell action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "exercise stale child native dispatch".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "fenced-child-native-shell".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Emit a stale child marker".to_string(),
                        command: "printf fenced-child-native-side-effect".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(1_000),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let native_execution = service
        .execute_agent_turn_with_provider(
            &child_turn.turn_id,
            &native_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(native_execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_native_shell_actions()
            .iter()
            .any(|identity| {
                identity
                    == &(
                        child_turn.turn_id.clone(),
                        "fenced-child-native-shell".to_string(),
                    )
            })
    );
    service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Completed,
            "detached child parent settled before resume",
        )
        .unwrap();
    {
        let overrides = service.integration.model_profile_overrides_mut();
        overrides
            .agent_profiles
            .insert(child_agent_id.clone(), "old-agent-profile".to_string());
        overrides
            .subagent_profiles
            .insert(child_agent_id.clone(), "old-subagent-profile".to_string());
    }

    service
        .execute_agent_shell_resume_command("%1", "/resume conversation-b")
        .unwrap();

    assert!(service.subagent_descendant_is_fenced(&child_agent_id));
    assert!(!service.subagent_lineage_has_live_parent_authority(&child_agent_id));
    assert!(
        service
            .subagent_scope_declaration(&child_agent_id)
            .is_none()
    );
    assert!(
        service
            .active_subagent_write_scopes_for(&child_agent_id)
            .is_empty()
    );
    assert_eq!(service.active_direct_subagent_count_for("agent-%1"), 0);
    let overrides = service.integration.model_profile_overrides();
    assert!(!overrides.agent_profiles.contains_key(&child_agent_id));
    assert!(!overrides.subagent_profiles.contains_key(&child_agent_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child_turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
    assert!(
        !service
            .native_shell_progress_turn_ids()
            .contains(&child_turn.turn_id)
    );
    assert!(
        service
            .claim_native_shell_action(&child_turn.turn_id, "fenced-child-native-shell")
            .unwrap()
            .is_none()
    );
    let replacement_turn = service
        .start_agent_prompt_turn("%1", "delegate from conversation B")
        .unwrap();
    service.remove_pending_agent_provider_task(&replacement_turn.turn_id);
    let replacement_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == replacement_turn.turn_id)
        .cloned()
        .expect("replacement parent turn should remain live");
    assert!(
        service
            .execute_spawn_action_for_turn(
                &replacement_turn,
                &runtime_spawn_agent_action("spawn-after-replacement", "begin conversation B work"),
            )
            .is_ok(),
        "a fenced child must not consume replacement conversation capacity"
    );
    assert!(
        service
            .execute_spawn_action_for_turn(
                &child_turn,
                &runtime_spawn_agent_action("nested-after-resume", "must be denied"),
            )
            .is_err()
    );

    service
        .emit_subagent_task_result_for_state(&child_turn, AgentTurnState::Completed)
        .unwrap();
    let resumed_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        resumed_text.contains("Conversation ID: conversation-b"),
        "{resumed_text}"
    );
    assert!(
        !resumed_text.contains("subagent task completed"),
        "fenced child result leaked into conversation B: {resumed_text}"
    );
    assert!(
        !resumed_text.contains("fenced-child-native-side-effect"),
        "fenced child native action reached a pane: {resumed_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies corrupt target objective metadata rejects `/resume` after the
/// target bind begins and restores the prior conversation and MMP identity.
///
/// Durable objective lookup is deliberately part of the resume transaction:
/// an unreadable target must not leave the pane bound to that target while its
/// peer-discovery identity still advertises the prior conversation's value.
#[test]
fn runtime_resume_objective_metadata_failure_restores_prior_binding_and_identity() {
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
                working_directory: Some(
                    temp_root("runtime-resume-target-directory")
                        .display()
                        .to_string(),
                ),
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
                allowed_actions: None,
            }],
        )
        .unwrap();
    let metadata_path = transcript_store
        .presentation_path("resume-target")
        .unwrap()
        .with_file_name("metadata.json");
    fs::write(metadata_path, b"not valid metadata\n").unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let prior_conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .save_user_objective(&prior_conversation, Some("Keep prior objective A"))
        .unwrap();
    service
        .sync_runtime_agent_objective_for_conversation("%1", &prior_conversation)
        .unwrap();
    let agent_id = mez_core::ids::AgentId::opaque("agent-%1".to_string()).unwrap();
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
    let prior_directory = temp_root("runtime-resume-prior-directory");
    service.set_pane_current_working_directory("%1", prior_directory.clone());

    let failed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-failure","method":"agent/shell/command","params":{"idempotency_key":"resume-failure","input":"/resume resume-target"}}"#,
        &primary,
    );
    assert!(failed.contains("error"), "{failed}");
    assert!(
        failed.contains("conversation metadata decode failed"),
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
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Keep prior objective A")
    );
    assert_eq!(
        service.persistence.pane_transcript_refs("%1"),
        transcript_refs_before
    );
    assert_eq!(
        service.pane_current_working_directory("%1").as_deref(),
        Some(prior_directory.as_path()),
        "corrupt objective metadata must fail before resume changes the pane directory"
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
    let prior_actions = mez_agent::AllowedActionSet::from_actions([
        mez_agent::AllowedAction::Say,
        mez_agent::AllowedAction::ShellCommand,
    ]);
    service.set_agent_enabled_actions(prior_actions.clone());
    service
        .capture_agent_session_allowed_actions_for_pane("%1")
        .unwrap();
    service.set_agent_enabled_actions(mez_agent::AllowedActionSet::say_only());
    service.set_pane_current_working_directory("%1", cwd.clone());

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-list","method":"agent/shell/command","params":{"idempotency_key":"resume-list","input":"/resume"}}"#,
        &primary,
    );
    assert!(picker.contains("`saved`"), "{picker}");
    assert!(picker.contains("`latest`"), "{picker}");
    let saved_row = picker
        .lines()
        .find(|line| line.contains("`saved`"))
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
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.allowed_actions.as_ref()),
        Some(&mez_agent::AllowedActionSet::say_only())
    );
    assert_ne!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.allowed_actions.as_ref()),
        Some(&prior_actions)
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
    let request = crate::integrations::agent::context::assemble_model_request(
        &profile,
        mez_agent::ProviderApiCompatibility::default_for_kind(&profile.provider)
            .expect("runtime test profile must use a known provider API"),
        &turn,
        &context,
    )
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
