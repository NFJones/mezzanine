//! Agent shell state tests.

use super::*;

/// Builds one concrete pane environment identity for path-resolution tests.
fn path_resolution_environment(working_directory: &Path) -> mez_agent::EnvironmentSignature {
    mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "test-host",
        "test-user",
        "/bin/sh",
        mez_agent::ShellClassification::PosixSh,
        None,
        Some("/usr/bin:/bin".to_string()),
        working_directory.to_string_lossy(),
        None,
        false,
        None,
        Vec::new(),
    )
    .unwrap()
}

/// Builds a running turn identity for action-specific path-resolution tests.
fn path_resolution_turn() -> mez_agent::AgentTurnRecord {
    mez_agent::AgentTurnRecord {
        turn_id: "path-resolution-turn".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    }
}

/// Builds one permission evaluation with caller-supplied filesystem effects.
fn path_resolution_evaluation(
    completeness: mez_agent::permissions::EffectCompleteness,
    effects: mez_agent::permissions::EffectiveCommandEffects,
) -> mez_agent::permissions::PermissionEvaluation {
    mez_agent::permissions::PermissionEvaluation {
        decision: RuleDecision::Allow,
        candidates: vec![mez_agent::permissions::CandidateEvaluation {
            command: "test command".to_string(),
            decision: RuleDecision::Allow,
            matched_rule_ids: vec!["test-rule".to_string()],
            effects: effects.clone(),
            completeness,
        }],
        matched_rule_ids: vec!["test-rule".to_string()],
        effects,
        completeness,
    }
}

/// Builds empty non-filesystem effects for path-resolution tests.
fn path_resolution_effects() -> mez_agent::permissions::EffectiveCommandEffects {
    mez_agent::permissions::EffectiveCommandEffects {
        reads: Vec::new(),
        writes: Vec::new(),
        creates: Vec::new(),
        deletes: Vec::new(),
        touches: Vec::new(),
        network: false,
        credentials: false,
        process_control: false,
        destructive: false,
        privilege_change: false,
        unknown: false,
    }
}

/// Builds one shell action for focused audit-record tests.
fn sandbox_audit_action() -> mez_agent::AgentAction {
    mez_agent::AgentAction {
        id: "sandbox-audit-action".to_string(),
        rationale: "exercise sandbox audit metadata".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect a protected fixture".to_string(),
            command: "cat /private/workspace/secret.txt".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    }
}

/// Verifies policy-only shell audit records identify their backend without
/// inventing Bubblewrap plan metadata or retaining command content.
#[test]
fn runtime_policy_only_shell_audit_omits_plan_metadata() {
    let root = temp_root("runtime-policy-only-sandbox-audit");
    let audit_path = root.join("audit.jsonl");
    let mut service = test_runtime_service();
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let turn = path_resolution_turn();
    let action = sandbox_audit_action();

    service
        .append_agent_shell_command_audit(
            &turn,
            &action,
            "cat /private/workspace/secret.txt",
            None,
            None,
            "sent",
        )
        .unwrap();

    let record: serde_json::Value =
        serde_json::from_str(fs::read_to_string(&audit_path).unwrap().trim()).unwrap();
    let metadata = record["metadata"].as_object().unwrap();
    assert_eq!(metadata["sandbox_backend"], "policy-only");
    assert!(!metadata.contains_key("sandbox_plan_sha256"));
    assert!(!metadata.contains_key("sandbox_authority_source"));
    assert!(
        !fs::read_to_string(&audit_path)
            .unwrap()
            .contains("/private/workspace/secret.txt")
    );
    fs::remove_dir_all(root).unwrap();
}

/// Verifies Bubblewrap shell audit records retain only the compiler's redacted
/// profile identity, authority source, counts, and deterministic plan digest.
#[test]
fn runtime_bubblewrap_shell_audit_records_redacted_plan_metadata() {
    let root = temp_root("runtime-bubblewrap-sandbox-audit");
    let audit_path = root.join("audit.jsonl");
    let mut service = test_runtime_service();
    configure_path_resolution_bubblewrap(&mut service);
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: true,
        required: true,
    }));
    let turn = path_resolution_turn();
    let action = sandbox_audit_action();
    let plan_sha256 = "a".repeat(64);
    let summary = crate::security::sandbox::SandboxAuditSummary {
        runtime_profile_version: crate::security::sandbox::BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
        authority_source: crate::security::sandbox::SandboxAuthoritySource::Narrowed,
        read_only_mount_count: 2,
        read_write_mount_count: 1,
        protected_mask_count: 6,
        plan_sha256: plan_sha256.clone(),
    };

    service
        .append_agent_shell_command_audit(
            &turn,
            &action,
            "cat /private/workspace/secret.txt",
            None,
            Some(&summary),
            "sent",
        )
        .unwrap();

    let serialized = fs::read_to_string(&audit_path).unwrap();
    let record: serde_json::Value = serde_json::from_str(serialized.trim()).unwrap();
    let metadata = record["metadata"].as_object().unwrap();
    assert_eq!(metadata["sandbox_backend"], "bubblewrap");
    assert_eq!(
        metadata["sandbox_profile_version"],
        crate::security::sandbox::BUBBLEWRAP_RUNTIME_PROFILE_VERSION
    );
    assert_eq!(metadata["sandbox_authority_source"], "narrowed");
    assert_eq!(metadata["sandbox_read_only_mount_count"], "2");
    assert_eq!(metadata["sandbox_read_write_mount_count"], "1");
    assert_eq!(metadata["sandbox_protected_mask_count"], "6");
    assert_eq!(metadata["sandbox_plan_sha256"], plan_sha256);
    assert!(!serialized.contains("/private/workspace/secret.txt"));
    assert!(!serialized.contains("--ro-bind"));
    fs::remove_dir_all(root).unwrap();
}

