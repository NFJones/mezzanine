//! Turn runner capability tests.

use super::*;

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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
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
        mez_agent::ModelInteractionKind::ActionExecution
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
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
        mez_agent::ModelInteractionKind::ActionExecution
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
        permissions: &crate::permissions::ProductPermissionPlanning::new(&policy, &approvals, None),
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
