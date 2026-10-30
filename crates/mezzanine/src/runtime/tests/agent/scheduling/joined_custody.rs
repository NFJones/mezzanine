//! Retained source identity for an outstanding real joined spawn.
//!
//! These controls qualify custody only; they do not claim that the parent has
//! acquired a dialogue provider permit or completed the saved clarification flow.

use super::*;

/// Creates one actual joined spawn and returns its existing parent/dependency.
/// Native child startup may still be queued; no child effect is dispatched.
fn custody_fixture() -> (
    RuntimeSessionService,
    mez_agent::AgentTurnRecord,
    JoinedSubagentDependency,
    mez_agent::AgentTurnExecution,
) {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    let parent = service
        .start_agent_prompt_turn("%1", "retain the original joined task")
        .unwrap();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "one joined spawn".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            provider_transcript_events: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "one original child".into(),
                actions: vec![runtime_spawn_agent_action(
                    "custody-original-spawn",
                    "inspect original task",
                )],
            }),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let child = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id != parent.turn_id)
        .unwrap();
    let dependency = service
        .joined_subagent_dependency(&child.turn_id)
        .unwrap()
        .clone();
    let parent = service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .unwrap()
        .clone();
    assert_eq!(parent.state, AgentTurnState::Blocked);
    (service, parent, dependency, execution)
}

