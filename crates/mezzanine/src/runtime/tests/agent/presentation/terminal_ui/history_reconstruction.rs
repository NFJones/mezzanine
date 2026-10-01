//! Complete durable history reconstruction and user-cleared viewport retention.
//!
//! Replay must retain typed styles and all history within the terminal budget,
//! but must not resurrect a viewport that the user explicitly cleared.

use super::*;

/// Verifies user-visible status rows persist typed source and replay through
/// their original presentation style after a geometry-aware rebuild.
#[test]
fn runtime_agent_status_presentation_persists_typed_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-status-source"));
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

    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: restoring durable status")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("styled-lines+json")),
        "{entries:?}"
    );

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
    let replayed_compact = replayed
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("agentrestoringdurablestatus"),
        "{replayed}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies source-backed reconstruction is bounded only by terminal history,
/// not by an arbitrary number of durable presentation entries.
#[test]
fn runtime_agent_resize_reconstructs_more_than_two_hundred_presentation_entries() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-complete-reconstruction"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    for sequence in 1..=205 {
        transcript_store
            .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
                conversation_id: conversation_id.clone(),
                sequence,
                created_at_unix_seconds: sequence,
                pane_id: "%1".to_string(),
                turn_id: None,
                terminal_width: 28,
                style_names: vec!["assistant".to_string()],
                display_lines: vec![format!("entry-{sequence:03}")],
                copy_lines: vec![format!("entry-{sequence:03}")],
                ansi_text: None,
                source_text: Some(format!("entry-{sequence:03}")),
                source_content_type: Some(
                    mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                ),
            })
            .unwrap();
    }
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 500).unwrap(),
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
    assert!(replayed.contains("entry-001"), "{replayed}");
    assert!(replayed.contains("entry-205"), "{replayed}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a source-backed transcript is not replayed into a viewport
/// cleared by the user. Resizing after Ctrl+L must retain the blank live pane
/// while preserving the prior agent output in scrollback.
#[test]
fn runtime_agent_presentation_resize_preserves_cleared_viewport() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-cleared-viewport"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "preserve this cleared agent viewport",
            mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
        .unwrap();

    service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .clear_visible_into_history();

    assert!(
        !service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let screen = service.agent_pane_screen("%1").unwrap();
    assert!(
        screen
            .visible_lines()
            .iter()
            .all(|line| line.trim().is_empty()),
        "{:?}",
        screen.visible_lines()
    );
    let history = screen
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        history.contains("preservethisclearedagentviewport"),
        "{history}"
    );
    service.terminate_all_pane_processes().unwrap();
}
