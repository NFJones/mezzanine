//! Runtime tests for agent presentation logging behavior.

use super::*;

/// Verifies progress `say` messages become a bounded current-turn context ledger
/// before provider continuation.
///
/// Progress `say` text is user-visible but easy for the model to paraphrase in a
/// later action batch. The runtime keeps the recent entries as turn-volatile
/// local context so the next provider request can suppress redundant updates
/// without moving that changing text into the cache-stable prefix.
#[test]
fn runtime_progress_say_context_ledger_reaches_provider_continuation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-progress-ledger","input":"fix the repeated progress updates"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "progress".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "record the first sequence point".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-progress".to_string(),
                    rationale: "tell the user the owner changed".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Progress,
                        text: "The redundant updates are coming from repeated progress says."
                            .to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    let ledger_block = context
        .blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::RuntimeHint
                && block.label == "current-turn progress say ledger"
        })
        .expect("progress say ledger should be active turn context");
    assert_eq!(
        ledger_block.cache_policy(),
        crate::agent::ContextCachePolicy::Ineligible
    );
    assert!(
        ledger_block
            .content
            .contains("already emitted during the current turn"),
        "{}",
        ledger_block.content
    );
    assert!(
        ledger_block
            .content
            .contains("progress_say: The redundant updates are coming"),
        "{}",
        ledger_block.content
    );

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();

    assert_eq!(executions.len(), 1);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::RuntimeHint
            && message
                .content
                .contains("[current-turn progress say ledger]")
            && message.content.contains("It is not a user request")
            && message
                .content
                .contains("progress_say: The redundant updates are coming")
    }));
    assert!(!service.agent_turn_contexts.contains_key("turn-1"));
}

/// Verifies runtime keeps repeated progress `say` updates visible during a turn.
///
/// Progress messages are user-visible sequence points, and repeated provider
/// updates should still render as ordinary progress output instead of being
/// silently transformed into a suppression marker.
#[test]
fn runtime_agent_keeps_redundant_progress_say_updates_visible() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-redundant-progress","input":"fix the selector"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_progress = "The selector bug is in the real resume pager path.";
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "progress".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "record the owner".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-progress-1".to_string(),
                    rationale: "tell the user the selector owner".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Progress,
                        text: first_progress.to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);

    let duplicate_progress = "The surviving selector bug is still in the real resume pager path.";
    let final_text = "The fix is complete.";
    let second_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: duplicate_progress.to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "say-progress-2".to_string(),
                        rationale: "repeat the selector owner".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Progress,
                            text: duplicate_progress.to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    crate::agent::AgentAction {
                        id: "say-final".to_string(),
                        rationale: "finish the reply".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: final_text.to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].terminal_state, AgentTurnState::Completed);

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains(first_progress), "{pane_text}");
    assert!(pane_text.contains(duplicate_progress), "{pane_text}");
    assert!(pane_text.contains(final_text), "{pane_text}");
    assert!(
        executions[0]
            .action_results
            .iter()
            .any(|result| result.action_id == "say-progress-2" && !result.is_error),
        "{:?}",
        executions[0].action_results
    );
    service.pane_processes_mut().terminate_all().unwrap();
}
