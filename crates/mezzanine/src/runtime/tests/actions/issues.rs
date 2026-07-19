//! Runtime regressions for issue action execution and provider continuation.

use super::*;

/// Verifies an issue capability grant and its causal evidence survive the
/// provider-worker boundary after a non-final issue query.
///
/// A provider invocation may negotiate the issues capability and execute one
/// action before returning control to the runtime actor. The following
/// provider invocation belongs to the same logical turn, so it must begin with
/// the accumulated action surface and chronological controller evidence rather
/// than restarting capability negotiation from the user prompt.
#[test]
fn runtime_issue_query_continuation_preserves_capability_state_and_chronology() {
    let mut service = test_runtime_service();
    let config_root = temp_root("runtime-issue-query-continuation");
    service.set_config_root(config_root);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-issue-continuation","input":"query the open issues and then fix them"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "querying open issues".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "inspect the issue backlog before making changes".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "issues-1".to_string(),
                    rationale: "load the open issues".to_string(),
                    payload: mez_agent::AgentActionPayload::IssueQuery {
                        kind: None,
                        state: Some("open".to_string()),
                        text: None,
                        limit: Some(100),
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
    assert_eq!(
        first_execution.request.interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityContinuation
    );
    assert!(
        first_execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"issue_query")
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: mez_agent::ModelResponse {
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
    assert_eq!(
        request.interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityContinuation
    );
    assert!(
        request
            .allowed_actions
            .action_type_names()
            .contains(&"issue_query")
    );
    let user_index = request
        .messages
        .iter()
        .position(|message| {
            message.source == ContextSourceKind::UserInstruction
                && message
                    .content
                    .contains("query the open issues and then fix them")
        })
        .expect("the original user instruction should remain in chronology");
    let capability_index = request
        .messages
        .iter()
        .position(|message| {
            message.source == ContextSourceKind::CommittedEvidence
                && message
                    .content
                    .starts_with("[controller capability decision]")
        })
        .expect("the issues capability decision should be durable evidence");
    let capability_message = &request.messages[capability_index];
    assert_eq!(
        capability_message.placement,
        mez_agent::ContextPlacement::ConversationAppend
    );
    assert_eq!(
        capability_message.role,
        mez_agent::ModelMessageRole::Context
    );
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|message| {
                message
                    .content
                    .starts_with("[controller capability decision]")
            })
            .count(),
        1
    );
    let assistant_index = request
        .messages
        .iter()
        .position(|message| {
            message.source == ContextSourceKind::TranscriptAssistant
                && message.content.contains("querying open issues")
        })
        .expect("the issue-query response should remain in chronology");
    let result_index = request
        .messages
        .iter()
        .position(|message| {
            message.source == ContextSourceKind::ActionResult
                && message
                    .content
                    .contains("[action_result issues-1 issue_query succeeded]")
        })
        .expect("the settled issue-query result should remain in chronology");
    assert!(
        user_index < capability_index
            && capability_index < assistant_index
            && assistant_index < result_index,
        "provider continuation messages were not causally ordered: {:?}",
        request
            .messages
            .iter()
            .map(|message| (&message.source, &message.content))
            .collect::<Vec<_>>()
    );
}
