//! Provisional header rendering parity and rollback without execution evidence.
//!
//! Safe summaries and discovery headers use ordinary display rules, but must
//! restore their baseline when no authoritative batch accepts their sources.

use super::*;

/// Verifies safe shell summaries and closed web-search headers render before
/// provider completion with the same rows and styling as static presentation.
///
/// These components remain provisional: authoritative completion restores the
/// baseline so an unvalidated header cannot imply that an action dispatched.
#[test]
fn runtime_streaming_summary_and_web_header_match_static_projection_and_restore() {
    let mut streaming = test_runtime_service();
    let mut static_render = test_runtime_service();
    for service in [&mut streaming, &mut static_render] {
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
            }])
            .unwrap();
        service
            .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            service,
            "%1",
            TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
        );
        service
            .append_agent_status_text_to_terminal_buffer("%1", "baseline")
            .unwrap();
    }
    let baseline = streaming.agent_pane_screen("%1").unwrap().clone();
    let summary = "Inspect current streaming previews";
    let query = "streaming-previews-with-an-unbroken-target-0123456789abcdefghij";
    let action = mez_agent::AgentAction {
        id: "search-streamed".to_string(),

        payload: mez_agent::AgentActionPayload::WebSearch {
            query: query.to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };

    for event in [
        mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
            action_index: 0,
            text: summary.to_string(),
        },
        mez_agent::StreamingSayEvent::ShellCommandSummaryTextComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::ActionHeader {
            action_index: 1,
            header: Box::new(mez_agent::StreamingActionHeader::WebSearch {
                query: query.to_string(),
            }),
        },
    ] {
        streaming
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        streaming
            .take_agent_streaming_say_projection_work("%1", "turn-1")
            .unwrap()
            .expect("safe provisional components should project before completion"),
    )
    .unwrap();
    assert!(
        streaming
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );

    let streamed_lines = streaming
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        streamed_lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 24),
        "{streamed_lines:?}"
    );

    static_render
        .append_agent_thinking_text_to_terminal_buffer("%1", summary)
        .unwrap();
    assert!(
        static_render
            .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
            .unwrap()
    );
    assert_eq!(
        streaming
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        static_render
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
    );
    assert_eq!(
        streaming
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines(),
        static_render
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines(),
    );

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
                rationale: String::new(),

                actions: vec![action],
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
        streaming
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    assert_eq!(streaming.agent_pane_screen("%1").unwrap(), &baseline);
}

/// Discovery and wait previews use the ordinary transient action-header path;
/// neither is durable or evidence that a peer response has arrived.
#[test]
fn runtime_streaming_discovery_and_wait_headers_restore_on_reconciliation() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "baseline")
        .unwrap();
    let baseline = service.agent_pane_screen("%1").unwrap().clone();
    for (index, payload) in [
        mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
        mez_agent::AgentActionPayload::Wait,
    ]
    .into_iter()
    .enumerate()
    {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer(
                "%1",
                "turn-1",
                &mez_agent::StreamingSayEvent::ActionHeader {
                    action_index: index,
                    header: Box::new(mez_agent::StreamingActionHeader::Action {
                        action: Box::new(mez_agent::AgentAction {
                            id: String::new(),
                            payload,
                        }),
                    }),
                },
            )
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
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        text.contains("list agents: type=subagent scope=project"),
        "{text}"
    );
    assert!(text.contains("wait: peer reply requested"), "{text}");
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
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
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &baseline);
}
