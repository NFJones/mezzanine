//! Exact provider-persistence settlement ownership regressions.
//!
//! A worker generation identifies an immutable turn/conversation owner, not
//! whichever conversation a pane happens to display when settlement arrives.

use super::*;

/// A still-live persistence generation must not fail a replacement conversation
/// or leave the original turn stranded after its pane binding changes.
#[test]
fn runtime_provider_persistence_failure_rejects_replaced_conversation() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "persist original work")
        .unwrap();
    let generation = service.mark_agent_provider_persistence_pending(&started.turn_id);
    service
        .agent_shell_store_mut()
        .finish_turn("%1", &started.turn_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement-conversation", 0)
        .unwrap();
    let transition = service
        .apply_agent_provider_persistence_failed_transition(
            &started.turn_id,
            generation,
            "test",
            "worker_lost",
            "uncertain writes must not replay",
        )
        .unwrap();
    assert!(
        !transition.applied,
        "stale conversation failure must be rejected"
    );
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        "replacement-conversation"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        0
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
    assert_eq!(
        service.agent_provider_persistence_generation(&started.turn_id),
        None
    );
    assert_replacement_can_start(&mut service, &started.turn_id);
}

/// A forged success identity cannot consume the genuine owner's generation;
/// cancellation then makes both late success and failure inert.
#[tokio::test]
async fn runtime_provider_persistence_mismatched_success_preserves_claim() {
    let (mut service, turn, generation) = persistence_fixture();
    let mut outcome = empty_persistence_outcome(&turn, generation);
    outcome.turn.conversation_id = "forged-conversation".to_string();
    assert!(
        !service
            .apply_agent_provider_persistence_settled_transition(outcome)
            .await
            .unwrap()
            .applied
    );
    assert_eq!(
        service.agent_provider_persistence_generation(&turn.turn_id),
        Some(generation)
    );
    service.stop_agent_turn_for_pane("%1").unwrap();
    assert!(
        !service
            .apply_agent_provider_persistence_settled_transition(empty_persistence_outcome(
                &turn, generation
            ),)
            .await
            .unwrap()
            .applied
    );
    assert!(
        !service
            .apply_agent_provider_persistence_failed_transition(
                &turn.turn_id,
                generation,
                "test",
                "late",
                "uncertain write",
            )
            .unwrap()
            .applied
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
}

/// Success uses the same detached-owner retirement as failure and cannot apply
/// returned action results to the pane's replacement conversation.
#[tokio::test]
async fn runtime_provider_persistence_success_rejects_replaced_conversation() {
    let (mut service, turn, generation) = persistence_fixture();
    service
        .agent_shell_store_mut()
        .finish_turn("%1", &turn.turn_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement", 0)
        .unwrap();
    assert!(
        !service
            .apply_agent_provider_persistence_settled_transition(empty_persistence_outcome(
                &turn, generation
            ),)
            .await
            .unwrap()
            .applied
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        "replacement"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        0
    );
    assert_eq!(
        service.agent_provider_persistence_generation(&turn.turn_id),
        None
    );
    assert_replacement_can_start(&mut service, &turn.turn_id);
}

/// Older worker failures cannot clear a later generation of the same owner.
#[test]
fn runtime_provider_persistence_failure_is_exact_generation_and_once() {
    let (mut service, turn, old) = persistence_fixture();
    let current = service.mark_agent_provider_persistence_pending(&turn.turn_id);
    assert!(
        !service
            .apply_agent_provider_persistence_failed_transition(
                &turn.turn_id,
                old,
                "test",
                "late",
                "old failure",
            )
            .unwrap()
            .applied
    );
    assert_eq!(
        service.agent_provider_persistence_generation(&turn.turn_id),
        Some(current)
    );
    assert!(
        service
            .apply_agent_provider_persistence_failed_transition(
                &turn.turn_id,
                current,
                "test",
                "lost",
                "writes may have committed",
            )
            .unwrap()
            .applied
    );
    assert!(
        !service
            .apply_agent_provider_persistence_failed_transition(
                &turn.turn_id,
                current,
                "test",
                "duplicate",
                "duplicate failure",
            )
            .unwrap()
            .applied
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
}

/// Captures a live turn with one actor-owned persistence generation.
fn persistence_fixture() -> (RuntimeSessionService, AgentTurnRecord, u64) {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "persist original work")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let generation = service.mark_agent_provider_persistence_pending(&turn.turn_id);
    (service, turn, generation)
}

/// Verifies detached cleanup releases capacity and exact agent/pane claims.
fn assert_replacement_can_start(service: &mut RuntimeSessionService, old_turn_id: &str) {
    assert!(
        service
            .agent_scheduler()
            .running_turns()
            .all(|work| work.turn_id != old_turn_id)
    );
    let next = service
        .start_agent_prompt_turn("%1", "start replacement work")
        .unwrap();
    assert!(service.agent_turn_is_running(&next.turn_id));
    assert!(
        service
            .agent_scheduler()
            .running_turns()
            .any(|work| work.turn_id == next.turn_id)
    );
}

/// Builds an outcome whose contents must never be inspected on rejected ownership.
fn empty_persistence_outcome(
    turn: &AgentTurnRecord,
    generation: u64,
) -> crate::runtime::RuntimeAgentProviderPersistenceOutcome {
    crate::runtime::RuntimeAgentProviderPersistenceOutcome {
        turn: turn.clone(),
        generation,
        model_profile: runtime_model_profile("runtime-batch", "test"),
        provider_id: "runtime-batch".to_string(),
        execution: mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: runtime_say_response(&turn.turn_id, "ignored result", true),
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: Vec::new(),
            final_turn: true,
            terminal_state: AgentTurnState::Completed,
        },
        fork_read: None,
        fork_snapshot: None,
        bookkeeping_read: None,
        bookkeeping_history: None,
        memory_results: Vec::new(),
        issue_results: Vec::new(),
        issue_query_freshness: Default::default(),
        issue_records_changed: false,
        actions_executed_before_persistence: 0,
        settled_action_results_before_persistence: Vec::new(),
    }
}
