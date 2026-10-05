//! Receipt-driven steering placement without model chronology changes.

use super::*;

/// Retiring a shell suffix during a durable write must remove only that suffix,
/// leaving preceding durable output and one pending receipt at the live tail.
#[test]
fn runtime_pending_steering_preserves_durable_rows_when_shell_preview_retires() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.start_agent_prompt_turn("%1", "initial").unwrap();
    service
        .execute_agent_shell_control_command(&primary, "pending guidance")
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable predecessor")
        .unwrap();
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-1".into(),
        action_id: "shell".into(),
        marker: "marker".into(),
    };
    service
        .update_agent_shell_output_preview("%1", owner.clone(), 1, &["temporary shell tail".into()])
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner));
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable successor")
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("durable predecessor"), "{text}");
    assert!(!text.contains("temporary shell tail"), "{text}");
    assert_eq!(text.matches("pending guidance").count(), 1, "{text}");
    assert!(
        text.find("durable successor").unwrap() < text.find("pending guidance").unwrap(),
        "{text}"
    );
}

/// Durable resize reconstruction must install the pending receipt once at its
/// new geometry without incorporating its transient rows in provider baselines.
#[test]
fn runtime_pending_steering_resize_keeps_single_source_backed_tail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap(),
    );
    service.start_agent_prompt_turn("%1", "initial").unwrap();
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "pending-steering-resize",
    )));
    service
        .execute_agent_shell_control_command(&primary, "pending guidance")
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable output")
        .unwrap();
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(40, 24).unwrap())
            .unwrap()
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "after resize")
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(text.matches("pending guidance").count(), 1, "{text}");
    assert!(
        text.find("after resize").unwrap() < text.find("pending guidance").unwrap(),
        "{text}"
    );
}

/// Provider and shell updates retain one receipt-owned tail, and provisional
/// rollback cannot make it durable or erase independently owned shell output.
#[test]
fn runtime_pending_steering_composes_with_provider_and_shell_without_persistence() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap(),
    );
    let turn = service.start_agent_prompt_turn("%1", "initial").unwrap();
    let store = AgentTranscriptStore::new(temp_root("pending-steering-interleaving"));
    service.set_agent_transcript_store(store.clone());
    service
        .execute_agent_shell_control_command(&primary, "pending guidance")
        .unwrap();
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.into(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "provider output".into(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
        .unwrap()
        .unwrap();
    let result = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(result)
            .unwrap()
    );
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: turn.turn_id.clone(),
        action_id: "shell".into(),
        marker: "marker".into(),
    };
    service
        .update_agent_shell_output_preview("%1", owner.clone(), 1, &["shell output".into()])
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(text.matches("pending guidance").count(), 1, "{text}");
    assert!(
        text.find("provider output").unwrap() < text.find("pending guidance").unwrap(),
        "{text}"
    );
    assert!(
        text.find("shell output").unwrap() < text.find("pending guidance").unwrap(),
        "{text}"
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(40, 24).unwrap())
            .unwrap()
    );
    let resized = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(resized.matches("pending guidance").count(), 1, "{resized}");
    service
        .discard_agent_streaming_say_presentation("%1", Some(&turn.turn_id))
        .unwrap();
    service
        .update_agent_shell_output_preview("%1", owner, 2, &["new shell output".into()])
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(text.matches("pending guidance").count(), 1, "{text}");
    assert!(!text.contains("provider output"), "{text}");
    assert!(
        text.find("new shell output").unwrap() < text.find("pending guidance").unwrap(),
        "{text}"
    );
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        store
            .inspect_presentation(&conversation)
            .unwrap()
            .iter()
            .all(|entry| {
                !entry.display_lines.join("\n").contains("pending guidance")
                    && !entry
                        .source_text
                        .as_deref()
                        .unwrap_or_default()
                        .contains("pending guidance")
            })
    );
}

/// Steering accepted during an active turn stays once at the live log tail
/// after subsequent durable output; its display label never enters input.
#[test]
fn runtime_pending_steering_follows_durable_output_without_reordering_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap(),
    );
    let turn = service.start_agent_prompt_turn("%1", "initial").unwrap();
    service
        .execute_agent_shell_control_command(&primary, "pending guidance")
        .unwrap();
    let context = service.agent_turn_contexts()[&turn.turn_id].clone();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "later durable output")
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(text.matches("pending guidance").count(), 1, "{text}");
    assert!(text.contains("user> [pending] pending guidance"), "{text}");
    assert!(
        text.find("later durable output").unwrap() < text.find("pending guidance").unwrap(),
        "{text}"
    );
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], context);
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "pending guidance")
            .count(),
        1
    );
}