/// Builds shell-dispatch and retry-result audit records for one approved
/// Bubblewrap fallback classification.
fn sandbox_fallback_audit_records(
    reason: &str,
    proof: &str,
    partial_effect_warning: bool,
) -> (Vec<serde_json::Value>, String) {
    let root = temp_root(&format!("runtime-sandbox-fallback-audit-{reason}"));
    let audit_path = root.join("audit.jsonl");
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    assert!(
        service
            .offer_sandbox_fallback_approval(
                "sandbox-audit-marker",
                &turn_id,
                &action_id,
                reason,
                proof,
                partial_effect_warning,
            )
            .unwrap()
    );
    service.grant_sandbox_bypass_after_approval(&turn_id, &action_id);
    assert!(service.activate_sandbox_bypass_after_approval(&turn_id, &action_id));
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    let execution = service.agent_turn_executions().get(&turn_id).unwrap();
    let action = execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .first()
        .unwrap()
        .clone();
    let evaluation = execution.action_results[0]
        .permission_evaluation
        .clone()
        .unwrap();

    service
        .append_agent_shell_command_audit(&turn, &action, "env", Some(&evaluation), None, "sent")
        .unwrap();
    service
        .append_sandbox_fallback_result_audit(&turn_id, &action_id, "succeeded")
        .unwrap();

    let serialized = fs::read_to_string(&audit_path).unwrap();
    let records = serialized
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    fs::remove_dir_all(root).unwrap();
    (records, serialized)
}

/// Verifies audit records distinguish proven and model-assessed fallbacks,
/// retain retry outcomes, and hash rather than disclose fallback proof text.
#[test]
fn runtime_sandbox_fallback_audit_distinguishes_classification_and_result() {
    let proven_proof = "trusted status closed without an exit-code event";
    let (proven, proven_serialized) =
        sandbox_fallback_audit_records("pre_payload_failure", proven_proof, false);
    let model_proof = "model confidence=0.930: minimal environment removed a variable";
    let (model, model_serialized) =
        sandbox_fallback_audit_records("model_assessed_sandbox_failure", model_proof, true);

    for (records, reason, partial_effect_warning) in [
        (&proven, "pre_payload_failure", "false"),
        (&model, "model_assessed_sandbox_failure", "true"),
    ] {
        let dispatch = records
            .iter()
            .find(|record| record["action"] == "send_to_pane")
            .expect("fallback dispatch audit should be present");
        let result = records
            .iter()
            .find(|record| record["action"] == "sandbox_fallback_result")
            .expect("fallback result audit should be present");
        assert_eq!(dispatch["metadata"]["sandbox_backend"], "policy-only");
        assert_eq!(dispatch["metadata"]["sandbox_fallback_reason"], reason);
        assert_eq!(
            dispatch["metadata"]["sandbox_fallback_partial_effect_warning"],
            partial_effect_warning
        );
        assert_eq!(dispatch["approval_state"], "approved_exact_sandbox_bypass");
        assert_eq!(result["outcome"], "succeeded");
        assert_eq!(result["metadata"]["sandbox_fallback_reason"], reason);
    }
    assert!(!proven_serialized.contains(proven_proof));
    assert!(!model_serialized.contains(model_proof));
    assert!(!proven_serialized.contains("\"env\""));
    assert!(!model_serialized.contains("\"env\""));
}

/// Configures Bubblewrap with one project root as maximum read/write authority.
fn configure_path_resolution_bubblewrap(service: &mut RuntimeSessionService) {
    let configured =
        crate::runtime::config::runtime_configured_permissions_from_config(&serde_json::json!({
            "permissions": {
                "sandbox": "bubblewrap",
                "read_scopes": ["."],
                "write_scopes": ["."],
                "network_policy": "deny",
                "bubblewrap": {
                    "executable": "/usr/bin/bwrap",
                    "unavailable": "fail",
                    "network": "isolated",
                    "environment": "minimal"
                }
            }
        }))
        .unwrap();
    service
        .integration
        .replace_configured_permissions(configured);
}

/// Builds a ready pane with Bubblewrap configured for capability-probe tests.
fn bubblewrap_probe_service() -> RuntimeSessionService {
    let root = temp_root("runtime-bubblewrap-probe");
    fs::create_dir_all(&root).unwrap();
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    configure_path_resolution_bubblewrap(&mut service);
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    mark_test_pane_ready(&mut service, "%1");
    service
}

/// Builds one prompting shell evaluation retained by sandbox-first dispatch.
fn sandbox_fallback_prompt_evaluation() -> mez_agent::permissions::PermissionEvaluation {
    let effects = path_resolution_effects();
    mez_agent::permissions::PermissionEvaluation {
        decision: RuleDecision::Prompt,
        candidates: vec![mez_agent::permissions::CandidateEvaluation {
            command: "env".to_string(),
            decision: RuleDecision::Prompt,
            matched_rule_ids: vec!["sandbox-fallback-prompt".to_string()],
            effects: effects.clone(),
            completeness: mez_agent::permissions::EffectCompleteness::Complete,
        }],
        matched_rule_ids: vec!["sandbox-fallback-prompt".to_string()],
        effects,
        completeness: mez_agent::permissions::EffectCompleteness::Complete,
    }
}

