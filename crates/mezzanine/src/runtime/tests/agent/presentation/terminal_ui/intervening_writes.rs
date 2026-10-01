//! Intervening durable writes revoke delayed provisional-screen authority.
//!
//! This regression keeps rollback and worker acceptance fenced by the same
//! installed lineage, so a later status row cannot be erased.

use super::*;

/// Verifies an ordinary pane write revokes a streaming projection's authority
/// before a delayed worker result or rollback can replace that write.
///
/// This covers the actor-serialized form of the reported race: projection work
/// is captured, a status row is appended, and the delayed projection must be
/// rejected while later cleanup preserves the status row.
#[test]
fn runtime_streaming_say_preserves_intervening_pane_writes() {
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
    for event in [
        mez_agent::StreamingSayEvent::ResponseStarted { response_index: 0 },
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "**provisional**".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let delayed_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();

    service
        .append_agent_status_text_to_terminal_buffer("%1", "intervening status")
        .unwrap();
    assert!(
        !service
            .apply_agent_streaming_say_projection_result(delayed_projection)
            .unwrap()
    );
    assert!(
        !service
            .discard_agent_streaming_say_presentation("%1", Some("turn-1"))
            .unwrap()
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("intervening status"), "{text}");
    assert!(!text.contains("provisional"), "{text}");
}
