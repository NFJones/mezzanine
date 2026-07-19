//! Runtime regressions for issue action execution and provider continuation.

use super::*;

/// Builds one non-final issue-query batch for freshness tests.
fn runtime_issue_query_batch(action_id: &str, refresh: bool) -> mez_agent::MaapBatch {
    mez_agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "Inspect the current open issue snapshot".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        actions: vec![mez_agent::AgentAction {
            id: action_id.to_string(),
            rationale: "Load open issues once for the current mutation state".to_string(),
            payload: mez_agent::AgentActionPayload::IssueQuery {
                kind: None,
                state: Some("open".to_string()),
                text: None,
                limit: Some(100),
                refresh,
            },
        }],
        final_turn: false,
    }
}

/// Builds one non-final issue-add batch that invalidates query freshness.
fn runtime_issue_add_batch(action_id: &str) -> mez_agent::MaapBatch {
    mez_agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "Record a newly discovered issue before refreshing the backlog".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        actions: vec![mez_agent::AgentAction {
            id: action_id.to_string(),
            rationale: "Mutate the issue store".to_string(),
            payload: mez_agent::AgentActionPayload::IssueAdd {
                kind: "task".to_string(),
                title: "Exercise query freshness invalidation".to_string(),
                body: None,
                notes: None,
                depends_on: Vec::new(),
            },
        }],
        final_turn: false,
    }
}

/// Builds one provider response around a supplied issue-action batch.
fn runtime_issue_response(batch: mez_agent::MaapBatch) -> mez_agent::ModelResponse {
    mez_agent::ModelResponse {
        provider: "runtime-batch".to_string(),
        model: "test".to_string(),
        raw_text: "issue action".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(batch),
        provider_transcript_events: Vec::new(),
    }
}

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
                        refresh: false,
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
            raw_text: "selected issue iss-42".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "Continue active issue iss-42 before inspecting its owner".to_string(),
                thought: Some(
                    "Active issue: iss-42; inspect its cited implementation and tests".to_string(),
                ),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "issues-selected".to_string(),
                    rationale: "Load only dependency evidence for active issue iss-42".to_string(),
                    payload: mez_agent::AgentActionPayload::IssueQuery {
                        kind: None,
                        state: Some("open".to_string()),
                        text: Some("iss-42".to_string()),
                        limit: Some(10),
                        refresh: false,
                    },
                }],
                final_turn: false,
            }),
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
    assert!(
        request.messages[assistant_index]
            .content
            .contains("rationale: inspect the issue backlog before making changes")
    );
    assert!(
        request.messages[assistant_index]
            .content
            .contains("action rationale issues-1 (issue_query): load the open issues")
    );
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

    let third_provider = RuntimeRecordingProvider {
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
    let third_executions = service
        .poll_agent_provider_tasks_with_provider(&third_provider, 1)
        .unwrap();
    assert_eq!(third_executions.len(), 1);
    let selected_request = third_provider.last_request.borrow().clone().unwrap();
    let selected_assistant = selected_request
        .messages
        .iter()
        .find(|message| {
            message.source == ContextSourceKind::TranscriptAssistant
                && message.content.contains("Active issue: iss-42")
        })
        .expect("the selected issue decision should survive the action boundary");
    assert!(
        selected_assistant
            .content
            .contains("rationale: Continue active issue iss-42 before inspecting its owner")
    );
    assert!(selected_assistant.content.contains(
        "action rationale issues-selected (issue_query): Load only dependency evidence for active issue iss-42"
    ));
    assert!(selected_request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result issues-selected issue_query succeeded]")
    }));
}

/// Verifies unchanged issue discovery is skipped until a successful mutation
/// invalidates freshness, while an explicit refresh remains available.
///
/// `$fix-issues` legitimately queries again after resolving or otherwise
/// mutating an issue. The guard therefore keys freshness to the current
/// mutation state rather than imposing a one-query-per-turn budget.
#[test]
fn runtime_issue_query_freshness_skips_duplicates_and_resets_after_mutation() {
    let mut service = test_runtime_service();
    let config_root = temp_root("runtime-issue-query-freshness");
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-issue-freshness","input":"fix every open issue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeBatchProvider {
                response: runtime_issue_response(runtime_issue_query_batch("query-1", false)),
            },
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(
        first.action_results[0]
            .structured_content_json
            .as_deref()
            .is_some_and(|json| json.contains("snapshot_sha256"))
    );

    let duplicate = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_issue_response(runtime_issue_query_batch("query-2", false)),
            },
            1,
        )
        .unwrap()
        .remove(0);
    let duplicate_json = duplicate.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(
        duplicate_json.contains("skipped_runtime_issue_query_freshness_guard"),
        "{duplicate_json}"
    );
    assert!(duplicate_json.contains(r#""reused_action_id":"query-1""#));

    let mutation = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_issue_response(runtime_issue_add_batch("issue-add-1")),
            },
            1,
        )
        .unwrap()
        .remove(0);
    assert_eq!(mutation.action_results[0].status, ActionStatus::Succeeded);

    let after_mutation = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_issue_response(runtime_issue_query_batch("query-3", false)),
            },
            1,
        )
        .unwrap()
        .remove(0);
    let after_mutation_json = after_mutation.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(after_mutation_json.contains("snapshot_sha256"));
    assert!(!after_mutation_json.contains("skipped_runtime_issue_query_freshness_guard"));

    let explicit_refresh = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_issue_response(runtime_issue_query_batch("query-4", true)),
            },
            1,
        )
        .unwrap()
        .remove(0);
    let explicit_refresh_json = explicit_refresh.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(explicit_refresh_json.contains("snapshot_sha256"));
    assert!(!explicit_refresh_json.contains("skipped_runtime_issue_query_freshness_guard"));
}

/// Verifies `/loop` does not reset issue-query freshness inside one iteration.
///
/// Loop ownership changes how turns are scheduled after completion, but every
/// provider continuation inside the current iteration still belongs to one
/// logical turn and must reuse its successful issue snapshot.
#[test]
fn runtime_loop_iteration_reuses_issue_query_evidence() {
    let mut service = test_runtime_service();
    service.set_config_root(temp_root("runtime-loop-issue-query-freshness"));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-loop","method":"agent/shell/command","params":{"idempotency_key":"agent-loop-fix-issues","input":"/loop --limit 1 $fix-issues"}}"#,
        &primary,
    );
    assert!(start.contains(r#""kind":"mutated""#), "{start}");
    assert!(start.contains("state=running"), "{start}");

    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeBatchProvider {
                response: runtime_issue_response(runtime_issue_query_batch("loop-query-1", false)),
            },
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(
        first.action_results[0]
            .structured_content_json
            .as_deref()
            .is_some_and(|json| json.contains("snapshot_sha256"))
    );

    let duplicate = service
        .poll_agent_provider_tasks_with_provider(
            &RuntimeBatchProvider {
                response: runtime_issue_response(runtime_issue_query_batch("loop-query-2", false)),
            },
            1,
        )
        .unwrap()
        .remove(0);
    let duplicate_json = duplicate.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(
        duplicate_json.contains("skipped_runtime_issue_query_freshness_guard"),
        "{duplicate_json}"
    );
    assert!(duplicate_json.contains(r#""reused_action_id":"loop-query-1""#));
    service.terminate_all_pane_processes().unwrap();
}
