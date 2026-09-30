//! Source-order barriers before field closure or authoritative validation.
//!
//! Receiving a later field or action cannot expose it before an earlier source
//! is closed, projected, and accepted at the appropriate ownership boundary.

use super::*;

/// An open rationale must keep even the first action off the pane until its
/// source closes; neither a started label nor a worker projection may bypass it.
#[test]
fn runtime_streaming_open_rationale_holds_first_say() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
        .unwrap();
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
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "working through the rationale".to_string(),
        },
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "later answer".to_string(),
        },
    ] {
        service
            .ingest_provider_log(
                "%1",
                "turn-rationale-order",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-rationale-order")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        visible.contains("working through the rationale"),
        "{visible}"
    );
    assert!(!visible.contains("later answer"), "{visible}");
    service
        .ingest_provider_log(
            "%1",
            "turn-rationale-order",
            crate::runtime::RuntimeProviderLogInput::Progress(
                &mez_agent::StreamingSayEvent::RationaleTextComplete,
            ),
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-rationale-order")
            .unwrap()
            .is_some(),
        "closing rationale must schedule buffered projection"
    );
}

/// Closing a rationale does not allow an action label to precede its
/// still-outstanding rich projection installation.
#[test]
fn runtime_streaming_closed_rationale_holds_action_start_until_projection() {
    for command in [false, true] {
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
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: "rationale awaits rendering".to_string(),
            },
            mez_agent::StreamingSayEvent::RationaleTextComplete,
        ] {
            service
                .ingest_provider_log(
                    "%1",
                    "turn-rationale-ack",
                    crate::runtime::RuntimeProviderLogInput::Progress(&event),
                )
                .unwrap();
        }
        let before = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        let start = if command {
            mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 }
        } else {
            mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            }
        };
        service
            .ingest_provider_log(
                "%1",
                "turn-rationale-ack",
                crate::runtime::RuntimeProviderLogInput::Progress(&start),
            )
            .unwrap();
        assert_eq!(
            service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines(),
            before,
            "command={command}: action label preceded rationale projection"
        );
        let delta = if command {
            mez_agent::StreamingSayEvent::ShellCommandTextDelta {
                action_index: 0,
                text: "printf ready".to_string(),
            }
        } else {
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: "action ready".to_string(),
            }
        };
        service
            .ingest_provider_log(
                "%1",
                "turn-rationale-ack",
                crate::runtime::RuntimeProviderLogInput::Progress(&delta),
            )
            .unwrap();
        let work = service
            .take_agent_streaming_say_projection_work("%1", "turn-rationale-ack")
            .unwrap()
            .expect("closed rationale and action source should produce projection work");
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
        let lines = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        let rationale = lines.find("rationale awaits rendering").unwrap();
        let action = lines
            .find(if command {
                "printf ready"
            } else {
                "action ready"
            })
            .unwrap();
        assert!(rationale < action, "command={command}: {lines}");
        service
            .ingest_provider_log(
                "%1",
                "turn-rationale-ack",
                crate::runtime::RuntimeProviderLogInput::Progress(
                    &mez_agent::StreamingSayEvent::RationaleTextComplete,
                ),
            )
            .unwrap();
        assert!(
            service
                .take_agent_streaming_say_projection_work("%1", "turn-rationale-ack")
                .unwrap()
                .is_none(),
            "command={command}: replayed rationale closure dirtied the installed projection"
        );
    }
}

/// A later complete action must wait while the preceding action still has
/// unclosed source, even when the later action has a complete renderable body.
#[test]
fn runtime_streaming_later_complete_action_waits_for_earlier_source() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
        .unwrap();
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
            text: "first still open".to_string(),
        },
        mez_agent::StreamingSayEvent::Started {
            action_index: 1,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 1,
            text: "second is complete".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 1 },
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 1 },
    ] {
        let before_later_start = matches!(
            event,
            mez_agent::StreamingSayEvent::Started {
                action_index: 1,
                ..
            }
        )
        .then(|| service.agent_pane_screen("%1").unwrap().clone());
        service
            .ingest_provider_log(
                "%1",
                "turn-order",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
        if let Some(before) = before_later_start {
            assert_eq!(
                service.agent_pane_screen("%1").unwrap(),
                &before,
                "later Started must not write to the pane before projection"
            );
        }
    }
    let labels_before_projection = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .iter()
        .filter(|line| line.contains("mez> "))
        .count();
    assert!(
        labels_before_projection <= 1,
        "later label leaked before projection"
    );
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-order")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(visible.contains("first still open"), "{visible}");
    assert!(!visible.contains("second is complete"), "{visible}");
    service
        .ingest_provider_log(
            "%1",
            "turn-order",
            crate::runtime::RuntimeProviderLogInput::Progress(
                &mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
            ),
        )
        .unwrap();
    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", "turn-order")
            .unwrap()
            .is_none(),
        "a closed field is not a whole-action receipt"
    );
    service
        .ingest_provider_log(
            "%1",
            "turn-order",
            crate::runtime::RuntimeProviderLogInput::Progress(
                &mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
            ),
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-order")
        .unwrap()
        .expect("receipt may update projection ownership");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !visible.contains("second is complete"),
        "receipt alone cannot finalize action 0: {visible}"
    );
}

/// A received action with no preview still occupies its ordinal until the
/// validated batch determines whether it has a visible header or result.
#[test]
fn runtime_streaming_no_preview_action_holds_later_say_until_validation() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(52, 20).unwrap(), 200)
        .unwrap();
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
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::Started {
            action_index: 1,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 1,
            text: "later answer".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 1 },
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 1 },
    ] {
        service
            .ingest_provider_log(
                "%1",
                "turn-empty-order",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
    }
    if let Some(work) = service
        .take_agent_streaming_say_projection_work("%1", "turn-empty-order")
        .unwrap()
    {
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap();
    }
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!visible.contains("later answer"), "{visible}");
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-empty-order"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "later answer".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![
                    mez_agent::AgentAction {
                        id: "empty".to_string(),
                        payload: mez_agent::AgentActionPayload::Complete,
                    },
                    mez_agent::AgentAction {
                        id: "later".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text: "later answer".to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service
        .ingest_provider_log(
            "%1",
            "turn-empty-order",
            crate::runtime::RuntimeProviderLogInput::Validated(&execution),
        )
        .unwrap();
    service
        .ingest_provider_log(
            "%1",
            "turn-empty-order",
            crate::runtime::RuntimeProviderLogInput::Settled(&execution),
        )
        .unwrap();
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(visible.matches("mez> later answer").count(), 1, "{visible}");
}

/// A command label for a later action must wait behind an earlier source;
/// receiving its start event cannot append directly to the pane.
#[test]
fn runtime_streaming_later_command_start_waits_for_earlier_action() {
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
            text: "earlier source".to_string(),
        },
    ] {
        service
            .ingest_provider_log(
                "%1",
                "turn-command-label-order",
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
    }
    let before = service.agent_pane_screen("%1").unwrap().clone();
    service
        .ingest_provider_log(
            "%1",
            "turn-command-label-order",
            crate::runtime::RuntimeProviderLogInput::Progress(
                &mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 1 },
            ),
        )
        .unwrap();
    assert_eq!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        before.normal_content_lines(),
        "later command label appeared before earlier action settled"
    );
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-command-label-order")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("earlier source"), "{text}");
    assert!(!text.contains("$ "), "later command leaked: {text}");
}
