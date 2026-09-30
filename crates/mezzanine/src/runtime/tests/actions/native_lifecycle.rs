//! Native lifecycle failure settlement and bounded model-feedback regressions.
//!
//! Trusted status facts must survive the worker boundary without causing the
//! runtime to repeat commands whose effects may already have happened.

use super::*;

/// A real child can perform effects and omit its trusted completion record.
/// Settlement must retain diagnostic facts in model feedback, reject duplicate
/// outcomes, and never redispatch that uncertain action automatically.
#[test]
fn runtime_native_incomplete_lifecycle_preserves_diagnostics_without_replay() {
    run_native_lifecycle_guidance(false, false);
}

/// Malformed status cannot authorize guidance or execution even when the
/// process performed effects before its untrustworthy status was collected.
#[test]
fn runtime_native_malformed_lifecycle_does_not_queue_recovery() {
    run_native_lifecycle_guidance(true, false);
}

/// Cancellation retires the exact action before a late incomplete outcome;
/// that outcome cannot queue a provider continuation or replay the workload.
#[test]
fn runtime_native_cancelled_lifecycle_does_not_queue_recovery() {
    run_native_lifecycle_guidance(false, true);
}

/// Runs a real child effect under valid, malformed, or cancelled ownership.
fn run_native_lifecycle_guidance(malformed: bool, cancelled: bool) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("native-lifecycle-transcript"));
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .execute_agent_shell_command(&primary, "inspect native status")
        .unwrap();
    let counter = temp_root("native-incomplete-effect").join("effects");
    fs::create_dir_all(counter.parent().unwrap()).unwrap();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native diagnostic fixture".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "inspect once".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Inspect once".to_string(),
                        command: format!("printf x >> '{}'", counter.display()),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(5_000),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let mut dispatch = service
        .claim_native_shell_action("turn-1", "shell-1")
        .unwrap()
        .unwrap();
    dispatch.sandbox_backend = Some(crate::runtime::SandboxBackend::Bubblewrap);
    dispatch.request.transaction = dispatch.request.transaction.with_child_launch(
        mez_agent::ShellChildLaunch::new("/bin/sh", vec![
            mez_agent::ShellChildArgument::Literal("-c".to_string()),
            mez_agent::ShellChildArgument::Literal("printf '{\"child-pid\":%s}\\n' \"$$\" >&3; /bin/sh \"$1\"; printf '%s\\n' 'launcher detail' '{\"nested\":{\"password\":\"opaque-secret\",\"access_token\":\"opaque-credential\"}}' >&2; exit 7".to_string()),
            mez_agent::ShellChildArgument::Literal("sh".to_string()),
            mez_agent::ShellChildArgument::MaterializedCommandFile,
        ]).unwrap().with_status_fd(3).unwrap(),
    );
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    let mut outcome = outcome;
    if malformed {
        let failure = outcome.result.as_mut().unwrap_err();
        let lifecycle = failure.lifecycle.as_mut().unwrap();
        lifecycle.class = crate::security::sandbox::SandboxLifecycleFailureClass::Malformed;
        lifecycle.child_record_present = None;
        lifecycle.exit_record_present = None;
    }
    if cancelled {
        service.stop_agent_turn_for_pane("%1").unwrap();
        assert!(!service.complete_native_shell_action(outcome).unwrap());
        assert!(!service.agent_provider_task_is_pending("turn-1"));
        assert!(service.pending_native_shell_actions().is_empty());
        assert_eq!(fs::read(&counter).unwrap(), b"x");
        service.terminate_all_pane_processes().unwrap();
        let _ = fs::remove_dir_all(counter.parent().unwrap());
        return;
    }
    let evidence = outcome
        .result
        .as_ref()
        .unwrap_err()
        .lifecycle
        .as_ref()
        .unwrap();
    assert_eq!(evidence.child_record_present, (!malformed).then_some(true));
    assert_eq!(evidence.outer_exit_code, Some(7));
    assert!(!evidence.stderr.contains("opaque-secret"));
    assert!(!evidence.stderr.contains("opaque-credential"));
    assert!(
        service
            .complete_native_shell_action(outcome.clone())
            .unwrap()
    );
    assert!(!service.complete_native_shell_action(outcome).unwrap());
    assert!(service.pending_native_shell_actions().is_empty());
    if malformed {
        assert!(!service.agent_provider_task_is_pending("turn-1"));
        assert_eq!(
            service.agent_turn_ledger().turn("turn-1").unwrap().state,
            AgentTurnState::Failed
        );
        assert_eq!(fs::read(&counter).unwrap(), b"x");
        service.terminate_all_pane_processes().unwrap();
        let _ = fs::remove_dir_all(counter.parent().unwrap());
        return;
    }
    assert!(
        service.agent_provider_task_is_pending("turn-1"),
        "valid incomplete lifecycle evidence must reach bounded model guidance"
    );
    let context = runtime_prepared_context_for_turn(&service, "turn-1");
    let feedback = context
        .blocks()
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::ActionResult
                && block.content.contains("sandbox_lifecycle")
        })
        .expect("bounded lifecycle evidence must reach the acting model");
    assert!(
        feedback.content.contains("launcher detail"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Do not replay the original command"),
        "{}",
        feedback.content
    );
    assert_eq!(fs::read(&counter).unwrap(), b"x");
    service.stop_agent_turn_for_pane("%1").unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = store.inspect(&conversation).unwrap();
    let feedback = entries
        .iter()
        .find(|entry| entry.content.contains("sandbox_lifecycle"))
        .expect("bounded lifecycle evidence must survive the model-facing action-error projection");
    assert!(
        feedback.content.contains("missing_exit"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("launcher detail"),
        "{}",
        feedback.content
    );
    assert!(
        matches!(mez_agent::TranscriptContextEvent::from_transcript_content(&feedback.content),
            Some(mez_agent::TranscriptContextEvent::ExecutionBlock { content, .. })
                if content.contains("\"automatic_replay\":false")),
        "{}",
        feedback.content
    );
    assert_eq!(fs::read(&counter).unwrap(), b"x");
    assert!(
        entries
            .iter()
            .all(|entry| !entry.content.contains("opaque-secret")
                && !entry.content.contains("opaque-credential"))
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(counter.parent().unwrap());
}

/// Valid incomplete evidence consumes the ordinary failure-feedback budget;
/// repeated advice cannot restart that budget or dispatch the original action.
#[test]
fn runtime_native_lifecycle_guidance_obeys_feedback_budget() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_action_failure_retry_limit(1);
    let started = service
        .start_agent_prompt_turn("%1", "diagnose safely")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let action = mez_agent::AgentAction {
        id: "uncertain-action".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect once".to_string(),
            command: "true".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(1000),
        },
    };
    let mut result = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "sandbox_lifecycle_incomplete",
        "completion unknown",
    )
    .unwrap();
    result.error.as_mut().unwrap().data_json = Some(
        serde_json::json!({
            "sandbox_lifecycle":{"class":"missing_exit"},
            "model_guidance":true,"automatic_replay":false
        })
        .to_string(),
    );
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "inspect once".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "inspect".to_string(),
                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution.clone(),
                "lifecycle_guidance"
            )
            .unwrap()
    );
    service.remove_pending_agent_provider_task(&turn.turn_id);
    assert!(
        !service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut execution.clone(),
                "lifecycle_guidance"
            )
            .unwrap()
    );
    assert!(!service.agent_provider_task_is_pending(&turn.turn_id));
    assert!(service.pending_native_shell_actions().is_empty());
}