/// Builds a live prompt turn whose sole shell action is awaiting sandbox
/// settlement.
fn sandbox_fallback_execution_service() -> (RuntimeSessionService, String, String) {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    let started = service
        .start_agent_prompt_turn("%1", "show the environment")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action_id = "sandbox-fallback-shell".to_string();
    let action = mez_agent::AgentAction {
        id: action_id.clone(),
        rationale: "inspect the environment".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the environment".to_string(),
            command: "env".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut result = mez_agent::ActionResult::running(
        &turn,
        &action,
        vec!["local action accepted for sandbox-first dispatch".to_string()],
        None,
    );
    result.permission_evaluation = Some(Box::new(sandbox_fallback_prompt_evaluation()));
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run env".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "exercise sandbox fallback".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![result],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    (service, turn.turn_id, action_id)
}

/// Adds one additional sandbox-first shell action to a running test turn so a
/// shared capability probe can exercise terminal settlement for every waiter.
fn add_sandbox_fallback_probe_waiter(
    service: &mut RuntimeSessionService,
    turn_id: &str,
    action_id: &str,
) {
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    let mut action = service
        .agent_turn_executions()
        .get(turn_id)
        .and_then(|execution| execution.response.action_batch.as_ref())
        .and_then(|batch| batch.actions.first())
        .cloned()
        .unwrap();
    action.id = action_id.to_string();
    let mut result = mez_agent::ActionResult::running(
        &turn,
        &action,
        vec!["local action accepted for sandbox-first dispatch".to_string()],
        None,
    );
    result.permission_evaluation = Some(Box::new(sandbox_fallback_prompt_evaluation()));
    let execution = service
        .agent_turn_executions_mut()
        .get_mut(turn_id)
        .unwrap();
    execution
        .response
        .action_batch
        .as_mut()
        .unwrap()
        .actions
        .push(action);
    execution.action_results.push(result);
}

/// Primes one sandbox-first action through a successful capability probe so
/// workload preparation can exercise typed compiler failures without running
/// a real Bubblewrap process.
fn settle_bubblewrap_probe_for_preparation_test(
    service: &mut RuntimeSessionService,
    turn_id: &str,
    action_id: &str,
    root: &Path,
) {
    configure_path_resolution_bubblewrap(service);
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(root));
    cache_path_resolution_maximum(service, root);
    mark_test_pane_ready(service, "%1");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, action_id)
            .unwrap()
    );
    let (marker, mut transaction) = take_bubblewrap_probe_transaction(service);
    let RunningShellTransactionKind::BubblewrapCapabilityProbe { probe_plan, .. } =
        &transaction.kind
    else {
        unreachable!();
    };
    transaction.observed_output_preview = probe_plan.expected_stdout.to_string();
    service
        .observe_bubblewrap_capability_probe_transaction_end(&marker, transaction, 0)
        .unwrap();
}

/// Verifies trusted evidence that Bubblewrap closed without an exit-code event
/// creates one normal approval for an exact unsandboxed retry while retaining
/// the original prompt evaluation.
#[test]
fn runtime_bubblewrap_pre_payload_failure_offers_exact_fallback_approval() {
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();

    assert!(
        service
            .offer_sandbox_pre_payload_fallback_approval(
                "sandbox-marker",
                &turn_id,
                &action_id,
                "trusted_status_closed_without_exit_code",
            )
            .unwrap()
    );

    let execution = service.agent_turn_executions().get(&turn_id).unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    assert_eq!(
        execution.action_results[0]
            .permission_evaluation
            .as_ref()
            .map(|evaluation| evaluation.decision),
        Some(RuleDecision::Prompt)
    );
    let approval_ids = service.blocked_agent_approval_ids_by_turn();
    let approval_id = approval_ids.get(&turn_id).unwrap().first().unwrap();
    assert!(service.blocked_approval_grants_sandbox_bypass_for_tests(approval_id));
    assert!(
        service
            .blocked_approvals()
            .get(approval_id)
            .unwrap()
            .action_summary
            .contains("env")
    );
}

