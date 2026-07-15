//! Turn Runner tests for execution behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies config changes follow the active approval policy instead of using
/// a bespoke hard-block path.
///
/// Live configuration changes still run through the runtime config-control path,
/// but permissive approval modes should accept the action at planning time just
/// like other privileged model actions.
fn turn_runner_accepts_config_change_with_full_access_and_bypass() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::ConfigChange,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "change the requested live setting".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![config_change_action("config-1")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let mut policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
    policy.set_approval_bypass(true);
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
                content: "change my theme to kanagawa".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec!["configuration change accepted for runtime application"]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"bypassed""#), "{structured}");
    assert!(
        structured.contains(r#""status":"pending_runtime_config_change""#),
        "{structured}"
    );
}

#[test]
/// Verifies that one capability-decision response can request multiple coarse
/// capabilities. Multi-agent analysis commonly needs workspace inspection plus
/// subagent coordination, and the controller should expose the union of those
/// granted surfaces instead of failing the batch as invalid.
fn turn_runner_accepts_multiple_capability_requests_in_one_batch() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request read and subagent capability".to_string(),
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
                    say_action("say-1", "I will inspect and subdivide the work."),
                    capability_action("capability-1", AgentCapability::Shell),
                    capability_action("capability-2", AgentCapability::Subagent),
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
                actions: vec![say_action("say-2", "Ready to proceed.")],
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

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "compare mezzanine to codex using agents".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::ActionExecution
    );
    let allowed_actions = requests[1].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"shell_command"));
    assert!(allowed_actions.contains(&"apply_patch"));
    assert!(allowed_actions.contains(&"spawn_agent"));
    assert!(allowed_actions.contains(&"send_message"));
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[capability decisions]"))
            .unwrap()
            .content
            .contains("[capability decisions]"),
        "{:?}",
        requests[1].messages
    );
}

#[test]
/// Verifies that capability negotiation accepts an accompanying visible `say`
/// action. Provider schemas expose both actions during the initial
/// non-executing phase, so the runner must not fail when the model emits a
/// short status line with the capability request.
fn turn_runner_accepts_say_with_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "say and request shell capability".to_string(),
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
                    say_action("say-1", "I will inspect the shell state."),
                    capability_action("capability-1", AgentCapability::Shell),
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
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
                content: "where am I".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::ActionExecution
    );
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
/// Verifies capability negotiation does not reintroduce skill lookup actions
/// after an explicit `$skill` prompt has already loaded the workflow.
///
/// The original failure mode repeatedly asked for `request_skills` after the
/// runtime reported that `$create-skill` was already loaded. This locks the
/// suppression to both the initial capability-decision request and the
/// post-capability execution request.
fn turn_runner_keeps_skill_actions_suppressed_after_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
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
                rationale: "finish after capability grant".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "done")],
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
            AgentContext::new(vec![
                ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    label: "explicit skill create-skill".to_string(),
                    content: "# Skill: create-skill\n\nCreate or update skills.".to_string(),
                },
                ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    label: "user prompt".to_string(),
                    content: "$create-skill create a review skill".to_string(),
                },
            ])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].allowed_actions.action_type_names(),
        vec!["say", "request_capability"]
    );
    assert_eq!(
        requests[1].allowed_actions.action_type_names(),
        vec!["say", "request_capability", "shell_command", "apply_patch"]
    );
    let capability_context = requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[capability granted]"))
        .expect("missing capability context");
    assert!(
        capability_context
            .content
            .contains("allowed_actions=say,request_capability,shell_command,apply_patch"),
        "{}",
        capability_context.content
    );
}

#[test]
/// Verifies Mezzanine `apply_patch` content remains accepted for
/// action planning.
///
/// A provider can request workspace-write capability and then emit the patch
/// block format that Codex commonly uses. The runner must plan the patch as a
/// shell-backed local action instead of sending repair feedback.
fn turn_runner_plans_codex_style_apply_patch_after_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
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
            raw_text: r#"{"rationale":"test action batch rationale","actions":[{"type":"apply_patch","patch":"*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"}]}"#
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
                    id: "patch-1".to_string(),
                    rationale: String::new(),
                    payload: AgentActionPayload::ApplyPatch {
                        patch:
                            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                                .to_string(),
                        strip: None,
                    },
                }],
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
                content: "edit a file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].action_type, "apply_patch");
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::ActionExecution
    );
}
