//! Turn runner async recovery tests.

use super::*;

#[tokio::test]
/// Verifies the async turn runner applies the same ephemeral MAAP repair path
/// used by the synchronous runner so production provider workers can recover
/// from model schema mistakes without adding repair instructions to context.
async fn async_turn_runner_retries_maap_validation_error_without_persisting_repair_context() {
    let turn = turn();
    let capability = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "request mcp capability".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![capability_action("capability-1", AgentCapability::Mcp)],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
    };
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid unavailable mcp action".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![AgentAction {
                id: "mcp-1".to_string(),
                rationale: "inspect unavailable state".to_string(),
                payload: AgentActionPayload::McpCall {
                    server: "missing".to_string(),
                    tool: "read".to_string(),
                    arguments_json: "{}".to_string(),
                },
            }],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
    };
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected async response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "Corrected asynchronously.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Ok(capability), Ok(invalid), Ok(corrected)]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        description: "List state".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &crate::security::permissions::ProductPermissionPlanning::new(
            &policy, &approvals, None,
        ),
        subagent_scope: None,
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: vec!["state".to_string()],
        available_mcp_tools: &tools,
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "inspect missing mcp state".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(provider.requests().len(), 3);
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("[MAAP repair state]")),
        "{:?}",
        execution.request.messages
    );
}

#[tokio::test]
/// Verifies the async turn runner repairs provider responses that omit the
/// parsed MAAP batch before it falls back to a failed terminal execution.
async fn async_turn_runner_retries_missing_provider_action_batch() {
    let turn = turn();
    let missing_batch = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "async plain text without maap".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    };
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected async missing batch response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "Corrected async missing batch.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Ok(missing_batch), Ok(corrected)]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &crate::security::permissions::ProductPermissionPlanning::new(
            &policy, &approvals, None,
        ),
        subagent_scope: None,
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "reply".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message
            .content
            .contains("provider response did not include a parsed MAAP action_batch")
            && message.content.contains("async plain text without maap")
    }));
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("[MAAP repair state]")),
        "{:?}",
        execution.request.messages
    );
}

#[tokio::test]
/// Verifies provider context-limit failures are returned to runtime recovery
/// instead of being summarized by the same oversized request.
///
/// The runtime owns active-turn context compaction and retry scheduling. Asking
/// the provider for a terminal failure summary with the rejected context would
/// repeat the same oversized payload and hide the recoverable condition.
async fn turn_runner_bubbles_context_limit_failure_to_runtime_recovery() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "OpenAI Responses API returned status 400: This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.",
        )
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary that should not be requested".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "retry later")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &crate::security::permissions::ProductPermissionPlanning::new(
            &policy, &approvals, None,
        ),
        subagent_scope: None,
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let error = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error.message().contains("maximum context length"),
        "{}",
        error.message()
    );
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
/// Verifies provider/controller failures that explicitly invite retry are
/// surfaced to the runtime retry scheduler instead of being converted into a
/// terminal failure-summary exchange.
async fn turn_runner_bubbles_provider_controller_retry_hint_to_runtime_retry() {
    let turn = turn();
    let retry_message = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID b331baf5-b254-46d7-8d3f-58b563ce7ee8 in your message.";
    let retry_error = crate::MezError::invalid_state(retry_message).with_provider_failure_json(
        serde_json::json!({
            "error": {
                "message": retry_message,
                "type": "server_error"
            }
        })
        .to_string(),
    );
    let provider = SequencedProvider::new(vec![
        Err(retry_error),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary that should not be requested".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "retry later")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &crate::security::permissions::ProductPermissionPlanning::new(
            &policy, &approvals, None,
        ),
        subagent_scope: None,
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let error = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("You can retry your request"));
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
/// Verifies retryable provider transport failures are not converted into
/// terminal failure summaries.
///
/// The async runtime owns retry backoff for transient provider failures. If the
/// turn runner asks the provider for a failure-summary `say` first, a successful
/// summary turns the retryable failure into a terminal failed turn and prevents
/// the actor from scheduling the retry.
async fn turn_runner_bubbles_retryable_provider_failure_to_runtime_retry() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider HTTP response read failed: error decoding response body",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary that should not be requested".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "retry later")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &crate::security::permissions::ProductPermissionPlanning::new(
            &policy, &approvals, None,
        ),
        subagent_scope: None,
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let error = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("provider HTTP response read failed"),
        "{}",
        error.message()
    );
    assert_eq!(provider.requests().len(), 1);
}
