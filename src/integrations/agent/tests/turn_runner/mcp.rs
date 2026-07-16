//! Turn Runner tests for mcp behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies that MCP calls with arguments matching the advertised schema are
/// accepted by MAAP validation and can proceed to runtime planning.
///
/// This pairs with the missing-required-field regression so the runtime gate
/// rejects malformed calls without blocking valid MCP usage.
fn turn_runner_accepts_mcp_actions_matching_input_schema_arguments() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
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
                    rationale: "read requested file through external integration".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: r#"{"path":"README.md"}"#.to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: false,
        input_schema_json:
            r#"{"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}"#
                .to_string(),
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
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""server":"fs""#), "{structured}");
    assert!(structured.contains(r#""tool":"read_file""#), "{structured}");
    assert!(
        structured.contains(r#""arguments":{"path":"README.md"}"#),
        "{structured}"
    );
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
}

#[test]
/// Verifies turn runner accepts mcp actions without required approval.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_accepts_mcp_actions_without_required_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
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
                    rationale: "inspect external state".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "state".to_string(),
                        tool: "list".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "list state".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        !execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("approval_required")
    );
}

#[test]
/// Verifies that MCP tools with approval requirements follow the same
/// auto-allow contract as shell commands: they may run only when the model
/// supplies an explicit reasoned assertion for the active request.
fn turn_runner_auto_allows_mcp_actions_with_model_assertion() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
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
                    rationale: "read requested project file through external integration"
                        .to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::AutoAllow);
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: true,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
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
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""state":"auto_allowed""#)
    );
}

#[test]
/// Verifies turn runner blocks mcp actions requiring approval.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_blocks_mcp_actions_requiring_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
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
                    rationale: "read through external integration".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: true,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
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
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"kind\":\"mcp_call\"")
    );
}

#[test]
/// Verifies turn runner executes accepted mcp actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_executes_accepted_mcp_actions() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![mcp_action("mcp-1")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpExecutionResponse {
            content_json: r#"[{"type":"text","text":"ok"}]"#.to_string(),
            structured_content_json: None,
            is_error: false,
        },
    };

    let execution = runner
        .run_turn_with_mcp_executor(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "list state".to_string(),
            }])
            .unwrap(),
            &mut executor,
            |_action| Ok(mcp_plan()),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(executor.plans.len(), 1);
}

#[test]
/// Verifies full-access approval policy accepts MCP actions that would
/// otherwise need an explicit approval prompt.
///
/// This protects the user-selected full-access mode from being treated like
/// the default ask mode for semantic integration actions.
fn turn_runner_full_access_accepts_mcp_actions_requiring_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
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
                    rationale: "read through external integration".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: true,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
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
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains(r#""state":"full_access""#)),
        "{execution:?}"
    );
}

#[test]
/// Verifies that runtime-discovered MCP tool schemas are attached to the
/// provider request rather than being used only for post-response MAAP
/// validation. Provider adapters need this metadata to constrain native
/// structured output before the model proposes an MCP action.
fn turn_runner_passes_mcp_tool_schemas_to_provider_request() {
    let turn = turn();
    let provider = RequestCapturingProvider {
        response: ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
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
                    id: "complete".to_string(),
                    rationale: "done".to_string(),
                    payload: AgentActionPayload::Complete,
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
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
    let mut ledger = AgentTurnLedger::new(false);
    runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "finish".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    let request = provider
        .last_request
        .borrow()
        .clone()
        .expect("provider should receive request");
    assert_eq!(request.available_mcp_tools, tools);
}

#[test]
/// Verifies that provider MAAP output is rejected before action planning when
/// it names a tool that was not advertised as available for an otherwise
/// available MCP server.
fn turn_runner_rejects_mcp_actions_for_unavailable_tools_before_planning() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
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
                    rationale: "inspect disabled external state".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "state".to_string(),
                        tool: "write".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "write state".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.action_results.is_empty());
    assert!(
        execution
            .response
            .raw_text
            .contains("maap_validation_error"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("unavailable or disabled tool"),
        "{}",
        execution.response.raw_text
    );
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Failed);
}

#[test]
/// Verifies that MCP tool arguments are checked against required fields in
/// the advertised input schema before action planning.
///
/// This keeps provider-side schema gaps from reaching external integrations as
/// live tool calls when the runtime already knows the tool's input contract.
fn turn_runner_rejects_mcp_actions_missing_required_schema_arguments_before_planning() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
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
                    rationale: "read requested file through external integration".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: false,
        input_schema_json:
            r#"{"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}"#
                .to_string(),
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
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.action_results.is_empty());
    assert!(
        execution
            .response
            .raw_text
            .contains("maap_validation_error"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("missing required field `path`"),
        "{}",
        execution.response.raw_text
    );
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Failed);
}