/// Verifies an unsupported Bubblewrap workload requirement is preserved as a
/// typed preparation failure and offers one exact approval without launching.
#[test]
fn runtime_bubblewrap_unsupported_preparation_offers_exact_fallback_approval() {
    let root = temp_root("runtime-bubblewrap-preparation-fallback");
    fs::create_dir_all(&root).unwrap();
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    let execution = service
        .agent_turn_executions_mut()
        .get_mut(&turn_id)
        .unwrap();
    let action = execution
        .response
        .action_batch
        .as_mut()
        .unwrap()
        .actions
        .first_mut()
        .unwrap();
    let mez_agent::AgentActionPayload::ShellCommand { interactive, .. } = &mut action.payload
    else {
        unreachable!();
    };
    *interactive = true;

    settle_bubblewrap_probe_for_preparation_test(&mut service, &turn_id, &action_id, &root);

    let execution = service.agent_turn_executions().get(&turn_id).unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""reason":"preparation_failure""#));
    assert!(structured.contains("unsupported_requirement"));
    assert!(service.running_shell_transactions_for_tests().is_empty());
    let approval_id = service
        .blocked_agent_approval_ids_by_turn()
        .get(&turn_id)
        .unwrap()
        .first()
        .unwrap()
        .clone();
    assert!(service.blocked_approval_grants_sandbox_bypass_for_tests(&approval_id));
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a policy-denied network requirement remains terminal and cannot
/// be converted into an approval-gated unsandboxed retry.
#[test]
fn runtime_bubblewrap_hard_preparation_failure_does_not_offer_fallback() {
    let root = temp_root("runtime-bubblewrap-hard-preparation-failure");
    fs::create_dir_all(&root).unwrap();
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    let evaluation = service
        .agent_turn_executions_mut()
        .get_mut(&turn_id)
        .unwrap()
        .action_results[0]
        .permission_evaluation
        .as_mut()
        .unwrap();
    evaluation.effects.network = true;
    evaluation.candidates[0].effects.network = true;

    settle_bubblewrap_probe_for_preparation_test(&mut service, &turn_id, &action_id, &root);

    assert!(service.blocked_approvals().pending().is_empty());
    assert!(service.agent_turn_executions().get(&turn_id).is_none());
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(service.running_shell_transactions_for_tests().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Verifies an approved sandbox bypass is scoped to one exact turn/action,
/// remains active across internal phases, and cannot survive settlement.
#[test]
fn runtime_sandbox_fallback_bypass_is_exact_and_cleared_on_settlement() {
    let mut service = test_runtime_service();
    service.grant_sandbox_bypass_after_approval("turn-1", "action-1");

    assert!(service.activate_sandbox_bypass_after_approval("turn-1", "action-1"));
    assert!(service.activate_sandbox_bypass_after_approval("turn-1", "action-1"));
    assert!(!service.activate_sandbox_bypass_after_approval("turn-1", "action-2"));
    assert!(!service.activate_sandbox_bypass_after_approval("turn-2", "action-1"));

    service.clear_sandbox_bypass_for_action("turn-1", "action-1");
    assert!(!service.activate_sandbox_bypass_after_approval("turn-1", "action-1"));
}

/// Builds one settled Bubblewrap payload transaction for assessment tests.
fn sandbox_failure_transaction(turn_id: &str, action_id: &str) -> RunningShellTransactionRef {
    RunningShellTransactionRef {
        turn_id: turn_id.to_string(),
        kind: RunningShellTransactionKind::AgentAction {
            action_id: action_id.to_string(),
        },
        pane_id: "%1".to_string(),
        command: "env".to_string(),
        started_at_unix_ms: 0,
        timeout_ms: None,
        pending_input_payload: None,
        observed_output_bytes: 18,
        observed_output_preview: "permission denied\n".to_string(),
        observed_output_truncated: false,
    }
}

/// Verifies a valid model-attributed sandbox failure creates one approval and
/// warns that the already-executed payload may have produced partial effects.
#[test]
fn runtime_sandbox_failure_assessment_offers_warned_fallback_approval() {
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    assert!(
        service
            .queue_sandbox_failure_assessment(
                &turn,
                &action_id,
                "sandbox-assessment-marker",
                sandbox_failure_transaction(&turn_id, &action_id),
                1,
            )
            .unwrap()
    );
    let request = service
        .sandbox_failure_assessment_request_for_tests(&turn_id)
        .unwrap();
    assert_eq!(
        request.interaction_kind,
        mez_agent::ModelInteractionKind::SandboxFailureAssessment
    );
    assert!(request.messages.iter().all(|message| {
        !message.content.contains("show the environment")
            && !message.content.contains("sandbox-fallback-shell")
    }));
    let response = mez_agent::ModelResponse {
        provider: "runtime-batch".to_string(),
        model: "test".to_string(),
        raw_text: r#"{"version":1,"class":"sandbox_failure","confidence":0.93,"rationale":"the fixed minimal environment likely removed a required variable","retry_requested":true}"#.to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    };

    service
        .apply_sandbox_failure_assessment_provider_response(&turn, &response)
        .unwrap();

    let execution = service.agent_turn_executions().get(&turn_id).unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""reason":"model_assessed_sandbox_failure""#));
    assert!(structured.contains(r#""partial_effect_warning":true"#));
    assert_eq!(
        service
            .blocked_agent_approval_ids_by_turn()
            .get(&turn_id)
            .map(Vec::len),
        Some(1)
    );
}

/// Verifies command-failure or uncertain assessments never create authority
/// and instead settle the original non-zero shell result normally.
#[test]
fn runtime_command_failure_assessment_does_not_offer_unsandboxed_retry() {
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    let execution = service
        .agent_turn_executions()
        .get(&turn_id)
        .cloned()
        .unwrap();
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service
        .queue_sandbox_failure_assessment(
            &turn,
            &action_id,
            "sandbox-command-failure-marker",
            sandbox_failure_transaction(&turn_id, &action_id),
            1,
        )
        .unwrap();
    let response = mez_agent::ModelResponse {
        provider: "runtime-batch".to_string(),
        model: "test".to_string(),
        raw_text: r#"{"version":1,"class":"command_failure","confidence":0.88,"rationale":"the command reported its own invalid input","retry_requested":false}"#.to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    };

    service
        .apply_sandbox_failure_assessment_provider_response(&turn, &response)
        .unwrap();

    assert!(service.blocked_approvals().pending().is_empty());
    assert!(
        !service
            .blocked_agent_approval_ids_by_turn()
            .contains_key(&turn_id)
    );
    assert_eq!(
        service
            .agent_turn_executions()
            .get(&turn_id)
            .unwrap()
            .action_results[0]
            .status,
        ActionStatus::Succeeded
    );
}

/// Verifies malformed assessment output cannot create retry authority and
/// settles the original command result through the ordinary shell path.
#[test]
fn runtime_malformed_sandbox_assessment_does_not_offer_unsandboxed_retry() {
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    let execution = service
        .agent_turn_executions()
        .get(&turn_id)
        .cloned()
        .unwrap();
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service
        .queue_sandbox_failure_assessment(
            &turn,
            &action_id,
            "sandbox-malformed-assessment-marker",
            sandbox_failure_transaction(&turn_id, &action_id),
            1,
        )
        .unwrap();
    let response = mez_agent::ModelResponse {
        provider: "runtime-batch".to_string(),
        model: "test".to_string(),
        raw_text: "not valid assessment json".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    };

    service
        .apply_sandbox_failure_assessment_provider_response(&turn, &response)
        .unwrap();

    assert!(service.blocked_approvals().pending().is_empty());
    assert!(
        !service
            .blocked_agent_approval_ids_by_turn()
            .contains_key(&turn_id)
    );
    assert!(
        service
            .sandbox_failure_assessment_request_for_tests(&turn_id)
            .is_none()
    );
}

/// Verifies provider failure or timeout during assessment cannot create retry
/// authority and instead settles the retained command result normally.
#[test]
fn runtime_failed_sandbox_assessment_does_not_offer_unsandboxed_retry() {
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();
    let execution = service
        .agent_turn_executions()
        .get(&turn_id)
        .cloned()
        .unwrap();
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service
        .queue_sandbox_failure_assessment(
            &turn,
            &action_id,
            "sandbox-provider-failure-marker",
            sandbox_failure_transaction(&turn_id, &action_id),
            1,
        )
        .unwrap();

    assert!(
        service
            .settle_pending_sandbox_failure_assessment(&turn_id, "provider_timeout")
            .unwrap()
    );

    assert!(service.blocked_approvals().pending().is_empty());
    assert!(
        !service
            .blocked_agent_approval_ids_by_turn()
            .contains_key(&turn_id)
    );
    assert!(
        service
            .sandbox_failure_assessment_request_for_tests(&turn_id)
            .is_none()
    );
}

/// Removes and returns the sole in-flight Bubblewrap capability probe.
fn take_bubblewrap_probe_transaction(
    service: &mut RuntimeSessionService,
) -> (String, RunningShellTransactionRef) {
    let marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::BubblewrapCapabilityProbe { .. }
            )
            .then(|| marker.clone())
        })
        .unwrap();
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .remove(&marker)
        .unwrap();
    (marker, transaction)
}

/// Verifies one failed probe settles only its waiting action and a later
/// independent action sends a fresh probe for the same capability identity.
#[test]
fn runtime_bubblewrap_probe_failure_allows_later_reprobe() {
    let mut service = bubblewrap_probe_service();
    let turn = path_resolution_turn();
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "action-1")
            .unwrap()
    );
    let (marker, transaction) = take_bubblewrap_probe_transaction(&mut service);

    service
        .fail_bubblewrap_capability_probe_transaction(
            &marker,
            transaction,
            "bubblewrap_probe_failed",
            "transient probe failure",
            false,
            false,
        )
        .unwrap();

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "action-2")
            .unwrap()
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| matches!(
                &transaction.kind,
                RunningShellTransactionKind::BubblewrapCapabilityProbe { action_id, .. }
                    if action_id == "action-2"
            ))
    );
}

