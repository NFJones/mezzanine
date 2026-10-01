//! Multi-owner product messaging fixtures without independent delivery state.
//!
//! Only pane echo inspection and typed turn/execution construction are shared.
//! One-owner discovery and approval setup remains beside its tests.

use super::*;

/// Returns the agent gutter lines currently visible in one test pane.
pub(super) fn peer_echo_pane_lines(
    service: &crate::runtime::RuntimeSessionService,
    pane_id: &str,
) -> Vec<String> {
    service
        .pane_screen(pane_id)
        .map(|screen| {
            screen
                .normal_content_lines()
                .into_iter()
                .filter(|line| line.starts_with("▐ "))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Returns the ledger turn with the supplied identity.
pub(super) fn messaging_test_turn(
    service: &crate::runtime::RuntimeSessionService,
    turn_id: &str,
) -> mez_agent::AgentTurnRecord {
    service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .expect("messaging test turn")
}

/// Builds an execution carrying one planned action result and terminal state.
pub(super) fn messaging_test_execution(
    turn: &mez_agent::AgentTurnRecord,
    action: &mez_agent::AgentAction,
    result: mez_agent::ActionResult,
    terminal_state: mez_agent::AgentTurnState,
) -> mez_agent::AgentTurnExecution {
    mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "messaging test batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "messaging test batch".to_string(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![result],
        final_turn: false,
        terminal_state,
    }
}
