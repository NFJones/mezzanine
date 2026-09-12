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
                rationale: "test action batch rationale".to_string(),

                actions: vec![say_action(
                    "say-1",
                    "The provider failed before any action ran.",
                )],
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
            model_capabilities: Default::default(),
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
        mez_agent::ModelInteractionKind::MaapRepair
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .any(|message| message.content.contains("[MAAP repair state]")),
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

                rationale: "test action batch rationale".to_string(),


                actions: vec![abort_action("abort-1", "need more repository context")],

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

                rationale: "test action batch rationale".to_string(),


                actions: vec![capability_action("capability-1", AgentCapability::Shell)],

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

                rationale: "test action batch rationale".to_string(),


                actions: vec![say_action("say-1", "Ready.")],

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
            model_capabilities: Default::default(),
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
        mez_agent::ModelInteractionKind::MaapRepair
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
/// Verifies unsupported shell heredocs are repairable validation failures.
///
/// The direct action surface includes shell commands, so an unsupported heredoc
/// must trigger an append-only repair request. The corrected response retains
/// the validation facts as durable chronology.
fn turn_runner_repairs_shell_command_heredoc_validation_error() {
    let turn = turn();
    let mut heredoc = shell_action("shell-heredoc");
    if let AgentActionPayload::ShellCommand {
        command, summary, ..
    } = &mut heredoc.payload
    {
        *summary = "Write the prepared file".to_string();
        *command = "cat <<'EOF' > README.md\nupdated\nEOF".to_string();
    }
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid heredoc shell response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),

            actions: vec![heredoc],
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
            rationale: "test action batch rationale".to_string(),

            actions: vec![say_action(
                "say-1",
                "I will use a supported command instead.",
            )],
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Ok(invalid), Ok(corrected)]);
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            model_capabilities: Default::default(),
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
                placement: mez_agent::ContextPlacement::ConversationAppend,
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
    assert!(execution.request.messages.iter().any(|message| {
        message
            .content
            .contains(mez_agent::MAAP_REPAIR_EVIDENCE_PREFIX)
            && message.content.contains("heredoc redirection is disabled")
    }));
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("[MAAP repair state]"))
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let repair_message = &requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[MAAP repair state]"))
        .unwrap()
        .content;
    assert!(
        repair_message.contains("[MAAP repair state]"),
        "{repair_message}"
    );
    assert!(
        repair_message.contains("heredoc redirection is disabled"),
        "{repair_message}"
    );
}

#[test]
/// Verifies that an unavailable MCP action retains a safe repair fact through
/// the corrected durable request without retaining the malformed provider
/// response excerpt.
fn turn_runner_retries_maap_validation_error_with_safe_durable_repair_evidence() {
    let turn = turn();
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid unavailable mcp action".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),

            actions: vec![AgentAction {
                id: "mcp-1".to_string(),

                payload: AgentActionPayload::McpCall {
                    server: "missing".to_string(),
                    tool: "read".to_string(),
                    arguments_json: "{}".to_string(),
                },
            }],
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
            rationale: "test action batch rationale".to_string(),

            actions: vec![say_action("say-1", "I cannot access that MCP server.")],
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Ok(invalid), Ok(corrected)]);
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
            model_capabilities: Default::default(),
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
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "inspect missing mcp state".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.response.raw_text, "corrected say response");
    assert!(execution.request.messages.iter().any(|message| {
        message
            .content
            .contains(mez_agent::MAAP_REPAIR_EVIDENCE_PREFIX)
            && message.content.contains("unavailable server")
    }));
    assert!(execution.request.messages.iter().all(|message| {
        !message.content.contains("[MAAP repair state]")
            && !message.content.contains("invalid unavailable mcp action")
    }));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[MAAP repair state]"))
            .unwrap()
            .content
            .contains("[MAAP repair state]"),
        "{:?}",
        requests[1].messages
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[MAAP repair state]"))
            .unwrap()
            .content
            .contains("unavailable server"),
        "{:?}",
        requests[1].messages
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[MAAP repair state]"))
            .unwrap()
            .content
            .contains("request_capability_available="),
        "{:?}",
        requests[1].messages
    );
    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    assert!(entries.iter().all(|entry| {
        !entry
            .content
            .contains(mez_agent::MAAP_REPAIR_EVIDENCE_PREFIX)
            && !entry.content.contains("[MAAP repair state]")
            && !entry.content.contains("invalid unavailable mcp action")
    }));
}

