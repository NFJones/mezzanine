//! Accepted predecessor retention while message delivery remains provisional.
//!
//! Resize and rejection cannot erase settled siblings or promote trailing source
//! before message acceptance. The case fixture is private to this owner.

use super::*;

/// A validated say is permanent even while a later outbound message retains
/// response ownership for its separate delivery settlement.
#[test]
fn runtime_promoted_say_survives_pending_message_and_durable_append() {
    for resize in [false, true] {
        for accepted in [None, Some(false), Some(true)] {
            promoted_say_pending_message_case(resize, accepted);
        }
    }
}

/// Checks the pending-message baseline both with and without a resize replay.
fn promoted_say_pending_message_case(resize: bool, accepted: Option<bool>) {
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "promoted-say-pending-message",
    )));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
    );
    let turn = service
        .start_agent_prompt_turn("%1", "report then send")
        .unwrap();
    let answer = mez_agent::AgentAction {
        id: "answer".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "permanent sibling".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let message = mez_agent::AgentAction {
        id: "message".to_string(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: "agent-%2".to_string(),
            scope: None,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            payload: "pending payload".to_string(),
            correlation_id: None,
        },
    };
    let trailing = mez_agent::AgentAction {
        id: "trailing".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "unpromoted trailing say".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "permanent sibling".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::MessageStarted {
            action_index: 1,
            recipient: "agent-%2".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadDelta {
            action_index: 1,
            text: "pending payload".to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 1 },
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 1 },
        mez_agent::StreamingSayEvent::Started {
            action_index: 2,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 2,
            text: "unpromoted trailing say".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 2 },
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 2 },
    ] {
        service
            .ingest_provider_log(
                "%1",
                &turn.turn_id,
                crate::runtime::RuntimeProviderLogInput::Progress(&event),
            )
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![answer, message, trailing],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert_eq!(
        service
            .reconcile_agent_streaming_say_completion("%1", &turn.turn_id, &execution)
            .unwrap(),
        std::collections::BTreeSet::from([0])
    );
    let reconciled = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !reconciled.contains("unpromoted trailing say"),
        "{reconciled}"
    );
    if resize {
        assert!(
            service
                .rebuild_agent_presentation_after_resize("%1", Size::new(72, 12).unwrap())
                .unwrap()
        );
        let resized = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(resized.matches("permanent sibling").count(), 1, "{resized}");
        assert!(!resized.contains("unpromoted trailing say"), "{resized}");
    }
    if let Some(accepted) = accepted {
        let mut settled = execution.clone();
        let batch = settled.response.action_batch.as_ref().unwrap();
        let ledger_turn = service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .clone();
        settled.action_results = vec![
            mez_agent::ActionResult::succeeded(&ledger_turn, &batch.actions[0], Vec::new(), None),
            if accepted {
                mez_agent::ActionResult::succeeded(
                    &ledger_turn,
                    &batch.actions[1],
                    Vec::new(),
                    None,
                )
            } else {
                mez_agent::ActionResult::failed(
                    &ledger_turn,
                    &batch.actions[1],
                    mez_agent::ActionStatus::Failed,
                    "recipient_unavailable",
                    "recipient unavailable",
                )
                .unwrap()
            },
            mez_agent::ActionResult::succeeded(&ledger_turn, &batch.actions[2], Vec::new(), None),
        ];
        settled.terminal_state = if accepted {
            AgentTurnState::Running
        } else {
            AgentTurnState::Failed
        };
        if accepted {
            service
                .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 1, &batch.actions[1])
                .unwrap();
        }
        service
            .finalize_settled_outbound_message_previews("%1", &turn.turn_id, &settled)
            .unwrap();
        service
            .present_deferred_agent_say_actions_to_terminal_buffer("%1", &settled)
            .unwrap();
        let settled_text = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(
            settled_text.matches("permanent sibling").count(),
            1,
            "{settled_text}"
        );
        assert_eq!(
            settled_text.contains("pending payload"),
            accepted,
            "{settled_text}"
        );
        assert_eq!(
            settled_text.matches("unpromoted trailing say").count(),
            usize::from(accepted),
            "{settled_text}"
        );
        if accepted {
            assert!(
                settled_text.find("pending payload").unwrap()
                    < settled_text.find("unpromoted trailing say").unwrap(),
                "{settled_text}"
            );
        }
    }
    service
        .append_agent_status_text_to_terminal_buffer("%1", "later durable status")
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(text.matches("permanent sibling").count(), 1, "{text}");
    assert_eq!(
        text.matches("unpromoted trailing say").count(),
        usize::from(accepted == Some(true)),
        "{text}"
    );
    assert!(text.contains("later durable status"), "{text}");
}