/// Parking captures the real original action independently of the live latest
/// response. Replacing only that test slot must not replace custody or grant a
/// continuation. An attempted recapture with changed provider source fails and
/// leaves the original action intact; dependency removal retires its source.
#[test]
fn runtime_joined_source_custody_keeps_original_after_latest_response_changes() {
    let (mut service, parent, dependency, execution) = custody_fixture();
    let original_execution = execution.clone();
    let original = execution.response.action_batch.as_ref().unwrap().actions[0].clone();
    assert_eq!(
        service.retained_joined_spawn_action(&dependency).unwrap(),
        Some(original.clone())
    );
    service
        .retain_joined_parent_execution(&parent, &execution)
        .unwrap();
    let mut later = execution.clone();
    later.response = runtime_say_response_for_agent(
        &parent.turn_id,
        &parent.agent_id,
        "later coordination source",
        false,
    );
    service
        .agent_turn_executions_mut()
        .insert(parent.turn_id.clone(), later);
    assert_eq!(
        service.retained_joined_spawn_action(&dependency).unwrap(),
        Some(original.clone())
    );
    let mut conflicting = execution;
    if let mez_agent::AgentActionPayload::SpawnAgent { task_prompt, .. } =
        &mut conflicting.response.action_batch.as_mut().unwrap().actions[0].payload
    {
        task_prompt.push_str(" changed source");
    }
    assert!(
        service
            .retain_joined_parent_execution(&parent, &conflicting)
            .is_err()
    );
    assert_eq!(
        service.retained_joined_spawn_action(&dependency).unwrap(),
        Some(original)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    service
        .agent_turn_executions_mut()
        .insert(parent.turn_id.clone(), original_execution);
    service.remove_joined_subagent_dependencies_for_agent(&dependency.child_agent_id);
    assert_eq!(
        service.retained_joined_spawn_action(&dependency).unwrap(),
        None
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A new pane conversation cannot inherit an old join's source identity. The
/// original source is rejected until explicit dependency/session cleanup drops
/// it, and no action is dispatched or synthetic task result is created.
#[test]
fn runtime_joined_source_custody_rejects_replacement_parent_binding() {
    let (mut service, parent, dependency, _) = custody_fixture();
    service
        .agent_shell_store_mut()
        .remove_session(&parent.pane_id);
    let replacement = service
        .agent_shell_store_mut()
        .enter_or_resume(&parent.pane_id)
        .unwrap()
        .session_id
        .clone();
    assert_ne!(replacement, parent.conversation_id);
    assert!(
        service
            .retained_joined_spawn_action(&dependency)
            .unwrap_err()
            .message()
            .contains("binding changed")
    );
    service.clear_all_joined_subagent_dependencies();
    assert_eq!(
        service.retained_joined_spawn_action(&dependency).unwrap(),
        None
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Preparation derives a controller value from the retained original target
/// even when a controlled later slot has no spawn action. It preserves the
/// first received value on exact repeat, rejects conflicting output and a
/// foreign application slot, and neither commits context nor admits work.
/// These supplied observation facts are fixture data, not child completion or
/// an authorized coordination response; the actual lifecycle control below
/// separately proves normal terminal adoption.
#[test]
fn runtime_joined_result_preparation_retains_original_target_and_first_value() {
    let (mut service, parent, dependency, execution) = custody_fixture();
    let mut later = execution.clone();
    later.response = runtime_say_response_for_agent(
        &parent.turn_id,
        &parent.agent_id,
        "later untouched response",
        false,
    );
    service
        .agent_turn_executions_mut()
        .insert(parent.turn_id.clone(), later.clone());
    let before_context = service
        .agent_turn_contexts()
        .get(&parent.turn_id)
        .unwrap()
        .clone();
    let prepared = service
        .prepare_joined_task_result(
            &dependency,
            true,
            "PRIVATE_RECEIVED_SUMMARY",
            "PRIVATE_RECEIVED_OUTPUT",
        )
        .unwrap();
    let duplicate = service
        .prepare_joined_task_result(
            &dependency,
            true,
            "PRIVATE_RECEIVED_SUMMARY",
            "PRIVATE_RECEIVED_OUTPUT",
        )
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&prepared, &duplicate));
    assert_eq!(prepared.result().action_id, dependency.parent_action_id);
    assert_eq!(prepared.result().turn_id, parent.turn_id);
    assert_eq!(prepared.matching_result_slot(&execution).unwrap(), 0);
    assert!(prepared.matching_result_slot(&later).is_err());
    let mut foreign = execution.clone();
    foreign.action_results[0].agent_id = "foreign-agent".into();
    assert!(prepared.matching_result_slot(&foreign).is_err());
    assert!(
        service
            .prepare_joined_task_result(
                &dependency,
                true,
                "PRIVATE_RECEIVED_SUMMARY",
                "different output"
            )
            .is_err()
    );
    assert!(!format!("{prepared:?}").contains("PRIVATE_RECEIVED"));
    assert_eq!(
        service.agent_turn_contexts().get(&parent.turn_id),
        Some(&before_context)
    );
    assert_eq!(
        service
            .agent_turn_executions()
            .get(&parent.turn_id)
            .unwrap()
            .response
            .raw_text,
        later.response.raw_text
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    service
        .agent_turn_executions_mut()
        .insert(parent.turn_id.clone(), execution);
    service.clear_all_joined_subagent_dependencies();
    service.terminate_all_pane_processes().unwrap();
}

/// A cached received value does not bypass conversation replacement. Rechecking
/// preparation must reject the stale original binding before returning cached
/// data, and a locally retained fact does not become application authority.
#[test]
fn runtime_joined_result_preparation_rechecks_binding_before_cached_value() {
    let (mut service, parent, dependency, _) = custody_fixture();
    let prepared = service
        .prepare_joined_task_result(&dependency, false, "child failed", "retained diagnostic")
        .unwrap();
    service
        .agent_shell_store_mut()
        .remove_session(&parent.pane_id);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&parent.pane_id)
        .unwrap();
    assert!(
        service
            .prepare_joined_task_result(&dependency, false, "child failed", "retained diagnostic")
            .unwrap_err()
            .message()
            .contains("binding changed")
    );
    assert!(
        prepared
            .result()
            .structured_content_json
            .as_ref()
            .unwrap()
            .contains("retained diagnostic")
    );
    service.clear_all_joined_subagent_dependencies();
    service.terminate_all_pane_processes().unwrap();
}

/// Actual child terminal execution uses the prepared original spawn result,
/// commits it once and normally resumes the parent. Repeated terminal delivery
/// cannot append another result or create another child. This control is normal
/// completion, not the still-unfinished clarification/admission workflow.
#[test]
fn runtime_joined_prepared_result_flows_through_real_child_completion_once() {
    let (mut service, parent, dependency, _) = custody_fixture();
    let child = service
        .agent_turn_ledger()
        .turn(&dependency.child_turn_id)
        .unwrap()
        .clone();
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    service.start_ready_agent_turns().unwrap();
    let provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            "PREPARED_REAL_CHILD_RESULT",
            true,
        ),
    };
    let finished = service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(!service.has_joined_subagent_dependency(&child.turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let result = &service
        .agent_turn_executions()
        .get(&parent.turn_id)
        .unwrap()
        .action_results[0];
    assert_eq!(result.status, ActionStatus::Succeeded);
    assert!(
        result
            .structured_content_json
            .as_ref()
            .unwrap()
            .contains("PREPARED_REAL_CHILD_RESULT")
    );
    let before = service
        .agent_turn_contexts()
        .get(&parent.turn_id)
        .unwrap()
        .clone();
    service
        .emit_subagent_task_result_for_execution(&child, &finished)
        .unwrap();
    assert_eq!(
        service.agent_turn_contexts().get(&parent.turn_id),
        Some(&before)
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 2);
    service.terminate_all_pane_processes().unwrap();
}

/// Locally retained terminal facts must not silently follow an explicitly
/// redirected child attempt. A metadata fixture establishes the existing
/// interruption route, then both prequeue preparation and transfer reject the
/// pending result without creating another turn or changing the dependency.
#[test]
fn runtime_joined_prepared_result_blocks_attempt_redirection_until_settlement() {
    let (mut service, _, dependency, _) = custody_fixture();
    let child = service
        .agent_turn_ledger()
        .turn(&dependency.child_turn_id)
        .unwrap()
        .clone();
    let prepared = service
        .prepare_joined_task_result(
            &dependency,
            true,
            "received result",
            "original attempt output",
        )
        .unwrap();
    assert!(service.retain_subagent_interruption_for_redirection(&child));
    let mut candidate = child.clone();
    candidate.turn_id = "unadmitted-redirection-candidate".into();
    assert!(
        service
            .prepare_interrupted_subagent_redirected_turn(&mut candidate)
            .unwrap_err()
            .message()
            .contains("must settle")
    );
    assert!(!service.transfer_interrupted_subagent_redirection(
        &child.agent_id,
        &child.conversation_id,
        &candidate.turn_id
    ));
    assert_eq!(
        service.joined_subagent_dependency(&child.turn_id),
        Some(&dependency)
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 2);
    let duplicate = service
        .prepare_joined_task_result(
            &dependency,
            true,
            "received result",
            "original attempt output",
        )
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&prepared, &duplicate));
    service.clear_all_joined_subagent_dependencies();
    service.terminate_all_pane_processes().unwrap();
}
