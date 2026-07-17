//! Turn Runner tests for shell permissions behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies turn runner accepts allowed shell actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_accepts_allowed_shell_actions() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                actions: vec![shell_action("a1")],
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "where am I".to_string(),
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
    assert!(structured.contains(r#""execution_transport":"pending_local_dispatch""#));
    assert!(!structured.contains(r#""execution_transport":"pane_shell""#));
    assert!(structured.contains(r#""sent_to_pane":false"#));
    assert!(structured.contains(r#""terminal_observation":{"state":"pending_dispatch"}"#));
}

#[test]
/// Verifies that the turn planner accepts the common MAAP response for listing
/// the current directory. The runtime may only know the pane cwd at this point,
/// so `ls` without path arguments must not fail as an unknown-effect action.
fn turn_runner_accepts_ls_declared_as_current_directory_read() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                    id: "list-current-directory".to_string(),
                    rationale: "list files in the current directory".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "List files in the current directory".to_string(),
                        command: "ls".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(1000),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let path_scopes = PathScopes::unresolved("/repo", Vec::new(), Vec::new());
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
            &policy,
            &approvals,
            Some(&path_scopes),
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "list the files in the current directory".to_string(),
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
    assert!(structured.contains(r#""state":"pending_dispatch""#));
    assert!(structured.contains(r#""command":"ls""#), "{structured}");
}

#[test]
/// Verifies that auto-allow uses the model rationale as its reasonableness
/// assessment. The reduced MAAP shape no longer carries a separate approval
/// hint field.
fn turn_runner_auto_allows_prompted_shell_actions_from_rationale() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                    id: "a1".to_string(),
                    rationale: "run command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Run the requested command".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "check changes".to_string(),
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
/// Verifies turn runner blocks shell actions requiring approval.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_blocks_shell_actions_requiring_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                    id: "a1".to_string(),
                    rationale: "run command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Run the requested command".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "check changes".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"state\":\"pending\"")
    );
}

#[test]
/// Verifies that the turn planner consumes shell-resolved path scopes when
/// deciding whether a shell action may auto-run. A command whose canonical path
/// escapes the active read scope must become a blocked approval request rather
/// than a running pane write.
fn turn_runner_blocks_shell_actions_with_canonical_scope_escape() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                    id: "a1".to_string(),
                    rationale: "read file".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Read the requested file".to_string(),
                        command: "cat link/secret.txt".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let scopes = PathScopes::shell_resolved("/repo", vec!["/repo".to_string()], Vec::new())
        .with_canonical_path("link/secret.txt", "/outside/secret.txt");
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
            &policy,
            &approvals,
            Some(&scopes),
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
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
            .contains(r#""state":"pending""#)
    );
}

#[test]
/// Verifies that an unknown shell command is routed through approval policy
/// without relying on provider-declared or provider-visible effect metadata.
/// The safe behavior is a pending approval in `ask` mode.
fn turn_runner_blocks_unknown_classified_shell_actions_without_declared_effect_failure() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                    id: "a1".to_string(),
                    rationale: "inspect with a short interpreter command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect with a short interpreter command".to_string(),
                        command: "python3 -c 'print(1)'".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "run script".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"pending_approval""#));
    assert!(
        structured.contains(r#""command":"python3 -c 'print(1)'""#),
        "{structured}"
    );
}

#[test]
/// Verifies turn runner executes allowed shell actions and records output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_executes_allowed_shell_actions_and_records_output() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                actions: vec![shell_action("a1")],
                final_turn: true,
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
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            signal: None,
            stdout: framed_shell_output("/repo\n"),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let execution = runner
        .run_turn_with_shell_executor(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "where am I".to_string(),
            }])
            .unwrap(),
            Path::new("/bin/sh"),
            &mut executor,
            |_action| Ok(marker()),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(execution.action_results[0].content_texts(), vec!["/repo\n"]);
    assert_eq!(executor.requests.len(), 1);
    assert_eq!(executor.requests[0].action_id, "a1");
}

#[test]
/// Verifies full-access sessions still enforce subagent write scopes.
///
/// A delegated subagent that emits `apply_patch` must remain within its declared
/// writable paths even when normal approval policy is FullAccess. This protects
/// native local execution from bypassing scope checks during planning.
fn turn_runner_full_access_denies_out_of_scope_subagent_apply_patch() {
    let mut turn = turn();
    turn.agent_id = "agent-%2".to_string();
    turn.pane_id = "%2".to_string();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "patch action".to_string(),
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
                    id: "a1".to_string(),
                    rationale: "patch out-of-scope file".to_string(),
                    payload: AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                            .to_string(),
                        strip: None,
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
    let subagent_scope = mez_agent::SubagentScopeDeclaration {
        cooperation_mode: mez_agent::CooperationMode::OwnedWrite,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["/repo/src/lib.rs".to_string()],
        write_scopes: vec!["/repo/docs".to_string()],
        permission_preset: None,
    };
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
        subagent_scope: Some(&subagent_scope),
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "patch a file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Denied);
    assert_eq!(
        execution.action_results[0].error.as_ref().unwrap().code,
        "subagent_scope_violation"
    );
}

#[test]
/// Verifies full-access sessions still enforce subagent read scopes.
///
/// Full-access mode can bypass ordinary approval prompts, but it must not widen
/// a delegated subagent beyond the parent-declared scope. Concrete read escapes
/// therefore become hard denials before local dispatch even under FullAccess.
fn turn_runner_full_access_denies_out_of_scope_subagent_shell_command() {
    let mut turn = turn();
    turn.agent_id = "agent-%2".to_string();
    turn.pane_id = "%2".to_string();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "read action".to_string(),
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
                    id: "a1".to_string(),
                    rationale: "inspect local instructions".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect local instructions".to_string(),
                        command: "cat AGENTS.md".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
    let subagent_scope = mez_agent::SubagentScopeDeclaration {
        cooperation_mode: mez_agent::CooperationMode::ExploreOnly,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["/elsewhere".to_string()],
        write_scopes: Vec::new(),
        permission_preset: None,
    };
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
        subagent_scope: Some(&subagent_scope),
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "summarize local instructions".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Denied);
    assert_eq!(
        execution.action_results[0].error.as_ref().unwrap().code,
        "subagent_scope_violation"
    );
}

#[test]
/// Verifies turn runner keeps final shell action running until observed.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_keeps_final_shell_action_running_until_observed() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                actions: vec![shell_action("a1")],
                final_turn: true,
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "where am I".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
}

#[test]
/// Verifies turn runner routes shell actions through approval policy without model effects.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_runner_routes_shell_actions_through_approval_policy_without_model_effects() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                    id: "a1".to_string(),
                    rationale: "inspect environment variables".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect environment variables".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "inspect environment".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
}

