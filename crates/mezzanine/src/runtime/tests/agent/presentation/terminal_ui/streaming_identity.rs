//! Streaming source-generation, screen-lineage, and response-ordinal fencing.
//!
//! Delayed candidates must never replace newer source or survive an ABA screen
//! replacement. Response-local ordinals cannot inherit a previous response.

use super::*;

/// Verifies a rich generation captured before newer source arrived
/// cannot replace the pane or expose rows from two streaming generations.
///
/// The renderer may finish after the actor has accepted another action. The
/// stale candidate must be rejected as a whole, leaving the newer cumulative
/// projection byte-for-byte unchanged.
#[test]
fn runtime_streaming_say_rejects_stale_projection_generation_atomically() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
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
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "**old generation**".to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        )
        .unwrap();
    let stale_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("completed source should produce projection work");
    let stale_projection = RuntimeSessionService::build_agent_streaming_say_projection(stale_work)
        .expect("private stale generation should still render completely");

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 1,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 1,
                text: "new literal generation".to_string(),
            },
        )
        .unwrap();
    let latest_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("newer cumulative source should produce projection work");
    let latest_projection =
        RuntimeSessionService::build_agent_streaming_say_projection(latest_work)
            .expect("newer cumulative generation should render completely");
    assert!(
        service
            .apply_agent_streaming_say_projection_result(latest_projection)
            .unwrap(),
        "the newest cumulative generation should install atomically"
    );
    let before_stale_install = service.agent_pane_screen("%1").unwrap().clone();

    assert!(
        !service
            .apply_agent_streaming_say_projection_result(stale_projection)
            .unwrap(),
        "a prior source generation must be rejected"
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap(),
        &before_stale_install,
        "rejecting stale work must not publish any partial candidate rows"
    );
    let text = before_stale_install.normal_content_lines().join("\n");
    assert!(text.contains("old generation"), "{text}");
    assert!(!text.contains("**old generation**"), "{text}");
    assert!(text.contains("new literal generation"), "{text}");
}

/// Verifies a same-content screen replacement still fences an old worker result.
///
/// Structural screen equality cannot distinguish this ABA transition because
/// the replacement retains identical visible content and history. Runtime-owned
/// lineage must reject the old result without inspecting or recording content.
#[test]
fn runtime_streaming_say_rejects_same_content_aba_projection_lineage() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap();
    for line in 0..256 {
        screen.feed(format!("retained history {line}\r\n").as_bytes());
    }
    set_agent_pane_screen_for_test(&mut service, "%1", screen);
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-aba",
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-aba",
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "**same-content ABA**".to_string(),
            },
        )
        .unwrap();
    let stale_work = service
        .take_agent_streaming_say_projection_work("%1", "turn-aba")
        .unwrap()
        .expect("streaming source should produce projection work");
    let stale_projection = RuntimeSessionService::build_agent_streaming_say_projection(stale_work)
        .expect("captured ABA generation should render completely");
    let original_lineage = service
        .agent_pane_screen_lineage("%1", &conversation_id)
        .unwrap();
    let same_content = service.agent_pane_screen("%1").unwrap().clone();

    service.set_agent_pane_screen("%1", conversation_id.clone(), same_content.clone());

    assert_ne!(
        service
            .agent_pane_screen_lineage("%1", &conversation_id)
            .unwrap(),
        original_lineage
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &same_content);
    assert!(
        !service
            .apply_agent_streaming_say_projection_result(stale_projection)
            .unwrap(),
        "an old worker result must not survive a same-content ABA replacement"
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &same_content);
    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_streaming_projection_results, 1);
    assert_eq!(metrics.agent_streaming_projection_installs, 0);
    assert_eq!(metrics.agent_streaming_projection_rejections, 1);
    assert_eq!(metrics.agent_streaming_projection_lineage_rejections, 1);
}

/// Verifies response-local MAAP action ordinals cannot append to source from a
/// preceding provider interaction in the same logical turn.
///
/// A capability continuation or repair starts its parser at action zero. The
/// ordered response barrier must retire the preceding provisional generation
/// before action zero from the new response is accepted.
#[test]
fn runtime_streaming_say_scopes_action_indices_to_provider_responses() {
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
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "first response".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let first_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(first_projection)
            .unwrap()
    );

    for event in [
        mez_agent::StreamingSayEvent::ResponseStarted { response_index: 1 },
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "second response".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let second_projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(second_projection)
            .unwrap()
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("second response"), "{text}");
    assert!(!text.contains("first responsesecond response"), "{text}");
}
