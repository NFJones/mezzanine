//! Idempotent event replay must not append labels or dirty settled projections.
//!
//! Repeated starts and closures retain response-local source ownership and
//! cannot invalidate an already acknowledged worker projection.

use super::*;

/// Replayed command start events must not create a second provisional label
/// before the cumulative source projection replaces that mutable suffix.
#[test]
fn runtime_streaming_replayed_command_start_has_one_label() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
    );
    let start = mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 };
    for _ in 0..2 {
        service
            .ingest_provider_log(
                "%1",
                "turn-command-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&start),
            )
            .unwrap();
    }
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert_eq!(
        lines.iter().filter(|line| line.trim_end() == "│ $").count(),
        1,
        "{lines:?}"
    );
}

/// Replaying a say start for the same response ordinal must not append
/// another provisional assistant label before its source projection arrives.
#[test]
fn runtime_streaming_replayed_say_start_has_one_label() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
    );
    let start = mez_agent::StreamingSayEvent::Started {
        action_index: 0,
        status: mez_agent::SayStatus::Progress,
        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
    };
    for _ in 0..2 {
        service
            .ingest_provider_log(
                "%1",
                "turn-say-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&start),
            )
            .unwrap();
    }
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.trim_end() == "│ mez>")
            .count(),
        1,
        "{lines:?}"
    );
}

/// Replaying field closure after an installed projection cannot make an
/// unchanged response dirty or request another worker render.
#[test]
fn runtime_streaming_replayed_field_closure_keeps_projection_current() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "first source".to_string(),
        },
        mez_agent::StreamingSayEvent::Started {
            action_index: 1,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 1,
            text: "second source".to_string(),
        },
    ] {
        service
            .ingest_provider_log(
                "%1",
                "turn-closure-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-closure-replay")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let closure = mez_agent::StreamingSayEvent::TextComplete { action_index: 0 };
    service
        .ingest_provider_log(
            "%1",
            "turn-closure-replay",
            crate::runtime::RuntimeProviderLogInput::Progress(&closure),
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-closure-replay")
        .unwrap()
        .expect("first closure must acknowledge newly eligible source");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    service
        .ingest_provider_log(
            "%1",
            "turn-closure-replay",
            crate::runtime::RuntimeProviderLogInput::Progress(&closure),
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-closure-replay")
            .unwrap()
            .is_none(),
        "replayed closure cannot dirty the acknowledged projection"
    );
}

/// Replaying command-field closure must not invalidate the projection that
/// already acknowledged the first closure and its buffered successor.
#[test]
fn runtime_streaming_replayed_command_closure_keeps_projection_current() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 0,
            text: "printf first".to_string(),
        },
        mez_agent::StreamingSayEvent::Started {
            action_index: 1,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 1,
            text: "later source".to_string(),
        },
    ] {
        service
            .ingest_provider_log(
                "%1",
                "turn-command-closure-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-command-closure-replay")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let closure = mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 };
    service
        .ingest_provider_log(
            "%1",
            "turn-command-closure-replay",
            crate::runtime::RuntimeProviderLogInput::Progress(&closure),
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-command-closure-replay")
        .unwrap()
        .expect("first closure must release buffered successor");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    service
        .ingest_provider_log(
            "%1",
            "turn-command-closure-replay",
            crate::runtime::RuntimeProviderLogInput::Progress(&closure),
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-command-closure-replay")
            .unwrap()
            .is_none(),
        "replayed command closure cannot dirty an acknowledged projection"
    );
}

/// A repeated summary or message closure cannot invalidate a projection
/// after the first closure has released and rendered its successor.
#[test]
fn runtime_streaming_replayed_auxiliary_closure_keeps_projection_current() {
    for message in [false, true] {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
        );
        let start = if message {
            mez_agent::StreamingSayEvent::MessageStarted {
                action_index: 0,
                recipient: "agent-%2".to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            }
        } else {
            mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 }
        };
        let delta = if message {
            mez_agent::StreamingSayEvent::MessagePayloadDelta {
                action_index: 0,
                text: "sent payload".to_string(),
            }
        } else {
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
                action_index: 0,
                text: "command summary".to_string(),
            }
        };
        let closure = if message {
            mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 }
        } else {
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextComplete { action_index: 0 }
        };
        for event in [
            start,
            delta,
            mez_agent::StreamingSayEvent::Started {
                action_index: 1,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 1,
                text: "later source".to_string(),
            },
        ] {
            service
                .ingest_provider_log(
                    "%1",
                    "turn-aux-closure-replay",
                    crate::runtime::RuntimeProviderLogInput::Progress(&event),
                )
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-aux-closure-replay")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        service
            .ingest_provider_log(
                "%1",
                "turn-aux-closure-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&closure),
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-aux-closure-replay")
            .unwrap()
            .expect("first closure releases the later ordinal");
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        service
            .ingest_provider_log(
                "%1",
                "turn-aux-closure-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&closure),
            )
            .unwrap();
        assert!(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-aux-closure-replay")
                .unwrap()
                .is_none(),
            "message={message}: duplicate closure dirtied the installed projection"
        );
    }
}

/// Replaying a rationale or summary start must not invalidate an already
/// acknowledged source projection for the same response component.
#[test]
fn runtime_streaming_replayed_rationale_and_summary_starts_keep_projection_current() {
    for summary in [false, true] {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
        );
        let start = if summary {
            mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 }
        } else {
            mez_agent::StreamingSayEvent::RationaleStarted
        };
        service
            .ingest_provider_log(
                "%1",
                "turn-start-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&start),
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-start-replay")
            .unwrap()
            .unwrap();
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        service
            .ingest_provider_log(
                "%1",
                "turn-start-replay",
                crate::runtime::RuntimeProviderLogInput::Progress(&start),
            )
            .unwrap();
        assert!(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-start-replay")
                .unwrap()
                .is_none(),
            "summary={summary}: duplicate start dirtied an acknowledged projection"
        );
    }
}

/// Replaying a visible outbound-message start cannot invalidate the projection
/// already acknowledged for that same response action.
#[test]
fn runtime_streaming_replayed_message_start_keeps_projection_current() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(52, 20).unwrap(), 200).unwrap(),
    );
    let start = mez_agent::StreamingSayEvent::MessageStarted {
        action_index: 0,
        recipient: "agent-%2".to_string(),
        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
    };
    service
        .ingest_provider_log(
            "%1",
            "turn-message-start-replay",
            crate::runtime::RuntimeProviderLogInput::Progress(&start),
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-message-start-replay")
        .unwrap()
        .expect("message start should schedule projection");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    service
        .ingest_provider_log(
            "%1",
            "turn-message-start-replay",
            crate::runtime::RuntimeProviderLogInput::Progress(&start),
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-message-start-replay")
            .unwrap()
            .is_none(),
        "replayed message start dirtied the acknowledged projection"
    );
}
