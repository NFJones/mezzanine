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
            "genuine routed worker pane survived parent close"
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
        "genuine routed worker pane survived parent exit"
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
    service.terminate_all_pane_processes().unwrap();
}

/// Fork/new continuation is an owned iteration of the same routed delegation,
/// including its newly bound conversation. After a real first result queues
/// iteration two, parent close must retire that same worker process.
#[test]
fn runtime_routed_cleanup_fresh_iteration_parent_close_retires_worker() {
    for mode in ["--fork", "--new"] {
        let (mut service, parent_turn_id, worker) =
            selected_routed_loop(&format!("/loop {mode} --limit 3 inspect cleanup iteration"));
        let pid = service
            .primary_pid_for_live_pane_process(&worker.pane_id)
            .unwrap();
        let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
        service
            .emit_subagent_task_result_for_execution(&worker, &routed_patch_execution(&worker))
            .unwrap();
        service
            .complete_running_agent_turn_and_start_ready(
                &worker,
                AgentTurnState::Completed,
                "actual routed cleanup iteration completed",
            )
            .unwrap();
        let next = service
            .routed_workflow_for_tests(&parent_turn_id)
            .unwrap()
            .child_turn_id
            .clone()
            .unwrap();
        assert_ne!(next, worker.turn_id);
        let next_turn = service.agent_turn_ledger().turn(&next).unwrap();
        assert_ne!(next_turn.conversation_id, worker.conversation_id);
        assert_eq!(
            service.primary_pid_for_live_pane_process(&worker.pane_id),
            Some(pid)
        );
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        let result =
            service.dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#);
        let child_present = service.find_pane_descriptor(&worker.pane_id).is_some();
        let after = mez_mux::process::process_start_token_for_pid(pid);
        service.terminate_all_pane_processes().unwrap();
        result.unwrap();
        assert!(
            !child_present,
            "{mode} worker survived parent close after iteration two"
        );
        assert_ne!(
            after,
            Some(token),
            "{mode} native worker survived parent close"
        );
    }
}
