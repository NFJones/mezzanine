//! Accepted-header settlement, exact source matching, and sibling retention.
//!
//! Provisional display is never execution evidence. Accepted or corrected
//! headers retain only authoritative sources without erasing matching progress.

use super::*;

/// Verifies a validated header does not vanish at provider completion while
/// its action is still pending, and never counts as an executed result.
#[tokio::test]
async fn runtime_streaming_matching_header_survives_reconciliation() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-header-settlement"));
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "search for matching header")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    let query = "matching header";
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "Search for the matching header".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::ActionHeader {
                action_index: 0,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                    query: query.to_string(),
                }),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::ActionHeader {
                action_index: 1,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                    query: "second matching header".to_string(),
                }),
            },
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        visible
            .join("\n")
            .contains("agent: web search: matching header")
    );
    let action = mez_agent::AgentAction {
        id: "action-1".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: query.to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    let second = mez_agent::AgentAction {
        id: "action-2".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: "second matching header".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Search for the matching header".to_string(),
                actions: vec![action.clone(), second.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: [&action, &second]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    let transition = service
        .apply_agent_provider_completed_transition(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();
    assert!(transition.applied);
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert_eq!(
        rows.iter()
            .filter(|line| line.contains("web search: matching header"))
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|line| line.contains("web search: second matching header"))
            .count(),
        1
    );
    assert_eq!(
        service
            .runtime_metrics()
            .agent_streaming_settlement_restorations,
        0
    );
    assert!(
        rows.iter()
            .any(|line| line.contains("thinking: Search for the matching header"))
    );
    assert!(
        visible
            .iter()
            .any(|line| line.contains("web search: matching header"))
    );
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| presentation_semantic_source(entry).as_deref()
                == Some("web search: matching header"))
            .count(),
        1,
        "{entries:?}"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| presentation_semantic_source(entry).as_deref()
                == Some("web search: second matching header"))
            .count(),
        1
    );
    use crate::storage::transcript::activity::{
        ACTIVITY_CONTENT_TYPE, ActivityComponentKind, ActivitySource,
    };
    let headers = entries
        .iter()
        .filter(|entry| entry.source_content_type.as_deref() == Some(ACTIVITY_CONTENT_TYPE))
        .map(|entry| ActivitySource::decode(entry.source_text.as_deref().unwrap()).unwrap())
        .filter(|source| source.kind == ActivityComponentKind::Header)
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[0].response_id, headers[1].response_id);
    assert_ne!(headers[0].action_id, headers[1].action_id);
    for (ordinal, header) in headers.iter().enumerate() {
        assert_eq!(header.action_ordinal, Some(ordinal));
        assert_eq!(header.status, "accepted");
        assert!(header.transaction.is_none());
    }
}

/// A changed accepted header must instead replace the unvalidated preview,
/// retaining only the authoritative action text.
#[test]
fn runtime_streaming_header_mismatch_restores_validated_source() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::ActionHeader {
                action_index: 0,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                    query: "unvalidated query".to_string(),
                }),
            },
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let action = mez_agent::AgentAction {
        id: "search".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: "validated query".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Use the validated query".to_string(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    assert!(
        service
            .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
            .unwrap()
    );
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(rows.contains("validated query"), "{rows}");
    assert!(!rows.contains("unvalidated query"), "{rows}");
}

/// Distinct raw queries with the same compact header must not inherit a
/// provisional row or durable source from the rejected provider preview.
#[test]
fn runtime_streaming_header_preview_collision_restores_baseline() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "check header collision")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    let prefix = "x".repeat(120);
    let streamed = format!("{prefix}a");
    let accepted = format!("{prefix}b");
    assert_ne!(streamed, accepted);
    let baseline = service.agent_pane_screen("%1").unwrap().clone();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            "turn-1",
            &mez_agent::StreamingSayEvent::ActionHeader {
                action_index: 0,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch { query: streamed }),
            },
        )
        .unwrap();
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let action = mez_agent::AgentAction {
        id: "search".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: accepted,
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Use validated query".to_string(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["search complete".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    assert!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &baseline);
}

/// A changed header must not erase a matching sibling progress row while the
/// authoritative header replaces only the rejected preview.
#[tokio::test]
async fn runtime_streaming_header_mismatch_retains_matching_progress() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("streaming-mismatch-sibling"));
    service.set_agent_transcript_store(store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "inspect both rows")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "Inspect both rows".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "keep sibling".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::ActionHeader {
            action_index: 1,
            header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                query: "wrong query".to_string(),
            }),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let say = mez_agent::AgentAction {
        id: "say".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "keep sibling".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let search = mez_agent::AgentAction {
        id: "search".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: "right query".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Inspect both rows".to_string(),
                actions: vec![say.clone(), search.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            mez_agent::ActionResult::succeeded(&turn, &say, Vec::new(), None),
            mez_agent::ActionResult::succeeded(&turn, &search, Vec::new(), None),
        ],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    let transition = service
        .apply_agent_provider_completed_transition(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();
    assert!(transition.applied);
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(rows.matches("keep sibling").count(), 1, "{rows}");
    assert!(!rows.contains("wrong query"), "{rows}");
    assert_eq!(rows.matches("right query").count(), 1, "{rows}");
    assert_eq!(
        service
            .runtime_metrics()
            .agent_streaming_settlement_restorations,
        0,
        "matching siblings must survive an atomic replacement, not baseline restoration"
    );
    let entries = store.inspect_presentation(&conversation_id).unwrap();
    for source in ["keep sibling", "web search: right query"] {
        assert_eq!(
            entries
                .iter()
                .filter(|entry| presentation_semantic_source(entry).as_deref() == Some(source))
                .count(),
            1,
            "{entries:?}"
        );
    }
    service
        .append_agent_status_text_to_terminal_buffer("%1", "later durable row")
        .unwrap();
    let appended = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(appended.matches("keep sibling").count(), 1, "{appended}");
    assert_eq!(appended.matches("right query").count(), 1, "{appended}");
    assert!(appended.contains("later durable row"), "{appended}");
}