#[test]
/// Verifies malformed apply-patch hunk syntax enters the normal bounded MAAP
/// repair loop before any local action is planned. The corrected replacement
/// must then become the sole planned file action, proving the rejected payload
/// cannot reach local dispatch or be replayed beside its replacement.
fn turn_runner_repairs_malformed_apply_patch_hunk_before_planning_replacement() {
    let turn = turn();
    let malformed = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "malformed patch hunk".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),
            actions: vec![AgentAction {
                id: "patch-malformed".to_string(),
                payload: AgentActionPayload::ApplyPatch {
                    patch: "*** Begin Patch\n*** Update File: note.txt\n@@\nnot a hunk line\n*** End Patch".to_string(),
                    strip: None,
                },
            }],
        }),
        provider_transcript_events: Vec::new(),
    };
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected patch replacement".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),
            actions: vec![AgentAction {
                id: "patch-corrected".to_string(),
                payload: AgentActionPayload::ApplyPatch {
                    patch: "*** Begin Patch\n*** Update File: note.txt\n@@ replace whole file\n+corrected\n*** End Patch".to_string(),
                    strip: None,
                },
            }],
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Ok(malformed), Ok(corrected)]);
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            model_capabilities: Default::default(),
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
                content: "replace the note".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].action_id, "patch-corrected");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let repair_message = requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[MAAP repair state]"))
        .unwrap();
    assert!(
        repair_message.content.lines().any(|line| {
            line == "validation_error=apply_patch: patch hunk lines must start with space, +, or -"
        }),
        "{}",
        repair_message.content
    );
}

#[test]
/// Verifies the bounded repair budget settles once when the provider keeps
/// returning malformed apply-patch hunks. The third rejected batch must finish
/// the turn through one failure summary rather than planning any invalid patch
/// or requesting a fourth repair.
fn turn_runner_settles_once_after_exhausted_malformed_apply_patch_hunk_repairs() {
    let turn = turn();
    let malformed = || {
        ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "malformed patch hunk".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),
            actions: vec![AgentAction {
                id: "patch-malformed".to_string(),
                payload: AgentActionPayload::ApplyPatch {
                    patch: "*** Begin Patch\n*** Update File: note.txt\n@@\nnot a hunk line\n*** End Patch".to_string(),
                    strip: None,
                },
            }],
        }),
        provider_transcript_events: Vec::new(),
    }
    };
    let summary = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "malformed patch hunk exhausted its repair budget".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),
            actions: vec![say_action(
                "say-summary",
                "The patch could not be validated.",
            )],
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![
        Ok(malformed()),
        Ok(malformed()),
        Ok(malformed()),
        Ok(summary),
    ]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            model_capabilities: Default::default(),
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
                content: "replace the note".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].action_id, "say-summary");
    assert!(
        execution
            .response
            .raw_text
            .contains("controller_failure_summary:")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].messages.iter().any(|message| {
        message
            .content
            .contains("patch hunk lines must start with space, +, or -")
    }));
    assert!(requests[2].messages.iter().any(|message| {
        message
            .content
            .contains("patch hunk lines must start with space, +, or -")
    }));
    assert_eq!(requests[3].allowed_actions.action_type_names(), vec!["say"]);
}

#[test]
/// Verifies an invalid apply-patch action rejects every sibling in the same
/// batch before planning. A valid shell sibling must not be dispatched or left
/// as a pending action while the replacement response is repaired.
fn turn_runner_does_not_plan_mixed_batch_siblings_of_malformed_apply_patch_hunk() {
    let turn = turn();
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid mixed patch batch".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),
            actions: vec![
                AgentAction {
                    id: "patch-malformed".to_string(),
                    payload: AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Update File: note.txt\n@@\nnot a hunk line\n*** End Patch".to_string(),
                        strip: None,
                    },
                },
                shell_action("shell-sibling"),
            ],
        }),
        provider_transcript_events: Vec::new(),
    };
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected response".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            rationale: "test action batch rationale".to_string(),
            actions: vec![shell_action("shell-corrected")],
        }),
        provider_transcript_events: Vec::new(),
    };
    let provider = SequencedProvider::new(vec![Ok(invalid), Ok(corrected)]);
    let policy =
        PermissionPolicy::default().with_approval_policy(mez_agent::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            model_capabilities: Default::default(),
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
    let mut executor = FakePaneShellExecutor::default();

    let execution = runner
        .run_turn_with_shell_executor(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "replace the note".to_string(),
            }])
            .unwrap(),
            Path::new("/bin/sh"),
            &mut executor,
            |_action| Ok(marker()),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].action_id, "shell-corrected");
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(executor.requests.len(), 1);
    assert_eq!(executor.requests[0].action_id, "shell-corrected");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::MaapRepair
    );
}
