//! Turn Runner tests for recovery behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
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
            .all(|message| !message.content.contains("ephemeral maap repair")),
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
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
            .all(|message| !message.content.contains("ephemeral maap repair")),
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
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

#[test]
/// Verifies that a provider response without a MAAP action batch fails the turn
/// instead of silently converting malformed structured output into completion.
fn turn_runner_fails_response_without_action_batch() {
    let turn = turn();
    let provider = BatchProvider {
        response: ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "plain text without maap".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
    };
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn.clone(),
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "summarize".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.action_results.is_empty());
    assert_eq!(ledger.turns()[0].turn_id, turn.turn_id);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Failed);
}

#[test]
/// Verifies mixed capability-routing and executable batches recover without effects.
///
/// A model may request a missing capability and optimistically include the
/// action that needs it in the same response. The controller must not execute
/// that invalid mixed batch, but it should still honor the capability request
/// and ask the model to re-emit deferred work on the expanded action surface.
fn turn_runner_recovers_mixed_capability_and_execution_batch_without_effects() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request shell and run it".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![
                    capability_action("capability-1", AgentCapability::Shell),
                    shell_action("shell-1"),
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "Ready.")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect the repository".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.action_type != "shell_command")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    let execution_actions = requests[1].allowed_actions.action_type_names();
    assert!(execution_actions.contains(&"shell_command"));
    assert!(execution_actions.contains(&"apply_patch"));
    assert!(execution_actions.contains(&"request_capability"));
    let recovery_context = requests[1]
        .messages
        .iter()
        .find(|message| {
            message
                .content
                .contains("[mixed capability batch recovery]")
        })
        .expect("missing mixed capability recovery context");
    assert!(recovery_context.content.contains("shell_command"));
}

#[test]
/// Verifies mixed capability-routing batches defer heredoc shell validation.
///
/// When a provider combines `request_capability` with a shell command that
/// would otherwise fail MAAP validation, the runner must treat the response as
/// mixed capability routing first, avoid executing or validating the deferred
/// shell payload, and ask the model to re-emit work on the expanded surface.
fn turn_runner_recovers_mixed_capability_batch_before_heredoc_validation() {
    let turn = turn();
    let mut deferred_heredoc = shell_action("shell-heredoc");
    if let AgentActionPayload::ShellCommand {
        command, summary, ..
    } = &mut deferred_heredoc.payload
    {
        *summary = "Write a Rust file with a heredoc".to_string();
        *command = "cat > hello.rs <<'EOF'\nfn main() {}\nEOF".to_string();
    }
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request shell and write file".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![
                    capability_action("capability-1", AgentCapability::Shell),
                    deferred_heredoc,
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "Ready.")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "write a short Rust program".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.response.raw_text, "ready");
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.action_type != "shell_command")
    );
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("ephemeral maap repair"))
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    let recovery_context = requests[1]
        .messages
        .iter()
        .find(|message| {
            message
                .content
                .contains("[mixed capability batch recovery]")
        })
        .expect("missing mixed capability recovery context");
    assert!(recovery_context.content.contains("shell_command"));
    assert!(
        !recovery_context
            .content
            .contains("heredoc redirection is disabled")
    );
}

#[test]
/// Verifies legacy model-authored completion actions are rejected when omitted
/// from the active allowed-action surface.
///
/// `complete` is not exposed by the current provider schema, so a legacy
/// provider response that injects it must go through the normal action-surface
/// validation and repair path instead of bypassing execution checks.
fn turn_runner_repairs_legacy_complete_during_capability_decision() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text:
                r#"{"rationale":"test action batch rationale","actions":[{"type":"complete"}]}"#
                    .to_string(),
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
                    id: "complete-1".to_string(),
                    rationale: "legacy completion".to_string(),
                    payload: AgentActionPayload::Complete,
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request workspace-write capability".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![capability_action("capability-1", AgentCapability::Shell)],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "Ready.")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect the workspace".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::Repair
    );
    assert!(
        requests[1].messages.iter().any(|message| message
            .content
            .contains("complete is not part of the provider action surface")),
        "{:?}",
        requests[1].messages
    );
    assert!(
        !requests[0]
            .allowed_actions
            .action_type_names()
            .contains(&"complete")
    );
}

#[tokio::test]
/// Verifies malformed failure-summary MAAP responses get one repair attempt.
///
/// The summary request is constrained to response-only `say` actions. If the
/// model returns malformed MAAP for that response, the existing MAAP repair
/// prompt should give it a bounded chance to emit the valid final say batch
/// rather than silently dropping the summary.
async fn turn_runner_repairs_malformed_failure_summary_response() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider schema rejected request",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "not a summary batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "repaired summary".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action(
                    "say-1",
                    "The provider failed before any action ran.",
                )],
                final_turn: false,
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
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
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        execution
            .response
            .raw_text
            .contains("controller_failure_summary")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].interaction_kind,
        crate::agent::ModelInteractionKind::Repair
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .any(|message| message.content.contains("[ephemeral maap repair]")),
        "{:?}",
        requests[2].messages
    );
}

