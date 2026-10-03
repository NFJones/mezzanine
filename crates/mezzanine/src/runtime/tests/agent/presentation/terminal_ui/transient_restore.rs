//! Resize and rollback restoration of exactly owned transient presentation.
//!
//! Durable sources rebuild the baseline, while response and shell metadata
//! survive only with their captured screen lineage. Stale snapshots cannot
//! regain authority over intervening output.

use super::*;

/// Verifies resize rebuilds durable source and then reprojects active transients.
///
/// Provider source and shell progress are not durable replay entries. A width
/// change must rebuild only the persisted baseline, retain owner and revision
/// metadata, regenerate provider presentation at the new geometry, and append
/// the active shell preview once after that provider projection.
#[test]
fn runtime_agent_resize_reprojects_provider_and_shell_preview() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-transient-resize"));
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable resize baseline")
        .unwrap();
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**provider resize source**".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-provider-resize",
                &event,
            )
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-provider-resize")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-shell-resize".to_string(),
        action_id: "shell-resize".to_string(),
        marker: "marker-resize".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            7,
            &["shell resize progress".to_string()],
        )
        .unwrap();

    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(28, 12).unwrap())
            .unwrap()
    );

    let resized = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(resized.contains("durable resize baseline"), "{resized}");
    let resized_compact = resized
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '│')
        .collect::<String>();
    assert!(
        resized_compact.contains("providerresizesource"),
        "{resized}"
    );
    assert!(resized.contains("shell resize progress"), "{resized}");
    assert_eq!(
        resized.matches("shell resize progress").count(),
        1,
        "{resized}"
    );
    let previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 1, "{previews:?}");
    assert_eq!(previews[0].0, owner);
    assert_eq!(previews[0].2, 7);
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert!(
        entries.iter().all(|entry| {
            entry.source_text.as_deref() != Some("provider resize source")
                && !entry
                    .display_lines
                    .iter()
                    .any(|line| line.contains("shell resize progress"))
        }),
        "{entries:?}"
    );
}

/// Verifies failed-transition snapshots restore transients only with exact lineage.
///
/// A matching rollback may restore its owner metadata. If an intervening pane
/// generation appears first, the same snapshot must not reattach stale provider
/// or shell projection ownership to that newer screen.
#[test]
fn runtime_agent_resume_snapshot_requires_exact_transient_lineage() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    service
        .presentation
        .seed_accepted_streaming_header_for_tests(
            "%1",
            "turn-snapshot",
            "search-snapshot",
            "web search: snapshot",
        );
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-snapshot".to_string(),
        action_id: "shell-snapshot".to_string(),
        marker: "marker-snapshot".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            3,
            &["snapshot preview".to_string()],
        )
        .unwrap();

    let matching = service.snapshot_agent_resume_presentation("%1");
    service.restore_agent_resume_presentation("%1", matching);
    let restored = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(restored.len(), 1, "{restored:?}");
    assert_eq!(restored[0].0, owner);
    assert_eq!(restored[0].2, 3);
    assert!(
        service
            .presentation
            .has_accepted_streaming_header_for_tests("%1", "turn-snapshot", "search-snapshot")
    );

    let stale = service.snapshot_agent_resume_presentation("%1");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut intervening = service.agent_pane_screen("%1").unwrap().clone();
    intervening.feed(b"\r\nintervening rollback row\r\n");
    service.set_agent_pane_screen("%1", conversation_id, intervening);
    service.restore_agent_resume_presentation("%1", stale);

    assert!(
        service
            .agent_shell_output_previews_for_tests("%1")
            .is_empty()
    );
    assert!(
        !service
            .presentation
            .has_accepted_streaming_header_for_tests("%1", "turn-snapshot", "search-snapshot")
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("intervening rollback row"), "{text}");
}