/// Verifies concurrent waiters share one exact in-flight capability probe and
/// may both continue once that probe has populated the capability cache.
#[test]
fn runtime_bubblewrap_probe_deduplicates_in_flight_requests() {
    let mut service = bubblewrap_probe_service();
    let turn = path_resolution_turn();

    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "action-1")
            .unwrap()
    );
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "action-2")
            .unwrap()
    );
    assert_eq!(service.running_shell_transactions_for_tests().len(), 1);
    let waiters = service
        .running_shell_transactions_for_tests()
        .values()
        .find_map(|transaction| match &transaction.kind {
            RunningShellTransactionKind::BubblewrapCapabilityProbe { waiters, .. } => {
                Some(waiters.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        waiters,
        vec![
            ("path-resolution-turn".to_string(), "action-1".to_string()),
            ("path-resolution-turn".to_string(), "action-2".to_string()),
        ]
    );
    let (marker, mut transaction) = take_bubblewrap_probe_transaction(&mut service);
    let RunningShellTransactionKind::BubblewrapCapabilityProbe { probe_plan, .. } =
        &transaction.kind
    else {
        unreachable!();
    };
    transaction.observed_output_preview = probe_plan.expected_stdout.to_string();
    service
        .observe_bubblewrap_capability_probe_transaction_end(&marker, transaction, 0)
        .unwrap();
    assert!(
        service
            .ensure_bubblewrap_capability_for_action(&turn, "action-1")
            .unwrap()
    );
    assert!(
        service
            .ensure_bubblewrap_capability_for_action(&turn, "action-2")
            .unwrap()
    );
}

/// Verifies a failed shared capability probe settles every waiting action
/// rather than leaving later actions in the running state indefinitely.
#[test]
fn runtime_bubblewrap_probe_failure_settles_all_waiters() {
    let root = temp_root("runtime-bubblewrap-probe-failure-waiters");
    fs::create_dir_all(&root).unwrap();
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    configure_path_resolution_bubblewrap(&mut service);
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    mark_test_pane_ready(&mut service, "%1");
    add_sandbox_fallback_probe_waiter(&mut service, &turn_id, "second-waiter");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();

    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, &action_id)
            .unwrap()
    );
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "second-waiter")
            .unwrap()
    );
    let (marker, transaction) = take_bubblewrap_probe_transaction(&mut service);
    service
        .fail_bubblewrap_capability_probe_transaction(
            &marker,
            transaction,
            "bubblewrap_probe_failed",
            "probe failed",
            false,
            false,
        )
        .unwrap();

    let execution = service.agent_turn_executions().get(&turn_id).unwrap();
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.status != ActionStatus::Running)
    );
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a timed-out shared capability probe settles every waiting action
/// rather than leaving later actions in the running state indefinitely.
#[test]
fn runtime_bubblewrap_probe_timeout_settles_all_waiters() {
    let root = temp_root("runtime-bubblewrap-probe-timeout-waiters");
    fs::create_dir_all(&root).unwrap();
    let (mut service, turn_id, action_id) = sandbox_fallback_execution_service();
    configure_path_resolution_bubblewrap(&mut service);
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    mark_test_pane_ready(&mut service, "%1");
    add_sandbox_fallback_probe_waiter(&mut service, &turn_id, "second-waiter");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .unwrap();

    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, &action_id)
            .unwrap()
    );
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "second-waiter")
            .unwrap()
    );
    let (marker, transaction) = take_bubblewrap_probe_transaction(&mut service);
    service
        .fail_bubblewrap_capability_probe_transaction(
            &marker,
            transaction,
            "bubblewrap_probe_timeout",
            "probe timed out",
            true,
            true,
        )
        .unwrap();

    let execution = service.agent_turn_executions().get(&turn_id).unwrap();
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.status != ActionStatus::Running)
    );
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a successful probe remains cached and suppresses later probes for
/// the same pane environment and runtime-profile identity.
#[test]
fn runtime_bubblewrap_successful_probe_is_cached() {
    let mut service = bubblewrap_probe_service();
    let turn = path_resolution_turn();
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "action-1")
            .unwrap()
    );
    let (marker, mut transaction) = take_bubblewrap_probe_transaction(&mut service);
    let RunningShellTransactionKind::BubblewrapCapabilityProbe { probe_plan, .. } =
        &transaction.kind
    else {
        unreachable!();
    };
    transaction.observed_output_preview = probe_plan.expected_stdout.to_string();

    service
        .observe_bubblewrap_capability_probe_transaction_end(&marker, transaction, 0)
        .unwrap();

    assert!(
        service
            .ensure_bubblewrap_capability_for_action(&turn, "action-2")
            .unwrap()
    );
    assert!(service.running_shell_transactions_for_tests().is_empty());
}

