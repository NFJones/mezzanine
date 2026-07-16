//! Turn runner repair tests.

use super::*;

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
        mez_agent::ModelInteractionKind::Repair
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
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
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
        mez_agent::ModelInteractionKind::Repair
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
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
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
