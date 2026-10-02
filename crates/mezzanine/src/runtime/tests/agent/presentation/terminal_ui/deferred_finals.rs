//! Final components and headers settle together after runtime-visible work.
//!
//! Visible final previews remain provisional until accepted runtime work
//! finishes; failure removes them without publishing durable output.

use super::*;

/// A matching progress say and accepted search header share one projected
/// screen; accepting the batch must not erase either visible component.
#[tokio::test]
async fn runtime_streaming_progress_and_header_keep_matching_rows() {
    for header_first in [false, true] {
        streaming_progress_and_header_case(header_first, mez_agent::SayStatus::Progress).await;
    }
}

/// A final say following an accepted header is already in settled display order.
#[tokio::test]
async fn runtime_streaming_header_then_final_say_keeps_matching_rows() {
    streaming_progress_and_header_case(true, mez_agent::SayStatus::Final).await;
}

/// A final say after pending work must not become durable before the runtime
/// result authorizes deferred presentation.
#[tokio::test]
async fn runtime_streaming_header_then_final_say_waits_for_runtime_settlement() {
    streaming_progress_and_header_case_with_outcome(
        true,
        mez_agent::SayStatus::Final,
        false,
        false,
        true,
    )
    .await;
    streaming_progress_and_header_case_with_outcome(
        true,
        mez_agent::SayStatus::Final,
        false,
        true,
        true,
    )
    .await;
    streaming_progress_and_header_case_with_outcome(
        true,
        mez_agent::SayStatus::Final,
        true,
        true,
        false,
    )
    .await;
    streaming_progress_and_header_case_with_finals(true, true, false).await;
    streaming_progress_and_header_case_with_finals(true, false, false).await;
    streaming_progress_and_header_case_with_finals(false, true, true).await;
}

/// Two final says after pending work remain provisional together and either
/// become durable on success or disappear together on failure.
async fn streaming_progress_and_header_case_with_finals(
    changed: bool,
    completed: bool,
    trailing: bool,
) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("streaming-multiple-finals"));
    service.set_agent_transcript_store(store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "inspect two finals")
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
    let search = mez_agent::AgentAction {
        id: "search".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: "accepted query".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    let finals = ["first final", "second final"]
        .into_iter()
        .enumerate()
        .map(|(index, text)| mez_agent::AgentAction {
            id: format!("final-{index}"),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                text: text.to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        })
        .collect::<Vec<_>>();
    let progress = mez_agent::AgentAction {
        id: "trailing-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "trailing progress".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "Inspect two finals".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::ActionHeader {
                action_index: 0,
                header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                    query: if changed {
                        "preview query"
                    } else {
                        "accepted query"
                    }
                    .to_string(),
                }),
            },
        )
        .unwrap();
    for (index, text) in ["first final", "second final"].into_iter().enumerate() {
        for event in [
            mez_agent::StreamingSayEvent::Started {
                action_index: index + 1,
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: index + 1,
                text: text.to_string(),
            },
            mez_agent::StreamingSayEvent::TextComplete {
                action_index: index + 1,
            },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
    }
    if trailing {
        for event in [
            mez_agent::StreamingSayEvent::Started {
                action_index: 3,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 3,
                text: "trailing progress".to_string(),
            },
            mez_agent::StreamingSayEvent::TextComplete { action_index: 3 },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
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
                rationale: "Inspect two finals".to_string(),
                actions: std::iter::once(search.clone())
                    .chain(finals.clone())
                    .chain(trailing.then_some(progress.clone()))
                    .collect(),
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: std::iter::once(mez_agent::ActionResult::running(
            &turn,
            &search,
            Vec::new(),
            None,
        ))
        .chain(
            finals
                .iter()
                .map(|action| mez_agent::ActionResult::succeeded(&turn, action, Vec::new(), None)),
        )
        .chain(
            trailing
                .then(|| mez_agent::ActionResult::succeeded(&turn, &progress, Vec::new(), None)),
        )
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
    let entries = store.inspect_presentation(&conversation_id).unwrap();
    for text in ["first final", "second final"] {
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        assert!(rows.iter().any(|row| row.contains(text)), "{rows:?}");
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some(text))
                .count(),
            0
        );
    }
    assert_eq!(
        service
            .settle_pending_final_say_preview("%1", &turn.turn_id, completed)
            .unwrap(),
        completed
    );
    let entries = store.inspect_presentation(&conversation_id).unwrap();
    for text in ["first final", "second final"] {
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some(text))
                .count(),
            usize::from(completed)
        );
        assert_eq!(
            service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .iter()
                .any(|row| row.contains(text)),
            completed
        );
    }
    if trailing && completed {
        let live = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines();
        let live_final = live
            .iter()
            .position(|row| row.contains("second final"))
            .unwrap();
        let live_progress = live
            .iter()
            .position(|row| row.contains("trailing progress"))
            .unwrap();
        let sources = entries
            .iter()
            .filter_map(|entry| entry.source_text.as_deref())
            .collect::<Vec<_>>();
        let durable_final = sources
            .iter()
            .position(|source| *source == "second final")
            .unwrap();
        let durable_progress = sources
            .iter()
            .position(|source| *source == "trailing progress")
            .unwrap();
        assert_eq!(
            live_final < live_progress,
            durable_final < durable_progress,
            "{live:?} {sources:?}"
        );
    }
}

