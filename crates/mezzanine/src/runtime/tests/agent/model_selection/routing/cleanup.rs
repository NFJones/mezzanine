//! Native routed-worker cleanup after its synchronous ephemeral binding setup.
//!
//! Each worker is created through actual routing selection and owns a native
//! process. Parent teardown must retain that genuine obligation through setup,
//! while a later independent child binding must stop the cleanup traversal.

use super::*;

/// Requires the exact fixture-owned process incarnation to have retired.
fn assert_routed_process_retired(identity: (u32, u64)) {
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(identity.0),
        Some(identity.1),
        "routed child process survived parent teardown"
    );
}

/// Creates an actual idle no-join grandchild under the routed worker. Keeping
/// it alive through invoking-parent restoration proves every transitive edge
/// remains valid, not merely the incoming routed-worker cleanup endpoint.
fn spawn_routed_cleanup_grandchild(
    service: &mut RuntimeSessionService,
    worker: &mez_agent::AgentTurnRecord,
) -> (String, (u32, u64)) {
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "routed-cleanup-grandchild",
    )));
    service.configure_subagent_policy(8, 8, 8, 8, SubagentWaitPolicy::Detach);
    let mut action = runtime_spawn_agent_action("cleanup-grandchild", "");
    let mez_agent::AgentActionPayload::SpawnAgent {
        lifetime,
        objective,
        ..
    } = &mut action.payload
    else {
        unreachable!()
    };
    *lifetime = mez_agent::SubagentLifetime::Persistent;
    *objective = Some("Idle native cleanup grandchild".to_string());
    let result = service
        .execute_spawn_action_for_turn(worker, &action)
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    let agent = value["spawn"]["agent"]["id"].as_str().unwrap();
    let pane = agent.strip_prefix("agent-").unwrap().to_string();
    let pid = service.primary_pid_for_live_pane_process(&pane).unwrap();
    let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
    (pane, (pid, token))
}

/// Routed setup's ephemeral conversation is part of the same successful fresh
/// delegation. Closing either a native or not-yet-started parent must close
/// the actual routed child, rather than lose its proof at that setup rebind.
#[test]
fn runtime_routed_cleanup_parent_close_retires_final_bound_worker() {
    for start_parent in [false, true] {
        let (mut service, _, worker) = selected_routed_loop_with_parent(
            "/loop inspect exact routed cleanup",
            crate::runtime::config::ShellMode::Native,
            start_parent,
        );
        let pid = service
            .primary_pid_for_live_pane_process(&worker.pane_id)
            .unwrap();
        let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        let result =
            service.dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#);
        let child_present = service.find_pane_descriptor(&worker.pane_id).is_some();
        let after = mez_mux::process::process_start_token_for_pid(pid);
        service.terminate_all_pane_processes().unwrap();
        result.unwrap();
        assert!(
            !child_present,
            "genuine routed worker pane survived parent close: native_after={after:?} expected={token}"
        );
        assert_ne!(
            after,
            Some(token),
            "genuine routed worker process survived parent close"
        );
        assert_routed_process_retired((pid, token));
    }
}

/// An actual accepted parent exit retains its native ownership evidence while
/// settling the worker. This exercises the event path after real wait/reap,
/// rather than synthesizing an exit for a pane with no process owner.
#[test]
fn runtime_routed_cleanup_actual_parent_exit_retires_final_bound_worker() {
    let (mut service, _, worker) = selected_routed_loop_with_parent(
        "/loop inspect routed exit cleanup",
        crate::runtime::config::ShellMode::Native,
        true,
    );
    let pid = service
        .primary_pid_for_live_pane_process(&worker.pane_id)
        .unwrap();
    let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
    let mut parent = service.take_running_pane_process_for_adapter("%1").unwrap();
    let parent_pid = parent.primary_pid();
    let status = parent.terminate(Duration::from_millis(10)).unwrap();
    let result = service.apply_pane_process_exit_event("%1", parent_pid, status);
    let child_present = service.find_pane_descriptor(&worker.pane_id).is_some();
    let after = mez_mux::process::process_start_token_for_pid(pid);
    service.terminate_all_pane_processes().unwrap();
    assert!(result.unwrap().is_some());
    assert!(
        !child_present,
        "genuine routed worker pane survived parent exit: native_after={after:?} expected={token}"
    );
    assert_ne!(
        after,
        Some(token),
        "genuine routed worker process survived parent exit"
    );
    assert_routed_process_retired((pid, token));
}

