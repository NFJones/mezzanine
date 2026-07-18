//! Turn Runner tests for capabilities behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies disabled local issue tracking denies issue capability before the
/// provider-visible issue action surface can be exposed.
///
/// This protects the action-surface contract documented in `SPEC.md`: when
/// `issues.enabled` is false, models may ask for the capability but the
/// controller must keep them on the non-effecting capability-decision surface
/// instead of revealing `issue_add`, `issue_query`, or related actions.
fn turn_runner_denies_issues_capability_when_issue_tracking_disabled() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request issues capability".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![capability_action("capability-1", AgentCapability::Issues)],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "finish after denied capability".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "issue tracking is disabled")],
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
        issue_actions_enabled: false,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "list project issues".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityContinuation
    );
    assert_eq!(
        requests[1].allowed_actions.action_type_names(),
        vec!["say", "request_capability"]
    );
    let capability_context = requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[capability denied]"))
        .expect("missing denied capability context");
    assert!(
        capability_context
            .content
            .contains("issues capability requires local issue tracking to be enabled"),
        "{}",
        capability_context.content
    );
    assert!(
        !requests[1]
            .allowed_actions
            .action_type_names()
            .contains(&"issue_query")
    );
}

#[test]
/// Verifies available MCP tools are exposed on the main model's initial
/// action surface instead of requiring a separate capability request.
///
/// MCP-backed integrations should be callable immediately when the runtime has
/// already surfaced concrete tools for the turn. This regression ensures the
/// first provider request can emit `mcp_call` directly while still retaining
/// `request_capability` for shell, network, and other coarse effects.
fn turn_runner_exposes_mcp_actions_on_initial_surface_when_available() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![Ok(ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "done".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "finish after checking MCP tools".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "done")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    })]);
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }];
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
        available_mcp_servers: vec!["fs".to_string()],
        available_mcp_tools: &tools,
        memory_actions_enabled: false,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "use any helpful MCP integration before answering".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityDecision
    );
    let allowed_actions = requests[0].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"mcp_call"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert!(!allowed_actions.contains(&"shell_command"));
}

#[test]
/// Verifies enabled persistent memory is exposed on the main model's initial
/// action surface instead of requiring a separate capability request.
///
/// Memory lookup and storage are intended to be routine context actions for the
/// main model when runtime memory is enabled. This regression ensures the first
/// provider request can call `memory_search` or `memory_store` directly while
/// still retaining `request_capability` for shell, network, MCP, and other
/// coarse effects.
fn turn_runner_exposes_memory_actions_on_initial_surface_when_enabled() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![Ok(ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "done".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "finish after inspecting memory".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "done")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    })]);
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
        memory_actions_enabled: true,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "use any helpful memory before answering".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityDecision
    );
    let allowed_actions = requests[0].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"memory_search"));
    assert!(allowed_actions.contains(&"memory_store"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert!(!allowed_actions.contains(&"shell_command"));
}

#[test]
/// Verifies that executable action surfaces are only exposed after the model
/// asks for a coarse capability. This protects the state-machine boundary that
/// keeps a greeting or other simple request from starting with shell or
/// network actions before the model opts into those broader capabilities.
fn turn_runner_exposes_shell_actions_only_after_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request shell capability".to_string(),
            usage: ModelTokenUsage {
                input_tokens: 900,
                output_tokens: 20,
                reasoning_tokens: 5,
                cached_input_tokens: Some(300),
                cache_write_input_tokens: None,
            },
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
            raw_text: "shell action".to_string(),
            usage: ModelTokenUsage {
                input_tokens: 1151,
                output_tokens: 50,
                reasoning_tokens: 12,
                cached_input_tokens: Some(180),
                cache_write_input_tokens: None,
            },
            latest_request_usage: Some(ModelTokenUsage {
                input_tokens: 251,
                output_tokens: 30,
                reasoning_tokens: 7,
                cached_input_tokens: Some(80),
                cache_write_input_tokens: None,
            }),
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![shell_action("shell-1")],
                final_turn: false,
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
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "where am I".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.response.usage.input_tokens, 2051);
    assert_eq!(execution.response.usage.output_tokens, 70);
    assert_eq!(execution.response.usage.reasoning_tokens, 17);
    assert_eq!(execution.response.usage.cached_input_tokens, Some(480));
    assert_eq!(execution.latest_response_usage.input_tokens, 251);
    assert_eq!(execution.latest_response_usage.output_tokens, 30);
    assert_eq!(execution.latest_response_usage.reasoning_tokens, 7);
    assert_eq!(
        execution.latest_response_usage.cached_input_tokens,
        Some(80)
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityDecision
    );
    let initial_actions = requests[0].allowed_actions.action_type_names();
    assert!(initial_actions.contains(&"request_capability"));
    assert!(!initial_actions.contains(&"shell_command"));
    assert!(!initial_actions.contains(&"fetch_url"));
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityContinuation
    );
    let execution_actions = requests[1].allowed_actions.action_type_names();
    assert!(execution_actions.contains(&"shell_command"));
    assert!(execution_actions.contains(&"request_capability"));
    assert!(!execution_actions.contains(&"fetch_url"));
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[capability granted]"))
            .unwrap()
            .content
            .contains("[capability granted]"),
        "{:?}",
        requests[1].messages
    );
}

#[test]
/// Verifies that the controller grants a network fetch capability without an
/// active-context URL provenance check.
///
/// Action scoping decides whether `fetch_url` is exposed at all. The concrete
/// URL target is validated later by the parser, permission layer, executor byte
/// bounds, and network loop guard.
fn turn_runner_grants_fetch_capability_without_context_url() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request fetch capability".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![capability_action(
                    "capability-1",
                    AgentCapability::NetworkFetch,
                )],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "fallback say".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "hello")],
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

    let execution = runner
        .run_turn(
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
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityContinuation
    );
    let allowed_actions = requests[1].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"fetch_url"));
    assert!(allowed_actions.contains(&"request_capability"));
    let decision_message = &requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[capability granted]"))
        .unwrap()
        .content;
    assert!(decision_message.contains("[capability granted]"));
    assert!(decision_message.contains("capability is permitted"));
}