#[test]
/// Verifies model-authored aborts are repaired instead of treated as a valid
/// way to end recoverable turns. A model that merely needs more repository
/// context must continue by requesting capability or performing available
/// actions rather than converting a solvable task into a terminal abort.
fn turn_runner_repairs_model_authored_abort_during_capability_decision() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"rationale":"test action batch rationale","actions":[{"reason":"need more repository context","type":"abort"}]}"#
                .to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![abort_action("abort-1", "need more repository context")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request workspace-read capability".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![capability_action("capability-1", AgentCapability::Shell)],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "Ready.")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect the workspace".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::Repair
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| {
                message
                    .content
                    .contains("abort is not part of the provider action surface")
            })
            .unwrap()
            .content
            .contains("abort is not part of the provider action surface"),
        "{:?}",
        requests[1].messages
    );
    assert!(
        !requests[0]
            .allowed_actions
            .action_type_names()
            .contains(&"abort")
    );
}

#[test]
/// Verifies heredoc shell commands are repairable MAAP validation failures.
///
/// Shell commands are exposed only after a capability request, so this test
/// first grants the shell surface and then returns a disabled heredoc command.
/// The runner should send a bounded ephemeral repair request with file-action
/// guidance, accept the corrected response, and avoid retaining the repair
/// diagnostic in durable execution context.
fn turn_runner_repairs_shell_command_heredoc_validation_error() {
    let turn = turn();
    let capability = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "request shell capability".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![capability_action("capability-1", AgentCapability::Shell)],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
    };
    let mut heredoc_action = shell_action("shell-heredoc");
    if let AgentActionPayload::ShellCommand {
        command, summary, ..
    } = &mut heredoc_action.payload
    {
        *summary = "Write a Rust file with a heredoc".to_string();
        *command = "cat > hello.rs <<'EOF'\nfn main() {}\nEOF".to_string();
    }
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid heredoc shell response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![heredoc_action],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
    };
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected file action response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "I will use a file action instead.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Ok(capability), Ok(invalid), Ok(corrected)]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn.clone(),
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "write a short Rust program".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(
        execution.response.raw_text,
        "corrected file action response"
    );
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("heredoc redirection is disabled")),
        "{:?}",
        execution.request.messages
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let repair_message = &requests[2]
        .messages
        .iter()
        .find(|message| message.content.contains("ephemeral maap repair"))
        .unwrap()
        .content;
    assert!(
        repair_message.contains("ephemeral maap repair"),
        "{repair_message}"
    );
    assert!(
        repair_message.contains("heredoc redirection is disabled"),
        "{repair_message}"
    );
    assert!(repair_message.contains("apply_patch"), "{repair_message}");
}

#[test]
/// Verifies that MAAP validation failures are repaired through a bounded ephemeral
/// provider retry before the runtime records a failed turn. The correction
/// instruction must be present only in the retry request; the returned
/// execution keeps the original request so transcripts and later context do not
/// inherit the validation error when repair succeeds.
fn turn_runner_retries_maap_validation_error_without_persisting_repair_context() {
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
        raw_text: "corrected say response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "I cannot access that MCP server.")],
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: vec!["state".to_string()],
        available_mcp_tools: &tools,
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn.clone(),
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect missing mcp state".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.response.raw_text, "corrected say response");
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("maap_validation_error")
                && !message.content.contains("ephemeral maap repair")),
        "{:?}",
        execution.request.messages
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .messages
            .iter()
            .find(|message| message.content.contains("ephemeral maap repair"))
            .unwrap()
            .content
            .contains("ephemeral maap repair"),
        "{:?}",
        requests[2].messages
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .find(|message| message.content.contains("ephemeral maap repair"))
            .unwrap()
            .content
            .contains("unavailable server"),
        "{:?}",
        requests[2].messages
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .find(|message| message.content.contains("ephemeral maap repair"))
            .unwrap()
            .content
            .contains("The corrected batch is the schema-valid wrapper for the next useful action"),
        "{:?}",
        requests[2].messages
    );
    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    assert!(
        entries.iter().all(|entry| {
            !entry.content.contains("ephemeral maap repair")
                && !entry.content.contains("maap_validation_error")
                && !entry.content.contains("invalid unavailable mcp action")
        }),
        "{entries:?}"
    );
}

