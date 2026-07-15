//! Runtime tests for actions network behavior.

use super::*;

/// Verifies runtime-owned URL actions render a human-readable execution line
/// with themed gutter colors in normal mode.
///
/// URL actions do not pass through the pane shell, so they need their own
/// concise action line. Their result payload should remain out of normal mode
/// and be left to elevated logging and provider context.
#[test]
fn runtime_url_action_logs_single_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "fetch-1".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/file.txt".to_string(),
            format: None,
            max_bytes: None,
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pane_text.contains("agent: fetch url: https://example.test/file.txt"));
    assert!(!pane_text.contains("line one"));
    assert!(!pane_text.contains("line two"));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: fetch url:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "fetch url");
    let argument_column = display_column_for_fragment(&action_line.text, "https://");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    let argument_rendition = styled_line_rendition_at(action_line, argument_column);
    assert_eq!(
        prefix_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(prefix_rendition.dim);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
    assert_ne!(
        argument_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground),
        "{action_line:?}"
    );
}

/// Verifies network research failure feedback is scoped per action batch and
/// that mixed successful results are sent back with the failures.
///
/// Broken documentation links and 404s are normal web-research evidence. A
/// previous single turn-wide failure-feedback budget let an earlier bad URL
/// consume budget for a later batch of different URLs. The network budget should
/// instead be per batch and controlled by the configured action-failure limit.
#[test]
fn runtime_network_action_failures_get_additional_model_feedback_budget() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-network-failure-feedback","input":"research docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let success_action = mez_agent::AgentAction {
        id: "fetch-good".to_string(),
        rationale: "capture one usable source".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/ok".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let failed_action = mez_agent::AgentAction {
        id: "fetch-missing".to_string(),
        rationale: "try a moved source".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let mut failed_result = mez_agent::ActionResult::failed(
        &turn,
        &failed_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404",
    )
    .unwrap();
    failed_result.structured_content_json = Some(
        serde_json::json!({
            "kind": "fetch_url",
            "response": {
                "url": "https://example.test/missing",
                "status_code": 404
            }
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: mez_agent::ModelRequest {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::for_capability(
                mez_agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "research docs".to_string(),
            }],
        },
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "mixed network fetches".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![success_action.clone(), failed_action.clone()],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            mez_agent::ActionResult::succeeded(
                &turn,
                &success_action,
                vec!["usable source body".to_string()],
                None,
            ),
            failed_result,
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    let previous_key = "turn-1:previous-network-batch".to_string();
    service
        .agent_turn_failure_feedback_attempts
        .insert(previous_key.clone(), 3);
    service
        .present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)
        .unwrap();
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent warning: URL fetch failed (HTTP 404)"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("model received the response detail")
            && pane_text.contains("for recovery"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("https://example.test/missing"),
        "{pane_text}"
    );

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "network_research_failed_action",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        service
            .agent_turn_failure_feedback_attempts
            .get(&previous_key)
            .copied(),
        Some(3)
    );
    let mut attempt_values = service
        .agent_turn_failure_feedback_attempts
        .values()
        .copied()
        .collect::<Vec<_>>();
    attempt_values.sort_unstable();
    assert_eq!(attempt_values, vec![1, 3]);
    assert!(service.pending_agent_provider_tasks.contains("turn-1"));
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-good fetch_url succeeded]")
            && block.content.contains("usable source body")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-missing fetch_url failed]")
            && block.content.contains("network request returned HTTP 404")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint && block.content.contains("attempt=1 max=5")
    }));
    assert!(context.blocks.iter().all(|block| {
        block.source != ContextSourceKind::RuntimeHint
            || !block.content.contains("Mutation-evidence rule")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}