/// A separate child binding after routed setup is not part of that successful
/// fresh transaction. Parent cleanup must preserve the same native process and
/// its new conversation instead of extending setup-only migration authority.
#[test]
fn runtime_routed_cleanup_independent_child_rebind_preserves_native_worker() {
    let (mut service, _, worker) = selected_routed_loop("/loop inspect independent routed binding");
    let pid = service
        .primary_pid_for_live_pane_process(&worker.pane_id)
        .unwrap();
    let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
    service.stop_agent_turn_for_pane(&worker.pane_id).unwrap();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage_and_transcript_source(
            &worker.pane_id,
            "independent-routed-binding".to_string(),
            0,
            None,
            None,
            0,
        )
        .unwrap();
    let independent = service
        .start_agent_prompt_turn(&worker.pane_id, "independent routed binding work")
        .unwrap();
    let independent_state = service
        .agent_turn_ledger()
        .turn(&independent.turn_id)
        .unwrap()
        .state;
    let identity_id = AgentId::opaque(worker.agent_id.clone()).unwrap();
    let identity = service
        .message_service()
        .registered_identity(&identity_id)
        .unwrap()
        .clone();
    let primary = service.session.layout_owner_client_id().unwrap().clone();
    service
        .dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#)
        .unwrap();
    assert!(service.find_pane_descriptor(&worker.pane_id).is_some());
    assert_eq!(
        service.primary_pid_for_live_pane_process(&worker.pane_id),
        Some(pid)
    );
    assert_eq!(
        mez_mux::process::process_start_token_for_pid(pid),
        Some(token)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&worker.pane_id)
            .unwrap()
            .session_id,
        "independent-routed-binding"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&independent.turn_id)
            .unwrap()
            .state,
        independent_state
    );
    assert_eq!(
        service.message_service().registered_identity(&identity_id),
        Some(&identity)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A fork/new continuation is deferred until its work turn settles, but that
/// settlement still owns its recovery. A continuation that cannot start
/// restores the invoking parent before the failure explanation; if that
/// explanation fails before child release, explicit parent close must retain
/// the original native obligation.
#[test]
fn runtime_routed_cleanup_failed_fresh_iteration_retains_recovery_obligation() {
    for mode in ["--fork", "--new"] {
        let (mut service, parent_turn_id, worker) =
            selected_routed_loop(&format!("/loop {mode} --limit 3 inspect cleanup recovery"));
        let (grandchild, grandchild_native) =
            spawn_routed_cleanup_grandchild(&mut service, &worker);
        let durable = service
            .agent_loop_state(&worker.pane_id)
            .unwrap()
            .parent_conversation_id
            .clone();
        let pid = service
            .primary_pid_for_live_pane_process(&worker.pane_id)
            .unwrap();
        let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
        service.fail_next_routed_loop_continuation_queue_for_tests();
        service.fail_routed_cleanup_recovery_before_release_for_tests();
        service
            .emit_subagent_task_result_for_execution(&worker, &routed_patch_execution(&worker))
            .unwrap();
        let error = service
            .complete_running_agent_turn_and_start_ready(
                &worker,
                AgentTurnState::Completed,
                "routed_loop_recovery_failure",
            )
            .unwrap_err();
        assert!(
            error.message().contains("injected routed recovery failure"),
            "{error}"
        );
        assert_eq!(
            service.agent_shell_store().get("%1").unwrap().session_id,
            durable
        );
        assert!(service.find_pane_descriptor(&worker.pane_id).is_some());
        assert_eq!(
            service.primary_pid_for_live_pane_process(&worker.pane_id),
            Some(pid)
        );
        assert_eq!(
            mez_mux::process::process_start_token_for_pid(pid),
            Some(token)
        );
        assert_eq!(
            service
                .routed_workflow_for_tests(&parent_turn_id)
                .unwrap()
                .child_turn_id
                .as_deref(),
            Some(worker.turn_id.as_str())
        );
        assert!(service.find_pane_descriptor(&grandchild).is_some());
        assert_eq!(
            mez_mux::process::process_start_token_for_pid(grandchild_native.0),
            Some(grandchild_native.1)
        );
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        let result =
            service.dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#);
        let child_present = service.find_pane_descriptor(&worker.pane_id).is_some();
        let after = mez_mux::process::process_start_token_for_pid(pid);
        assert!(service.find_pane_descriptor(&grandchild).is_none());
        assert_routed_process_retired(grandchild_native);
        service.terminate_all_pane_processes().unwrap();
        result.unwrap();
        assert!(
            !child_present && after != Some(token),
            "{mode} recovery child_present={child_present} native_after={after:?} expected={token}"
        );
    }
}

/// Successful limit-one restoration keeps the routed child unfenced for its
/// handoff. A close while that handoff is outstanding still retires its actual
/// native process. Current-binding fencing must also select the same original
/// obligation after that owned restoration, without rewriting its attribution.
#[test]
fn runtime_routed_cleanup_restored_parent_handoff_retains_unfenced_obligation() {
    for mode in ["--fork", "--new"] {
        let (mut service, parent_turn_id, worker) =
            selected_routed_loop(&format!("/loop {mode} --limit 1 inspect cleanup handoff"));
        let (grandchild, grandchild_native) =
            spawn_routed_cleanup_grandchild(&mut service, &worker);
        let durable = service
            .agent_loop_state(&worker.pane_id)
            .unwrap()
            .parent_conversation_id
            .clone();
        let pid = service
            .primary_pid_for_live_pane_process(&worker.pane_id)
            .unwrap();
        let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
        service
            .emit_subagent_task_result_for_execution(&worker, &routed_patch_execution(&worker))
            .unwrap();
        assert_eq!(
            service.agent_shell_store().get("%1").unwrap().session_id,
            durable
        );
        assert_eq!(
            service
                .routed_workflow_for_tests(&parent_turn_id)
                .unwrap()
                .phase,
            mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForHandoff
        );
        assert!(!service.subagent_descendant_is_fenced(&worker.agent_id));
        let previous =
            service.fence_subagent_descendants_for_parent_conversation("agent-%1", &durable);
        assert!(
            previous.contains_key(&worker.agent_id),
            "current qualified parent binding must select its original obligation"
        );
        assert!(service.subagent_descendant_is_fenced(&worker.agent_id));
        service.restore_subagent_descendant_fences(previous);
        assert!(!service.subagent_descendant_is_fenced(&worker.agent_id));
        assert!(service.find_pane_descriptor(&grandchild).is_some());
        assert_eq!(
            mez_mux::process::process_start_token_for_pid(grandchild_native.0),
            Some(grandchild_native.1)
        );
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        let result =
            service.dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#);
        let child_present = service.find_pane_descriptor(&worker.pane_id).is_some();
        let after = mez_mux::process::process_start_token_for_pid(pid);
        assert!(service.find_pane_descriptor(&grandchild).is_none());
        assert_routed_process_retired(grandchild_native);
        service.terminate_all_pane_processes().unwrap();
        result.unwrap();
        assert!(
            !child_present && after != Some(token),
            "{mode} handoff child_present={child_present} native_after={after:?} expected={token}"
        );
    }
}

/// This component seam starts with an actual selected workflow/native child
/// and tests only its exact owned binding-proof transaction. It does not claim
/// a successful second fork/new iteration (separate issue 5ecc7f25). The lower
/// restore requires the exact running turn rather than clearing turn ownership.
/// A replaced native parent rejects the stale token, restores the prior shell,
/// and preserves the now-independent child through later parent close.
#[test]
fn runtime_routed_cleanup_owned_binding_seam_requires_original_native_edge() {
    for replace_parent in [false, true] {
        let (mut service, _, worker) = selected_routed_loop_with_parent(
            "/loop --fork --limit 3 inspect owned binding seam",
            crate::runtime::config::ShellMode::Native,
            true,
        );
        let state = service.agent_loop_state(&worker.pane_id).unwrap().clone();
        let prepared = service.prepare_routed_loop_cleanup_rebind(&state).unwrap();
        let previous = service
            .agent_shell_store()
            .get(&worker.pane_id)
            .unwrap()
            .clone();
        let pid = service
            .primary_pid_for_live_pane_process(&worker.pane_id)
            .unwrap();
        let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
        service
            .agent_shell_store_mut()
            .restore_conversation_for_running_turn_with_lineage(
                &worker.pane_id,
                &worker.turn_id,
                "owned-routed-component-binding",
                0,
                None,
            )
            .unwrap();
        if replace_parent {
            service.terminate_runtime_pane_process("%1", true).unwrap();
            service.start_initial_pane_process(Some("cat")).unwrap();
        }
        let committed =
            service.commit_routed_cleanup_rebind(prepared, "owned-routed-component-binding");
        if replace_parent {
            assert!(committed.is_err());
            service
                .agent_shell_store_mut()
                .restore_session(&worker.pane_id, previous.clone())
                .unwrap();
            assert_eq!(
                service.agent_shell_store().get(&worker.pane_id).unwrap(),
                &previous
            );
        } else {
            committed.unwrap();
        }
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        let result =
            service.dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#);
        let present = service.find_pane_descriptor(&worker.pane_id).is_some();
        let after = mez_mux::process::process_start_token_for_pid(pid);
        if replace_parent {
            assert!(present);
            assert_eq!(after, Some(token));
            assert_eq!(
                service.primary_pid_for_live_pane_process(&worker.pane_id),
                Some(pid)
            );
            assert_eq!(
                service
                    .agent_shell_store()
                    .get(&worker.pane_id)
                    .unwrap()
                    .session_id,
                previous.session_id
            );
        }
        service.terminate_all_pane_processes().unwrap();
        result.unwrap();
        if !replace_parent {
            assert!(
                !present && after != Some(token),
                "owned migration child_present={present} native_after={after:?} expected={token}"
            );
        }
    }
}