#[test]
/// Verifies that malformed provider-native MAAP output can also be repaired
/// without surfacing the malformed output as a durable turn when the retry
/// returns a valid action batch.
fn turn_runner_retries_malformed_provider_maap_output() {
    let turn = turn();
    let malformed =
        crate::MezError::invalid_args("provider MAAP output is malformed: missing required field")
            .with_provider_raw_text(
                r#"{"rationale":"test action batch rationale","actions":[{"type":"say"}]}"#,
            );
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected malformed response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "Corrected.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Err(malformed), Ok(corrected)]);
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "reply".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| {
                message.content.contains(
                    r#"{"rationale":"test action batch rationale","actions":[{"type":"say"}]}"#,
                )
            })
            .unwrap()
            .content
            .contains(r#"{"rationale":"test action batch rationale","actions":[{"type":"say"}]}"#),
        "{:?}",
        requests[1].messages
    );
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("ephemeral maap repair")),
        "{:?}",
        execution.request.messages
    );
}

#[test]
/// Verifies a provider response with no parsed MAAP batch enters the same
/// ephemeral repair flow as malformed provider-native MAAP output instead of
/// becoming an immediate failed turn.
fn turn_runner_retries_missing_provider_action_batch() {
    let turn = turn();
    let missing_batch = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "plain text without maap".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    };
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected missing batch response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "Corrected missing batch.")],
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "reply".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message
            .content
            .contains("provider response did not include a parsed MAAP action_batch")
            && message.content.contains("plain text without maap")
    }));
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("ephemeral maap repair")),
        "{:?}",
        execution.request.messages
    );
}

#[test]
/// Verifies failure-summary provider calls retry transient transport failures.
///
/// The final failure summary is best-effort, but the summary request is still a
/// provider interaction. A transient transport failure while asking for the
/// summary should use the same retry classification instead of immediately
/// collapsing to the unsummarized terminal provider error.
fn turn_runner_retries_retryable_failure_summary_provider_call() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider schema rejected request",
        )),
        Err(crate::MezError::invalid_state(
            "provider HTTP response read failed: error decoding response body",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary after retry".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action(
                    "say-1",
                    "The provider failed before any action ran.",
                )],
                final_turn: false,
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        execution
            .response
            .raw_text
            .contains("controller_failure_summary")
    );
    assert_eq!(provider.requests().len(), 3);
}

#[test]
/// Verifies repair responses can recover by routing disallowed actions through capability negotiation.
///
/// A repair interaction may expose only `say` and `request_capability` while
/// the model still emits a valid concrete action such as `shell_command`. The
/// runner should convert that disallowed concrete action into a capability
/// continuation, avoid the terminal failure-summary path, and keep ephemeral
/// repair instructions out of the durable request.
fn turn_runner_routes_repair_disallowed_shell_action_through_capability_recovery() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "malformed response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "repair emitted shell command".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![shell_action("shell-repair")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "Ready.")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn.clone(),
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect the workspace".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.response.raw_text, "ready");
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::Repair
    );
    let recovery_request = &requests[2];
    let action_types = recovery_request.allowed_actions.action_type_names();
    assert!(action_types.contains(&"shell_command"), "{action_types:?}");
    assert!(action_types.contains(&"apply_patch"), "{action_types:?}");
    assert!(recovery_request.messages.iter().any(|message| {
        message
            .content
            .contains("[disallowed action capability recovery]")
    }));
    assert!(
        recovery_request
            .messages
            .iter()
            .all(|message| !message.content.contains("[ephemeral maap repair]")),
        "{:?}",
        recovery_request.messages
    );
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("[ephemeral maap repair]")),
        "{:?}",
        execution.request.messages
    );
}

#[test]
/// Verifies terminal provider/controller failures get one response-only
/// characterization pass. The summary request exposes only `say`, which lets
/// the model explain the failure without recursively requesting tools or
/// capabilities after the controller has already failed the turn.
fn turn_runner_summarizes_terminal_provider_failure_with_say_only_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider schema rejected request",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary".to_string(),
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
                    id: "say-1".to_string(),
                    rationale: "summarize the controller failure".to_string(),
                    payload: AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Progress,
                        text: "The provider request failed before an action could run.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: false,
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
        subagent_scope: None,
        subagent_scope_enforcement: &crate::subagent::AGENT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let summary_batch = execution.response.action_batch.as_ref().unwrap();
    assert!(summary_batch.final_turn);
    match &summary_batch.actions[0].payload {
        AgentActionPayload::Say { status, .. } => {
            assert_eq!(*status, crate::agent::SayStatus::Final)
        }
        payload => panic!("unexpected payload: {payload:?}"),
    }
    assert!(execution.response.raw_text.contains("provider_error"));
    assert!(
        execution
            .response
            .raw_text
            .contains("controller_failure_summary")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].allowed_actions.action_type_names(), vec!["say"]);
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[controller failure summary]"))
            .unwrap()
            .content
            .contains("[controller failure summary]"),
        "{:?}",
        requests[1].messages
    );
}