/// Verifies a timed-out probe leaves no durable negative entry, while pane
/// readiness recovery remains an explicit prerequisite for a later reprobe.
#[test]
fn runtime_bubblewrap_probe_timeout_allows_reprobe_after_readiness_recovery() {
    let mut service = bubblewrap_probe_service();
    let turn = path_resolution_turn();
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "action-1")
            .unwrap()
    );
    let (marker, transaction) = take_bubblewrap_probe_transaction(&mut service);

    service
        .fail_bubblewrap_capability_probe_transaction(
            &marker,
            transaction,
            "bubblewrap_probe_timeout",
            "transient probe timeout",
            true,
            true,
        )
        .unwrap();
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Degraded
    );

    mark_test_pane_ready(&mut service, "%1");
    assert!(
        !service
            .ensure_bubblewrap_capability_for_action(&turn, "action-2")
            .unwrap()
    );
}

/// Resolves and caches the configured project root as maximum authority.
fn cache_path_resolution_maximum(service: &mut RuntimeSessionService, root: &Path) {
    let request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![".".to_string()],
        vec![".".to_string()],
        Vec::new(),
    )
    .unwrap();
    let command = mez_agent::shell::pane_path_resolution_command(
        &request,
        mez_agent::ShellClassification::PosixSh,
    )
    .unwrap();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let cache_key = service.path_resolution_cache_key("%1", &request).unwrap();
    service
        .observe_path_resolution_transaction_end(
            "maximum-path-resolution",
            "%1",
            0,
            cache_key,
            &String::from_utf8(output.stdout).unwrap(),
            false,
        )
        .unwrap();
}

/// Verifies complete read, write, create, delete, and touch effects are
/// deduplicated into one action-specific resolver request before execution.
#[test]
fn runtime_complete_effects_request_action_specific_path_resolution() {
    let root = temp_root("runtime-action-path-resolution");
    for directory in ["src", "target", "generated", "obsolete", "metadata"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    configure_path_resolution_bubblewrap(&mut service);
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    cache_path_resolution_maximum(&mut service, &root);
    mark_test_pane_ready(&mut service, "%1");

    let mut effects = path_resolution_effects();
    effects.reads = vec!["src".to_string(), "src".to_string()];
    effects.writes = vec!["target".to_string()];
    effects.creates = vec!["generated/new.txt".to_string()];
    effects.deletes = vec!["obsolete/old.txt".to_string()];
    effects.touches = vec!["metadata/stamp".to_string()];
    let evaluation = path_resolution_evaluation(
        mez_agent::permissions::EffectCompleteness::Complete,
        effects,
    );

    assert!(
        !service
            .ensure_bubblewrap_path_resolution_for_action(
                &path_resolution_turn(),
                "action-1",
                Some(&evaluation),
            )
            .unwrap()
    );
    let transaction = service
        .running_shell_transactions_for_tests()
        .values()
        .find(|transaction| {
            matches!(
                &transaction.kind,
                RunningShellTransactionKind::PathResolution {
                    action_id: Some(action_id),
                    ..
                } if action_id == "action-1"
            )
        })
        .unwrap();
    let RunningShellTransactionKind::PathResolution { cache_key, .. } = &transaction.kind else {
        unreachable!();
    };
    assert_eq!(
        cache_key.request.additional_paths,
        vec![
            "generated/new.txt",
            "metadata/stamp",
            "obsolete/old.txt",
            "src",
            "target",
        ]
    );
    assert_eq!(cache_key.request.read_scopes, vec![root.to_string_lossy()]);
    assert_eq!(cache_key.request.write_scopes, vec![root.to_string_lossy()]);

    fs::remove_dir_all(root).unwrap();
}

/// Verifies unknown filesystem effects retain maximum authority and do not
/// dispatch an unnecessary action-specific resolver transaction.
#[test]
fn runtime_unknown_effects_skip_action_specific_path_resolution() {
    let root = temp_root("runtime-unknown-action-path-resolution");
    fs::create_dir_all(root.join("src")).unwrap();
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    configure_path_resolution_bubblewrap(&mut service);
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    cache_path_resolution_maximum(&mut service, &root);
    mark_test_pane_ready(&mut service, "%1");

    let mut effects = path_resolution_effects();
    effects.reads.push("src".to_string());
    effects.unknown = true;
    let evaluation =
        path_resolution_evaluation(mez_agent::permissions::EffectCompleteness::Unknown, effects);

    assert!(
        service
            .ensure_bubblewrap_path_resolution_for_action(
                &path_resolution_turn(),
                "action-1",
                Some(&evaluation),
            )
            .unwrap()
    );
    assert!(service.running_shell_transactions_for_tests().is_empty());

    fs::remove_dir_all(root).unwrap();
}

/// Verifies broad deterministic user-home authority resolves every protected
/// credential descendant even when command effects are otherwise unknown.
#[test]
fn runtime_user_home_authority_resolves_credential_mask_paths() {
    let root = temp_root("runtime-user-home-path-resolution");
    let home = root.join("home").join("alice");
    fs::create_dir_all(&home).unwrap();
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    configure_path_resolution_bubblewrap(&mut service);
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&home));
    cache_path_resolution_maximum(&mut service, &home);
    mark_test_pane_ready(&mut service, "%1");

    let mut effects = path_resolution_effects();
    effects.unknown = true;
    let evaluation =
        path_resolution_evaluation(mez_agent::permissions::EffectCompleteness::Unknown, effects);

    assert!(
        !service
            .ensure_bubblewrap_path_resolution_for_action(
                &path_resolution_turn(),
                "action-1",
                Some(&evaluation),
            )
            .unwrap()
    );
    let transaction = service
        .running_shell_transactions_for_tests()
        .values()
        .find(|transaction| {
            matches!(
                &transaction.kind,
                RunningShellTransactionKind::PathResolution {
                    action_id: Some(action_id),
                    ..
                } if action_id == "action-1"
            )
        })
        .unwrap();
    let RunningShellTransactionKind::PathResolution { cache_key, .. } = &transaction.kind else {
        unreachable!();
    };
    let expected = [".aws", ".azure", ".docker", ".gnupg", ".kube", ".ssh"]
        .map(|protected| home.join(protected).to_string_lossy().into_owned())
        .to_vec();
    assert_eq!(cache_key.request.additional_paths, expected);

    fs::remove_dir_all(root).unwrap();
}

