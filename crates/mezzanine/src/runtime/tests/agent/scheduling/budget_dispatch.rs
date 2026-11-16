//! Runtime-pump boundary for the scheduler's orthogonal budget reason.
//! This deliberately supplies the scheduler reason, not policy/receipt evidence:
//! root qualification, effect fences, and policy-driven release are downstream.

use super::*;

#[test]
fn runtime_scheduler_budget_release_does_not_clear_human_pause_or_replace_turn() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let other = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane in ["%1", other.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane.to_string(), screen);
    }
    let original = service
        .start_agent_prompt_turn("%1", "issued owner")
        .unwrap();
    let held = service
        .start_agent_prompt_turn("%1", "retained queued prompt")
        .unwrap();
    let healthy = service
        .start_agent_prompt_turn(other.as_str(), "independent progress")
        .unwrap();
    assert!(
        service
            .agent_scheduler_mut()
            .set_budget_exhausted(&held.turn_id, true)
    );
    service
        .agent_scheduler_mut()
        .set_human_paused(&held.turn_id, true);
    service
        .agent_scheduler_mut()
        .complete(&original.turn_id)
        .unwrap();
    service
        .finish_agent_turn("%1", &original.turn_id, AgentTurnState::Completed)
        .unwrap();
    assert!(
        service
            .agent_scheduler()
            .running_turns()
            .any(|work| work.turn_id == healthy.turn_id)
    );
    service
        .agent_scheduler_mut()
        .complete(&healthy.turn_id)
        .unwrap();
    service
        .finish_agent_turn(other.as_str(), &healthy.turn_id, AgentTurnState::Completed)
        .unwrap();

    assert!(
        service
            .agent_scheduler_mut()
            .set_budget_exhausted(&held.turn_id, false)
    );
    assert_eq!(service.start_ready_agent_turns().unwrap(), 0);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&held.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Queued
    );
    service
        .agent_scheduler_mut()
        .set_human_paused(&held.turn_id, false);
    assert_eq!(service.start_ready_agent_turns().unwrap(), 1);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&held.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert_eq!(service.start_ready_agent_turns().unwrap(), 0);
    assert_eq!(service.agent_turn_ledger().turns().len(), 3);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
}