/// A blocked header does not become execution evidence, but its accepted
/// rationale and progress sibling should not disappear during settlement.
#[tokio::test]
async fn runtime_streaming_blocked_header_keeps_matching_siblings() {
    streaming_progress_and_header_case_with_blocked(
        false,
        mez_agent::SayStatus::Progress,
        true,
        false,
    )
    .await;
    streaming_progress_and_header_case_with_blocked(
        false,
        mez_agent::SayStatus::Progress,
        true,
        true,
    )
    .await;
}

/// Exercises both action orders so persisted replay matches the live projection.
async fn streaming_progress_and_header_case(header_first: bool, status: mez_agent::SayStatus) {
    streaming_progress_and_header_case_with_outcome(header_first, status, false, false, false)
        .await;
}

/// Exercises the blocked variant without changing the successful replay oracle.
async fn streaming_progress_and_header_case_with_blocked(
    header_first: bool,
    status: mez_agent::SayStatus,
    blocked: bool,
    changed: bool,
) {
    streaming_progress_and_header_case_with_outcome(header_first, status, blocked, changed, false)
        .await;
}

/// Exercises the pending variant without treating a visible final say as durable.
async fn streaming_progress_and_header_case_with_outcome(
    header_first: bool,
    status: mez_agent::SayStatus,
    blocked: bool,
    changed: bool,
    pending: bool,
) {
    let say_index = usize::from(header_first);
    let header_index = usize::from(!header_first);
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("streaming-progress-header"));
    service.set_agent_transcript_store(store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "search with progress")
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
    let say = mez_agent::AgentAction {
        id: "progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status,
            text: "searching now".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let search = mez_agent::AgentAction {
        id: "search".to_string(),
        payload: mez_agent::AgentActionPayload::WebSearch {
            query: "matching query".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "Look up matching query".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::Started {
            action_index: say_index,
            status,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: say_index,
            text: "searching now".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete {
            action_index: say_index,
        },
        mez_agent::StreamingSayEvent::ActionHeader {
            action_index: header_index,
            header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                query: if changed {
                    "wrong query"
                } else {
                    "matching query"
                }
                .to_string(),
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
    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(visible.iter().any(|row| row.contains("searching now")));
    assert!(visible.iter().any(|row| row.contains(if changed {
        "web search: wrong query"
    } else {
        "web search: matching query"
    })));
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
                rationale: "Look up matching query".to_string(),
                actions: if header_first {
                    vec![search.clone(), say.clone()]
                } else {
                    vec![say.clone(), search.clone()]
                },
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            mez_agent::ActionResult::succeeded(
                &turn,
                &say,
                vec!["searching now".to_string()],
                None,
            ),
            if pending {
                mez_agent::ActionResult::running(&turn, &search, Vec::new(), None)
            } else if blocked {
                mez_agent::ActionResult::blocked(
                    &turn,
                    &search,
                    Vec::new(),
                    "{\"approval\":{}}".to_string(),
                )
            } else {
                mez_agent::ActionResult::succeeded(
                    &turn,
                    &search,
                    vec!["search complete".to_string()],
                    None,
                )
            },
        ],
        final_turn: !blocked && !pending,
        terminal_state: if pending {
            AgentTurnState::Running
        } else if blocked {
            AgentTurnState::Blocked
        } else {
            AgentTurnState::Completed
        },
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
            .filter(|row| row.contains("searching now"))
            .count(),
        usize::from(!blocked || status == mez_agent::SayStatus::Progress)
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("web search: matching query"))
            .count(),
        usize::from(!blocked)
    );
    if blocked {
        assert_eq!(
            service
                .runtime_metrics()
                .agent_streaming_settlement_restorations,
            0
        );
    }
    let entries = store.inspect_presentation(&conversation_id).unwrap();
    if pending {
        assert_eq!(status, mez_agent::SayStatus::Final);
        assert!(rows.iter().any(|row| row.contains("searching now")));
        assert!(
            !entries
                .iter()
                .any(|entry| entry.source_text.as_deref() == Some("searching now")),
            "pending final must not become durable before the action settles"
        );
        return;
    }
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some("searching now"))
            .count(),
        usize::from(!blocked || status == mez_agent::SayStatus::Progress)
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| presentation_semantic_source(entry).as_deref()
                == Some("web search: matching query"))
            .count(),
        usize::from(!blocked)
    );
    let ordered_sources = entries
        .iter()
        .filter_map(presentation_semantic_source)
        .filter(|source| {
            matches!(
                source.as_str(),
                "Look up matching query" | "searching now" | "web search: matching query"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ordered_sources,
        if blocked {
            if status == mez_agent::SayStatus::Progress {
                vec!["Look up matching query", "searching now"]
            } else {
                vec!["Look up matching query"]
            }
        } else if header_first {
            vec![
                "Look up matching query",
                "web search: matching query",
                "searching now",
            ]
        } else {
            vec![
                "Look up matching query",
                "searching now",
                "web search: matching query",
            ]
        }
    );
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let progress_row = replayed
        .iter()
        .position(|row| row.contains("searching now"));
    if blocked {
        assert_eq!(
            progress_row.is_some(),
            status == mez_agent::SayStatus::Progress
        );
        assert!(
            !replayed
                .iter()
                .any(|row| row.contains("web search: matching query"))
        );
    } else {
        let progress_row = progress_row.unwrap();
        let header_row = replayed
            .iter()
            .position(|row| row.contains("web search: matching query"))
            .unwrap();
        assert_eq!(header_row < progress_row, header_first, "{replayed:?}");
    }
}