#[test]
/// Verifies subagent scope checks do not convert unknown shell effects into a
/// hard denial before approval policy runs. Broad interpreter commands still
/// need approval in ask mode, but full-access sessions should be able to run
/// read-only discovery scripts through the normal permission model.
fn turn_runner_routes_subagent_unknown_shell_actions_through_approval_policy() {
    let mut turn = turn();
    turn.agent_id = "agent-%2".to_string();
    turn.pane_id = "%2".to_string();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "script action".to_string(),
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
                    id: "a1".to_string(),
                    rationale: "inspect repository metadata with a read-only script".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect repository metadata with a read-only script".to_string(),
                        command: "python3 -c 'print(\"metadata\")'".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
    let subagent_scope = mez_agent::SubagentScopeDeclaration {
        cooperation_mode: mez_agent::CooperationMode::ExploreOnly,
        current_directory: "/home/neil".to_string(),
        read_scopes: vec![
            "/home/neil/.codex".to_string(),
            "/home/neil/.cargo".to_string(),
        ],
        write_scopes: Vec::new(),
        permission_preset: None,
    };
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
        subagent_scope: Some(&subagent_scope),
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "search local repositories".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
}

#[test]
/// Verifies that auto-allow only advances a prompted shell action when the
/// model supplies the explicit approval hint and rationale required for the
/// active request.
fn turn_runner_runs_prompted_shell_actions_with_auto_allow_assertion() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
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
                    id: "a1".to_string(),
                    rationale: "run command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Run the requested command".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "check changes".to_string(),
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
