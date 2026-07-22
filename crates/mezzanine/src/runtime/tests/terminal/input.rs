//! Runtime tests for terminal input behavior.

use super::*;

/// Verifies command-start metadata revokes stale readiness while a turn waits.
///
/// A user command may start after the agent shell is opened while the active
/// turn is still waiting for provider output or shell dispatch. The runtime
/// should suppress queueing/repaint side effects for that command-start event,
/// but it must still record the pane as busy and revoke any manual readiness
/// override so the next agent shell action cannot write into a non-idle pane.
#[test]
fn runtime_osc_command_start_while_turn_waiting_marks_pane_busy() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);
    service.set_pane_readiness("%1", PaneReadinessState::Ready);
    service
        .mark_pane_readiness_override_for_tests("%1", 7, "test override", true)
        .unwrap();

    let observed = service
        .observe_passive_shell_busy("%1", "osc133-command-start")
        .unwrap();

    assert_eq!(observed, 1);
    assert_eq!(service.pane_readiness_state("%1"), PaneReadinessState::Busy);
    assert!(!service.pane_readiness_override_allows_epoch_for_tests("%1", 7));
}
