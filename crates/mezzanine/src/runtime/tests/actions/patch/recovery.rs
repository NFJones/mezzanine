//! Runtime tests for actions patch recovery behavior.

use super::*;

/// Verifies controller retry coaching is not persisted beside factual action
/// evidence in durable model chronology.
fn assert_no_persisted_failure_feedback(context: &mez_agent::AgentContext) {
    assert!(context.blocks().iter().all(|block| {
        block.source != ContextSourceKind::RuntimeHint || block.label != "action failure feedback"
    }));
}

/// Verifies an `apply_patch` validation failure is eligible for model
/// correction.
///
/// Malformed Mezzanine patch payloads are model-correctable input errors and
/// must not end the turn before the model sees the failed action result.
#[test]
fn runtime_apply_patch_invalid_params_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-invalid".to_string(),
        rationale: "apply an invalid patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch".to_string(),
            strip: None,
        },
    };

    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "invalid_params",
        "apply_patch requires Mezzanine patch blocks starting with *** Begin Patch; use shell_command with git apply for raw unified diffs",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "state": "dispatch_failed",
            "stage": "local_action_plan",
            "error": {
                "kind": "invalid_params",
                "message": "apply_patch requires Mezzanine patch blocks starting with *** Begin Patch; use shell_command with git apply for raw unified diffs"
            }
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "invalid patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
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
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_validation_failed",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    assert_eq!(
        service
            .agent_failure_feedback_attempts_for_tests()
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![1]
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-invalid apply_patch failed]")
            && block.content.contains("Mezzanine patch blocks starting")
    }));
    assert_no_persisted_failure_feedback(context);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("Failed after"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `apply_patch` hunk mismatches preserve exact factual evidence.
///
/// A generic "action failed" continuation is not enough for patch hunk
/// mismatches because replaying the same patch will deterministically fail.
/// The model should be steered to inspect the current file and generate a fresh
/// Mezzanine patch block instead.
#[test]
fn runtime_apply_patch_hunk_mismatch_recovery_preserves_failure_evidence() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-hunk".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch:
                "*** Begin Patch\n*** Update File: src/driver/mod.rs\n@@\n-old\n+new\n*** End Patch"
                    .to_string(),
            strip: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": "apply_patch: hunk did not match: src/driver/mod.rs\napply_patch: patch failed",
                "combined_output_bytes": 91,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "hunk mismatch patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
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
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_hunk_mismatch",
        )
        .unwrap();

    assert!(queued);
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_no_persisted_failure_feedback(context);
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("apply_patch: hunk did not match: src/driver/mod.rs")
    }));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    assert!(pane_text.contains("(patch hunk mismatch)"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies replacement-present patch mismatches retain their diagnostic.
///
/// Matcher diagnostics can report that the replacement block or distinctive
/// added lines are already present. Runtime feedback should preserve that
/// subtype so the model inspects current file state instead of replaying the
/// same stale patch.
#[test]
fn runtime_apply_patch_replacement_hint_recovery_preserves_diagnostic() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-replacement".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch:
                "*** Begin Patch\n*** Update File: src/driver/mod.rs\n@@\n-old\n+new\n*** End Patch"
                    .to_string(),
            strip: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": "apply_patch: hunk did not match: src/driver/mod.rs\napply_patch: replacement_hint=full_replacement_block_present span(s): 18-21\napply_patch: replacement_hint_next_step=skip_or_reconcile_already_applied_change\napply_patch: patch failed",
                "combined_output_bytes": 231,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "replacement hint patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
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
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution,
                "apply_patch_hunk_mismatch",
            )
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_no_persisted_failure_feedback(context);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies missing-anchor patch mismatches steer recovery toward refreshing
/// the hunk header anchor.
///
/// Matcher diagnostics can explain that a structural or header anchor could not
/// be found in order. Runtime feedback should preserve that signal so the model
/// repairs the anchor instead of treating the failure as a generic reread case.
#[test]
fn runtime_apply_patch_missing_anchor_recovery_preserves_diagnostic() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-anchor".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch:
                "*** Begin Patch\n*** Update File: src/driver/mod.rs\n@@ fn owner()\n-old\n+new\n*** End Patch"
                    .to_string(),
            strip: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "apply_patch",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": "apply_patch: hunk did not match: src/driver/mod.rs\napply_patch: hunk header anchor was not found in order: fn owner()\napply_patch: suggested_next_step=fix_or_refresh_header_anchor\napply_patch: patch failed",
                "combined_output_bytes": 211,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "missing anchor patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
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
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution,
                "apply_patch_hunk_mismatch",
            )
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_no_persisted_failure_feedback(context);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies ambiguous repeated-candidate mismatches keep candidate-region
/// reread guidance.
///
/// When matcher diagnostics surface multiple candidate read ranges, runtime
/// feedback should preserve that ambiguity and avoid collapsing the next step
/// back to one generic owner-range reread.
#[test]
fn runtime_apply_patch_candidate_region_recovery_preserves_diagnostic() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-candidates".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: note.rs\n@@\n-old();\n+new();\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "apply_patch",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": "apply_patch: hunk did not match: note.rs\napply_patch: candidate match span(s): 10, 12\napply_patch: suggested_candidate_read_range(s): note.rs:6-12, note.rs:8-12\napply_patch: suggested_next_step=reread_candidate_regions\napply_patch: patch failed",
                "combined_output_bytes": 256,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "ambiguous candidate patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
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
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution,
                "apply_patch_hunk_mismatch",
            )
            .unwrap()
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_no_persisted_failure_feedback(context);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies real `apply_patch` write-phase hunk failures enter model recovery.
///
/// `apply_patch` runs through a read transaction followed by a generated write
/// transaction. Direct recovery-unit tests do not prove the shell-transaction
/// observer routes write-phase hunk mismatches back into the correction loop,
/// so this covers the user-visible path that emits the final patch diagnostic.
#[test]
fn runtime_apply_patch_write_phase_hunk_mismatch_queues_model_recovery() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-patch-write-phase-recovery","input":"patch the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "patch response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "patch-write".to_string(),
                    rationale: "apply a source patch".to_string(),
                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Update File: tests/standard_config_consumer_test.rs\n@@\n-old\n+new\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(service.running_shell_transactions_for_tests().len(), 1);
    let marker = service
        .running_shell_transactions_for_tests()
        .keys()
        .next()
        .cloned()
        .unwrap();
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.command = "# __MEZ_APPLY_PATCH_WRITE_PHASE__".to_string();
    transaction.observed_output_preview =
        "apply_patch: hunk did not match: tests/standard_config_consumer_test.rs\n\
         apply_patch: exact hunk context was not found in the current file"
            .to_string();
    transaction.observed_output_bytes = transaction.observed_output_preview.len();

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 1)
        .unwrap();

    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Running)
    );
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert_no_persisted_failure_feedback(context);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    assert!(pane_text.contains("(patch hunk mismatch)"), "{pane_text}");
    assert!(!pane_text.contains("recovery unavailable"), "{pane_text}");
    let copy_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer failed-patches")
        .unwrap();
    assert!(
        copy_response.contains(r#""command":"copy-patches""#),
        "{copy_response}"
    );
    assert!(copy_response.contains("patches=written"), "{copy_response}");
    assert!(
        copy_response.contains("destination=buffer"),
        "{copy_response}"
    );
    let failed_patches = service.paste_buffers().get("failed-patches").unwrap();
    assert!(
        failed_patches.contains("patch 1: turn=turn-1 action=patch-write status=failed"),
        "{failed_patches}"
    );
    assert!(
        failed_patches
            .contains("apply_patch: hunk did not match: tests/standard_config_consumer_test.rs"),
        "{failed_patches}"
    );
    assert!(
        failed_patches.contains("*** Update File: tests/standard_config_consumer_test.rs"),
        "{failed_patches}"
    );
    assert!(failed_patches.contains("-old"), "{failed_patches}");
    assert!(failed_patches.contains("+new"), "{failed_patches}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies repeated identical `apply_patch` hunk mismatches stay unbounded
/// and omit retry-budget noise.
///
/// Provider wording and generated action ids can vary while the model repeats
/// the same bad patch. `apply_patch` recovery should still track repeated
/// identical failures for guidance, but it must not consume the generic
/// bounded retry budget or surface `(attempt/max)` status text.
#[test]
fn runtime_apply_patch_hunk_mismatch_recovery_is_unbounded_and_hides_retry_budget() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let build_execution = |raw_text: &str, action_id: &str| {
        let action = mez_agent::AgentAction {
            id: action_id.to_string(),
            rationale: "apply a source patch".to_string(),
            payload: mez_agent::AgentActionPayload::ApplyPatch {
                patch:
                    "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch"
                        .to_string(),
                strip: None,
            },
        };
        let mut failed = mez_agent::ActionResult::failed(
            &turn,
            &action,
            ActionStatus::Failed,
            "shell_command_failed",
            "shell command exited with status 1",
        )
        .unwrap();
        failed.structured_content_json = Some(
            serde_json::json!({
                "command": "apply_patch",
                "terminal_observation": {
                    "exit_code": 1,
                    "combined_output_preview": "apply_patch: hunk did not match: src/main.rs\napply_patch: patch failed",
                    "combined_output_bytes": 75,
                    "output_truncated": false
                }
            })
            .to_string(),
        );
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: raw_text.to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
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
            action_results: vec![failed],
            final_turn: false,
            terminal_state: AgentTurnState::Failed,
        }
    };

    for index in 0..8 {
        let action_id = format!("patch-{index}");
        let raw_text = if index % 2 == 0 {
            "first provider wording"
        } else {
            "different provider wording"
        };
        let mut execution = build_execution(raw_text, &action_id);
        append_test_execution_assistant_context(&mut service, &turn, &execution);
        assert!(
            service
                .queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "apply_patch_hunk_mismatch",
                )
                .unwrap()
        );
    }

    assert_eq!(
        service
            .agent_failure_feedback_attempts_for_tests()
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![8]
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_no_persisted_failure_feedback(context);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover (patch hunk mismatch)"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("/5"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies unsafe `apply_patch` paths receive CWD-relative recovery guidance.
///
/// Mezzanine patch headers are intentionally restricted to paths relative to
/// the pane current working directory. When a model emits an absolute path, the
/// corrective continuation should include the rejected path, the best-known CWD,
/// and a clear note that this restriction is specific to `apply_patch` headers.
#[test]
fn runtime_apply_patch_unsafe_path_recovery_preserves_diagnostic() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_current_working_directory(
        "%1".to_string(),
        PathBuf::from("/home/neil/Documents/repos/chimera"),
    );
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let unsafe_path = "/home/neil/Documents/repos/chimera/src/conf/document.rs";
    let action = mez_agent::AgentAction {
        id: "patch-absolute".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: format!(
                "*** Begin Patch\n*** Update File: {unsafe_path}\n@@\n-old\n+new\n*** End Patch"
            ),
            strip: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": format!("apply_patch: unsafe patch path: {unsafe_path}\n"),
                "combined_output_bytes": 96,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "absolute path patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
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
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_unsafe_path",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_no_persisted_failure_feedback(context);
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("apply_patch: unsafe patch path: /home/neil/Documents/repos/chimera/src/conf/document.rs")
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies unrecovered `apply_patch` failures render their captured terminal
/// diagnostic when the turn is actually ending failed.
///
/// While the model still has a recovery attempt, normal logging does not need
/// to show the patch stderr/stdout. Once recovery is unavailable or exhausted,
/// the user needs enough final context to understand why the patch action
/// failed, so the renderer should surface the bounded terminal observation
/// before the failed-turn footer.
#[test]
fn runtime_unrecovered_apply_patch_failure_logs_terminal_observation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-unrecovered-patch-failure","input":"patch the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .expect("started turn should be recorded");
    let action = mez_agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let mut result = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    result.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "combined_output_preview": "\n\n∙ MEZ_PATCH=$(mktemp) || exit 1\n∙ printf %s '*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch' > \"$MEZ_PATCH\"\n∙ \"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\napply_patch: hunk did not match: src/lib.rs\napply_patch: patch failed\n",
                "combined_output_bytes": 298,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failed patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![action],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions_mut()
        .insert("turn-1".to_string(), execution);

    service
        .finish_agent_turn("%1", "turn-1", AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_flat = pane_text.replace("▐ ", "").replace('\n', "");
    assert!(
        pane_text_flat.contains(
            "failed; recovery unavailable: no model-correction continuation was queued after the apply_patch failure"
        ),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("apply_patch: hunk did not match: src/lib.rs"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("MEZ_RESTORE_NOUNSET_NOW"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("[mez: failure output truncated for pane display]"),
        "{pane_text}"
    );
    assert!(pane_text.contains("Failed after"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies unrecovered `apply_patch` failures do not expose shell-wrapper
/// fragments when no actionable diagnostic survived capture.
///
/// Some failed patch commands can echo a partially quoted generated command as
/// isolated glyphs or words after the shell wrapper has already been stripped.
/// Those fragments are confusing to users and do not help model recovery, so a
/// final failed turn should prefer a concise generic diagnostic when no real
/// `apply_patch:` or error line is available.
#[test]
fn runtime_unrecovered_apply_patch_failure_uses_generic_line_for_fragments() {
    let action = mez_agent::AgentAction {
        id: "patch-fragment".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let lines = runtime_unrecovered_failure_output_lines(
        &action,
        "\n∙\nb\ngal(&mut\ncomma\nd\nS\ne\nu\nl\nE\nR\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n",
    );

    assert_eq!(
        lines,
        vec![
            "apply_patch failed without an actionable patch diagnostic. Next step: inspect the current target file with a bounded shell_command, then retry with a smaller fresh Mezzanine *** Begin Patch block."
                .to_string()
        ]
    );
}