/// Verifies resolved authority is cached only for the exact pane environment,
/// configuration generation, and bounded request that produced it.
#[test]
fn runtime_path_resolution_cache_invalidates_on_config_generation() {
    let root = temp_root("runtime-path-resolution-cache");
    fs::create_dir_all(root.join("target")).unwrap();
    let mut service = test_runtime_service();
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    let request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![".".to_string()],
        vec!["target/generated".to_string()],
        Vec::new(),
    )
    .unwrap();
    let command = mez_agent::shell::pane_path_resolution_command(
        &request,
        mez_agent::ShellClassification::PosixSh,
    )
    .unwrap();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let cache_key = service.path_resolution_cache_key("%1", &request).unwrap();

    service
        .observe_path_resolution_transaction_end(
            "path-resolution-marker",
            "%1",
            0,
            cache_key,
            &String::from_utf8(output.stdout).unwrap(),
            false,
        )
        .unwrap();
    let scopes = service
        .path_scopes_for_pane_request("%1", &request)
        .unwrap()
        .unwrap();
    assert_eq!(scopes.current_directory, root.to_string_lossy());
    assert_eq!(
        scopes.write_scopes,
        vec![root.join("target/generated").to_string_lossy()]
    );

    service.session.advance_config_generation();
    assert!(
        service
            .path_scopes_for_pane_request("%1", &request)
            .unwrap()
            .is_none()
    );
    fs::remove_dir_all(root).unwrap();
}

/// Verifies exact path-resolution requests coexist within one unchanged pane
/// environment so action-specific evidence does not evict maximum authority.
#[test]
fn runtime_path_resolution_cache_retains_distinct_exact_requests() {
    let root = temp_root("runtime-path-resolution-cache-coexistence");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    let mut service = test_runtime_service();
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    let maximum_request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![".".to_string()],
        vec![".".to_string()],
        Vec::new(),
    )
    .unwrap();
    let action_request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![root.to_string_lossy().into_owned()],
        vec![root.to_string_lossy().into_owned()],
        vec!["src".to_string(), "target".to_string()],
    )
    .unwrap();

    for (marker, request) in [
        ("maximum-path-resolution", &maximum_request),
        ("action-path-resolution", &action_request),
    ] {
        let command = mez_agent::shell::pane_path_resolution_command(
            request,
            mez_agent::ShellClassification::PosixSh,
        )
        .unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let cache_key = service.path_resolution_cache_key("%1", request).unwrap();
        service
            .observe_path_resolution_transaction_end(
                marker,
                "%1",
                0,
                cache_key,
                &String::from_utf8(output.stdout).unwrap(),
                false,
            )
            .unwrap();
    }

    assert!(
        service
            .path_scopes_for_pane_request("%1", &maximum_request)
            .unwrap()
            .is_some()
    );
    assert!(
        service
            .path_scopes_for_pane_request("%1", &action_request)
            .unwrap()
            .is_some()
    );
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a terminal resolver failure is retained for the exact cache key so
/// provider polling fails closed instead of repeatedly launching the resolver.
#[test]
fn runtime_path_resolution_failure_is_terminal_for_exact_identity() {
    let root = temp_root("runtime-path-resolution-failure");
    fs::create_dir_all(&root).unwrap();
    let mut service = test_runtime_service();
    service.set_pane_environment_signature_for_tests("%1", path_resolution_environment(&root));
    let request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![".".to_string()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let cache_key = service.path_resolution_cache_key("%1", &request).unwrap();
    let transaction = RunningShellTransactionRef {
        turn_id: "path-resolution-turn".to_string(),
        kind: RunningShellTransactionKind::PathResolution {
            cache_key: cache_key.clone(),
            action_id: None,
        },
        pane_id: "%1".to_string(),
        command: "path resolution".to_string(),
        started_at_unix_ms: 0,
        timeout_ms: Some(10_000),
        pending_input_payload: None,
        observed_output_bytes: 0,
        observed_output_preview: String::new(),
        observed_output_truncated: false,
    };

    service
        .fail_path_resolution_transaction(
            "path-resolution-marker",
            &transaction,
            "resolver protocol failed",
        )
        .unwrap();
    let error = service
        .path_scopes_for_pane_request("%1", &request)
        .unwrap_err();
    assert!(error.message().contains("resolver protocol failed"));

    service.session.advance_config_generation();
    assert!(
        service
            .path_scopes_for_pane_request("%1", &request)
            .unwrap()
            .is_none()
    );
    fs::remove_dir_all(root).unwrap();
}

/// Verifies runtime control agent shell state persists in service.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_agent_shell_state_persists_in_service() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let show = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        show.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{show}"
    );

    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""pane_id":"%1""#), "{list}");
    assert!(list.contains(r#""visible":true"#), "{list}");
    assert!(
        list.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{list}"
    );

    let targeted_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"targeted-list","method":"agent/list","params":{"target":{"default":true}}}"#,
        &primary,
    );
    assert!(
        targeted_list.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{targeted_list}"
    );

    let missing_session_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"missing-list","method":"agent/list","params":{"target":{"session_id":"missing"}}}"#,
        &primary,
    );
    assert!(
        missing_session_list.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session_list}"
    );

    let hide = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"hide","method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");

    let relist = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"relist","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(relist.contains(r#""visible":false"#), "{relist}");
    assert!(
        relist.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{relist}"
    );
}

