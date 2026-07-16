//! Turn runner retry tests.

use super::*;

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
        mez_agent::ModelInteractionKind::Repair
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
                        status: mez_agent::SayStatus::Progress,
                        text: "The provider request failed before an action could run.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
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
            assert_eq!(*status, mez_agent::SayStatus::Final)
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
