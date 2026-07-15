//! Turn Runner tests for memory behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies route-matched MCP availability does not make memory unusable.
///
/// MCP routing hints should steer models away from memory as placeholder setup,
/// but a legitimate durable-context memory action must still reach the runtime
/// when persistent memory is enabled and MCP tools are also available.
fn turn_runner_accepts_memory_search_with_matched_mcp_available() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Memory,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "search durable prior context about required function call compliance regressions".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![memory_search_action("memory-search-1")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let mcp_tool = McpPromptTool {
        server_id: "githubcopilot".to_string(),
        tool_name: "list_ci_results".to_string(),
        description: "Read GitHub CI check results for a repository".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object"}"#.to_string(),
    };
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
        available_mcp_servers: vec!["githubcopilot".to_string()],
        available_mcp_tools: std::slice::from_ref(&mcp_tool),
        memory_actions_enabled: true,
        issue_actions_enabled: true,
    };
    let context = mez_agent::append_mcp_context(
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "use githubcopilot and recall the durable project preference".to_string(),
        }])
        .unwrap(),
        &mez_agent::McpPromptSummary {
            available_servers: vec![mez_agent::McpPromptServer {
                server_id: "githubcopilot".to_string(),
                display_name: "GitHub Copilot".to_string(),
                purpose: "GitHub repository and CI operations".to_string(),
                usage_instructions: String::new(),
                tool_count: 1,
                approval_required_tool_count: 0,
            }],
            available_tools: vec![mcp_tool.clone()],
            unavailable_servers: Vec::new(),
        },
    )
    .unwrap();

    let execution = runner.run_turn(&mut ledger, turn, context).unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec!["memory action accepted for runtime execution"]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"pending_runtime_memory""#));
}

#[test]
/// Verifies prior memory stores in context do not block later store actions.
///
/// Macro and subagent workflows may need to persist a final document after
/// earlier memory activity in the same active context. This regression ensures
/// prior stores no longer consume a runtime turn budget that would skip the
/// final durable store.
fn turn_runner_accepts_memory_store_after_prior_store_context() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Memory,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "try one more memory store".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![memory_store_action("memory-store-2")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
        memory_actions_enabled: true,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![
                ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    label: "user".to_string(),
                    content: "store this repeatedly".to_string(),
                },
                ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label: "action result memory-store-1".to_string(),
                    content: "[action_result memory-store-1 memory_store succeeded]\ncontent:\nmemory_store persisted 1 record".to_string(),
                },
            ])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec!["memory action accepted for runtime execution"]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"pending_runtime_memory""#));
}

#[test]
/// Verifies memory actions plan as runtime-owned work instead of falling
/// through the shell-action planner.
///
/// Persistent memory operations execute through the runtime store after the
/// planner marks them as running. This regression ensures the planner produces
/// a pending runtime result so memory actions can continue instead of failing
/// with the shell-backed-action planning error.
fn turn_runner_accepts_memory_store_for_runtime_execution() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Memory,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "store the requested memory".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "memory-1".to_string(),
                    rationale: "store durable project context".to_string(),
                    payload: AgentActionPayload::MemoryStore {
                        kind: "fact".to_string(),
                        priority: Some(60),
                        scope: Some("project".to_string()),
                        keywords: vec!["memory".to_string(), "regression".to_string()],
                        content: "remember this regression scenario".to_string(),
                        expires_in_days: Some(7),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
        memory_actions_enabled: true,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "remember this for later".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec!["memory action accepted for runtime execution"]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"pending_runtime_memory""#));
}

#[test]
/// Verifies same-turn memory search results already in context consume the
/// runner budget.
///
/// Memory loops often appear across continuation turns rather than inside one
/// provider batch. This regression makes those prior action-result blocks count
/// toward the same user-turn limit so a third search is skipped instead of
/// extending a paraphrase loop.
fn turn_runner_counts_prior_memory_search_results_from_context() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Memory,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "try one more memory search".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![memory_search_action("memory-search-3")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
        memory_actions_enabled: true,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![
                ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    label: "user".to_string(),
                    content: "search memory repeatedly".to_string(),
                },
                ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label: "action result memory-search-1".to_string(),
                    content: "[action_result memory-search-1 memory_search succeeded]\ncontent:\nmemory_search returned 0 records".to_string(),
                },
                ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label: "action result memory-search-2".to_string(),
                    content: "[action_result memory-search-2 memory_search succeeded]\ncontent:\nmemory_search returned 0 records".to_string(),
                },
            ])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec![
            "memory_search skipped: per-turn memory search limit reached; continue the task with direct artifacts, current action results, MCP, shell, web, or a bounded report instead, and do not search memory again this turn"
        ]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(
        structured.contains(r#""state":"skipped_runtime_memory_guardrail""#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""code":"memory_search_turn_limit""#),
        "{structured}"
    );
}

#[test]
/// Verifies wrapper-compliance memory placeholders are skipped at runtime.
///
/// The model can still see and use memory actions, but a memory action whose
/// rationale says it is only satisfying a required current-actions/function
/// wrapper should not execute or consume a same-turn memory budget slot.
fn turn_runner_skips_memory_search_used_as_action_wrapper_placeholder() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Memory,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale:
                    "Complying with a required immediate current-actions call before proceeding"
                        .to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![
                    memory_search_action("memory-search-placeholder"),
                    memory_search_action("memory-search-legitimate"),
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
        memory_actions_enabled: true,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "use memory only if it is needed".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 2);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(execution.action_results[1].status, ActionStatus::Succeeded);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec![
            "memory action skipped: rationale identified this as action-wrapper compliance rather than a concrete durable-context need; continue with the direct task action instead"
        ]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(
        structured.contains(r#""state":"skipped_runtime_memory_guardrail""#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""code":"memory_wrapper_placeholder""#),
        "{structured}"
    );
    assert!(structured.contains(r#""limit":0"#), "{structured}");
    assert_eq!(
        execution.action_results[1].content_texts(),
        vec![
            "memory action skipped: rationale identified this as action-wrapper compliance rather than a concrete durable-context need; continue with the direct task action instead"
        ]
    );
}

#[test]
/// Verifies the runner accepts only the first two memory searches in one turn.
///
/// Prompt guidance should reduce memory-search eagerness, but the runtime must
/// also stop repeated same-turn searches deterministically. This regression
/// ensures an over-budget search is converted into a successful skipped action
/// result while the earlier memory actions still proceed to runtime execution.
fn turn_runner_skips_memory_searches_after_per_turn_limit() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Memory,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "try repeated memory searches".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![
                    memory_search_action("memory-search-1"),
                    memory_search_action("memory-search-2"),
                    memory_search_action("memory-search-3"),
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
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
        memory_actions_enabled: true,
        issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "search memory repeatedly".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 3);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(execution.action_results[1].status, ActionStatus::Running);
    assert_eq!(execution.action_results[2].status, ActionStatus::Succeeded);
    assert_eq!(
        execution.action_results[2].content_texts(),
        vec![
            "memory_search skipped: per-turn memory search limit reached; continue the task with direct artifacts, current action results, MCP, shell, web, or a bounded report instead, and do not search memory again this turn"
        ]
    );
    let structured = execution.action_results[2]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(
        structured.contains(r#""state":"skipped_runtime_memory_guardrail""#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""code":"memory_search_turn_limit""#),
        "{structured}"
    );
    assert!(structured.contains(r#""limit":2"#), "{structured}");
}