/// Verifies that the JSON-RPC agent shell visibility endpoints apply the same
/// live pane subshell side effects as the terminal `agent-shell` command. This
/// protects clients that enter agent mode through control APIs from bypassing
/// the parent-shell isolation boundary.
#[test]
fn runtime_control_agent_shell_visibility_enters_and_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .pane_screen_mut(&pane_id)
        .unwrap()
        .feed(b"control show history\ncontrol show visible text");

    let show = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let after_show_screen = service.pane_screen(&pane_id).unwrap();
    assert!(
        !after_show_screen
            .visible_lines()
            .join("\n")
            .contains("control show visible text")
    );
    assert!(
        after_show_screen
            .normal_content_lines()
            .join("\n")
            .contains("control show visible text")
    );
    let enter_input = service.drain_pane_io_transition().side_effects;
    assert_eq!(pane_input_effects(&enter_input).len(), 1);
    assert_eq!(enter_input[0].pane_input_parts().0, pane_id);
    assert!(service.agent_subshell_is_active(&pane_id));

    let hide = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"hide","method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");
    let exit_effects = service.drain_pane_io_transition().side_effects;
    let exit_inputs = pane_input_effects(&exit_effects);
    assert_eq!(exit_inputs.len(), 1);
    assert_eq!(exit_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x04");
    assert!(!service.agent_subshell_is_active(&pane_id));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies that hidden agent-shell output uses a bounded Mezzanine-marker
/// scanner instead of feeding arbitrary command output into a terminal screen.
/// Long shell-command bodies can contain megabytes of plain text or embedded
/// terminal escapes; those bytes are model data and must not monopolize the UI
/// parser while the runtime waits for its own transaction marker.
#[test]
fn runtime_hidden_agent_shell_osc_parser_skips_large_command_bodies() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service
        .pane_transaction_osc_screens_mut_for_tests()
        .remove("%1");
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "read-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "head -c 1048577 -- src/lib.rs".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let mut output = vec![b'x'; 2 * 1024 * 1024];
    output.extend_from_slice(b"\x1b[?1049hignored alternate-screen bytes from file content\n");
    output.extend_from_slice(
        b"\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );

    let (events, alternate_active, _) = service
        .terminal_osc_events_for_pane_bytes("%1", size, &output)
        .unwrap();

    assert!(!alternate_active);
    assert_eq!(
        events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
    assert!(
        !service
            .pane_transaction_osc_screens_for_tests()
            .contains_key("%1"),
        "hidden agent shell output should not allocate or feed the full terminal parser"
    );
    assert!(
        !service
            .pane_transaction_osc_pending_for_tests()
            .contains_key("%1")
    );
}

/// Verifies the bounded hidden-output marker scanner still preserves
/// transaction markers split across PTY reads. This keeps the lightweight path
/// compatible with the real-world fragmentation that the full terminal parser
/// handled before hidden agent-shell output was bypassed.
#[test]
fn runtime_hidden_agent_shell_osc_parser_preserves_fragmented_markers() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service
        .pane_transaction_osc_screens_mut_for_tests()
        .remove("%1");
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "read-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "head -c 1048577 -- src/lib.rs".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let (first_events, _, _) = service
        .terminal_osc_events_for_pane_bytes(
            "%1",
            size,
            b"large body\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez",
        )
        .unwrap();
    let (second_events, _, _) = service
        .terminal_osc_events_for_pane_bytes("%1", size, b"_pane=%1\x1b\\")
        .unwrap();

    assert_eq!(first_events, Vec::<TerminalOscEvent>::new());
    assert_eq!(
        second_events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
    assert!(
        !service
            .pane_transaction_osc_pending_for_tests()
            .contains_key("%1")
    );
}

///
/// Shell-mode inheritance must land on the child pane before the subagent turn
/// begins so model-authored local actions use the same executor path as the
/// Verifies exiting a parent agent shell closes active child subagent panes.
///
/// Subagent panes are owned by the parent delegation tree. Leaving the parent
/// session should not leave child agents, write scopes, or panes behind as
/// orphaned runtime state.
#[test]
fn runtime_parent_agent_shell_exit_closes_child_subagent_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .execute_terminal_command(&primary, "split-window")
        .unwrap();
    let child_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .find(|pane| pane.id.as_str() != "%1")
        .map(|pane| pane.id.to_string())
        .expect("split-window should create a child pane");
    let child_agent_id = format!("agent-{child_pane_id}");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&child_pane_id)
        .unwrap();
    service.set_subagent_lineage(
        child_agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "helper".to_string(),
        },
    );

    service.request_agent_shell_exit_for_pane("%1").unwrap();
    assert!(
        service
            .session()
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .all(|pane| pane.id.as_str() != child_pane_id)
    );
    assert!(!service.has_subagent_lineage(&child_agent_id));
    assert!(service.agent_shell_store().get(&child_pane_id).is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that foreground pane input applied through the async deferred I/O
/// path clears retained agent-shell output filters before the pane process
/// echoes new user-owned bytes. Without this boundary reset, a delayed parent
/// prompt repaint can be reduced to a carriage return while the foreground
/// cursor remains visually placed after the old prompt, causing the next echoed
/// input to render at column zero.
#[test]
fn runtime_deferred_foreground_input_clears_agent_shell_output_filters() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.remember_hidden_shell_render_suppression("%1");
    service.remember_mez_wrapper_filter_command("%1", "MEZ_MARKER_TOKEN='abc'");

    let (report, deferred) = service
        .apply_attached_terminal_step_transition(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"a".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 1);
    let pane_inputs = pane_input_effects(&deferred.side_effects);
    assert_eq!(pane_inputs.len(), 1);
    assert_eq!(pane_inputs[0].pane_input_parts().0, "%1");
    assert_eq!(pane_inputs[0].pane_input_parts().1, b"a");
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let prompt_repaint = service.visible_pane_output_bytes("%1", b"\r$ ");
    assert_eq!(prompt_repaint, b"\r$ ");
}

/// Verifies that a visible pane agent shell publishes the active model profile,
/// reasoning profile, and idle status into pane frame context before any turn
/// has started. The default header relies on these fields for agent mode.
#[test]
fn runtime_frame_context_reports_visible_agent_shell_metadata() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.mode.as_deref(), Some("agent"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_status.as_deref(), Some("idle"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-work"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}
