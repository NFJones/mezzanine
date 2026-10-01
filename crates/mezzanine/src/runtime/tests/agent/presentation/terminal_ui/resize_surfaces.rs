//! Durable resize reconstruction and isolation of hidden shell surfaces.
//!
//! Semantic replay preserves durable order at new geometry, while hidden agent
//! sessions must not replace the process-owned shell screen.

use super::*;

/// Verifies a geometry-aware rebuild preserves an earlier legacy snapshot
/// before replaying a later semantic entry at the destination geometry.
#[test]
fn runtime_agent_resize_keeps_legacy_snapshots_ordered_with_semantic_entries() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-mixed-presentation-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
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
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["status".to_string()],
            display_lines: vec!["agent: legacy snapshot".to_string()],
            copy_lines: vec!["agent: legacy snapshot".to_string()],
            ansi_text: None,
            source_text: None,
            source_content_type: None,
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id,
            sequence: 2,
            created_at_unix_seconds: 2,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale cached projection".to_string()],
            copy_lines: vec!["stale cached projection".to_string()],
            ansi_text: None,
            source_text: Some("# Semantic entry\n\nreflows at destination width".to_string()),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let compact = replayed
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(compact.contains("agentlegacysnapshot"), "{replayed}");
    assert!(
        compact.contains("Semanticentryreflowsatdestinationwidth"),
        "{replayed}"
    );
    assert!(
        compact.find("agentlegacysnapshot").unwrap()
            < compact
                .find("Semanticentryreflowsatdestinationwidth")
                .unwrap(),
        "{replayed}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a live width change rebuilds a source-backed agent screen instead
/// of reflowing its stale cached terminal rows. This keeps Markdown rendering
/// semantic across pane geometry changes while preserving legacy resize
/// behavior for panes that do not retain presentation source.
#[test]
fn runtime_agent_resize_rebuilds_source_backed_presentation_at_new_width() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
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
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id,
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale cached projection".to_string()],
            copy_lines: vec!["stale cached projection".to_string()],
            ansi_text: None,
            source_text: Some(
                "# Rebuilt heading\n\n- source layout changes with width".to_string(),
            ),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 12).unwrap())
        .unwrap();

    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("width change should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should rebuild at the resized width");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );

    let rebuilt = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(rebuilt.contains("Rebuiltheading"), "{rebuilt}");
    assert!(
        rebuilt.contains("sourcelayoutchangeswithwidth"),
        "{rebuilt}"
    );
    assert!(!rebuilt.contains("stalecachedprojection"), "{rebuilt}");
    let rebuilt_size = service.agent_pane_screen("%1").unwrap().size();
    assert!(
        !service
            .rebuild_agent_presentation_after_resize("%1", rebuilt_size)
            .unwrap(),
        "the installed projection should bypass repeated semantic replay"
    );
    assert_eq!(
        transcript_store
            .inspect_presentation(
                service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .as_str()
            )
            .unwrap()
            .len(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies resizing a pane after its agent session is hidden preserves the
/// shell-owned screen instead of replaying retained agent presentation.
///
/// Hidden sessions retain durable transcript records for a later resume, but
/// their pane screen belongs to the shell. A width resize must therefore use
/// ordinary terminal resizing without replacing the shell prompt.
#[test]
fn runtime_agent_resize_does_not_replay_hidden_session_over_shell_prompt() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-hidden-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
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
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale agent transcript".to_string()],
            copy_lines: vec!["stale agent transcript".to_string()],
            ansi_text: None,
            source_text: Some("# Retained agent source".to_string()),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let mut shell_screen = TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap();
    shell_screen.feed(b"distinct-shell$ ");
    service.set_pane_screen("%1", shell_screen);

    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 12).unwrap())
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("distinct-shell$"), "{pane_text}");
    assert!(!pane_text.contains("Retained agent source"), "{pane_text}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().visibility,
        AgentShellVisibility::Hidden
    );
    assert_eq!(
        transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .len(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a row-only terminal resize updates a retained hidden agent screen
/// without replacing either surface or requiring source-backed width replay.
#[test]
fn runtime_hidden_agent_screen_resizes_when_only_rows_change() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut agent_screen = TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap();
    agent_screen.feed(b"retained-agent-view");
    service.set_agent_pane_screen("%1", &conversation_id, agent_screen);
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let mut process_screen = TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap();
    process_screen.feed(b"retained-process-view");
    service.set_process_pane_screen("%1", process_screen);

    service
        .resize_attached_primary_terminal(&primary, Size::new(28, 16).unwrap())
        .unwrap();

    let window = service.session().active_window().unwrap();
    let expected_process_size = service.pane_presentation_size_for(window, "%1").unwrap();
    let expected_agent_size = service.pane_process_size_for(window, "%1").unwrap();
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        expected_process_size
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().size(),
        expected_agent_size
    );
    assert!(
        service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("retained-process-view")
    );
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("retained-agent-view")
    );
    service.terminate_all_pane_processes().unwrap();
}
