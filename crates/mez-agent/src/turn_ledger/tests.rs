//! Agent tests for turn ledger behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

/// Builds one complete turn record for lower ledger regressions.
fn turn() -> AgentTurnRecord {
    AgentTurnRecord {
        turn_id: "turn-1".to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: "agent-1".to_string(),
        pane_id: "%1".to_string(),
        trigger: AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: AgentTurnState::Queued,
        cooperation_mode: None,
        initial_capability: None,
    }
}

#[test]
/// Verifies terminal turn states are immutable once recorded in the ledger. A
/// failed, completed, or interrupted turn must not later be reclassified by a
/// duplicate finish path because scheduler, transcript, and metrics callers all
/// rely on the first terminal result as the authoritative turn outcome.
fn agent_turn_ledger_rejects_duplicate_terminal_finish() {
    let mut ledger = AgentTurnLedger::new(false);
    ledger.start_turn(turn()).unwrap();
    ledger
        .finish_turn("turn-1", AgentTurnState::Failed)
        .unwrap();

    let error = ledger
        .finish_turn("turn-1", AgentTurnState::Completed)
        .unwrap_err();

    assert_eq!(error.message(), "agent turn is already terminal");
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Failed);
}

#[test]
/// Verifies direct turn starts reject duplicate identifiers across all ledger
/// states so lifecycle recovery cannot create orphaned records. The regression
/// covers a previously reported defense-in-depth gap where `start_turn` could
/// have appended a reused turn id while later lifecycle APIs updated only the
/// first matching record.
fn agent_turn_ledger_start_turn_rejects_duplicate_turn_id() {
    let mut ledger = AgentTurnLedger::new(true);
    let mut duplicate = turn();
    duplicate.agent_id = "agent-other".to_string();

    ledger.start_turn(turn()).unwrap();

    let error = ledger.start_turn(duplicate).unwrap_err();

    assert_eq!(error.message(), "agent turn id already exists");
    assert_eq!(ledger.turns().len(), 1);
}

#[test]
/// Verifies that completed turn records are retained within a large bounded
/// window while active work remains represented by the ledger. Long-lived
/// sessions can complete many agent turns, and the ledger should not retain all
/// historical terminal records forever.
fn turn_ledger_bounds_terminal_turn_retention() {
    let mut ledger = AgentTurnLedger::new(false);

    for index in 0..4100 {
        let turn_id = format!("turn-{index}");
        ledger
            .start_turn(AgentTurnRecord {
                turn_id: turn_id.clone(),
                conversation_id: "conversation-1".to_string(),
                ..turn()
            })
            .unwrap();
        ledger
            .finish_turn(&turn_id, AgentTurnState::Completed)
            .unwrap();
    }

    assert_eq!(ledger.turns().len(), 4096);
    assert_eq!(ledger.turns()[0].turn_id, "turn-4");
    assert_eq!(ledger.turns()[4095].turn_id, "turn-4099");
    assert!(ledger.turn("turn-3").is_none());
    assert_eq!(
        ledger.turn("turn-4").map(|turn| turn.turn_id.as_str()),
        Some("turn-4")
    );
    assert_eq!(
        ledger
            .latest_turn_for_pane("%1")
            .map(|turn| turn.turn_id.as_str()),
        Some("turn-4099")
    );
}

#[test]
/// Verifies indexed turn, pane, and running-count queries follow every state
/// transition without scanning the retained terminal history.
fn turn_ledger_indexes_track_lifecycle_transitions() {
    let mut ledger = AgentTurnLedger::new(true);
    let initial_generation = ledger.semantic_generation();
    ledger.queue_turn(turn()).unwrap();
    let mut second = turn();
    second.turn_id = "turn-2".to_string();
    second.pane_id = "%2".to_string();
    ledger.start_turn(second).unwrap();

    assert!(ledger.turn("turn-1").is_some());
    assert!(!ledger.turn_is_running("turn-1"));
    assert!(ledger.turn_is_running("turn-2"));
    assert_eq!(ledger.running_turn_count_for_panes(["%1", "%2"]), 1);
    assert!(ledger.semantic_generation() > initial_generation);

    ledger.mark_turn_running("turn-1").unwrap();
    assert_eq!(ledger.running_turn_count_for_panes(["%1", "%2"]), 2);
    ledger
        .finish_turn("turn-1", AgentTurnState::Blocked)
        .unwrap();
    assert!(!ledger.turn_is_running("turn-1"));
    assert_eq!(ledger.running_turn_count_for_panes(["%1", "%2"]), 1);
    ledger.resume_blocked_turn("turn-1").unwrap();
    assert!(ledger.turn_is_running("turn-1"));
    ledger
        .finish_turn("turn-2", AgentTurnState::Completed)
        .unwrap();

    assert_eq!(ledger.running_turn_count_for_panes(["%1", "%2"]), 1);
    assert_eq!(
        ledger.latest_turn_for_pane("%2").map(|turn| turn.state),
        Some(AgentTurnState::Completed)
    );
}

#[test]
/// Verifies turn ledger serializes turns for one agent.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_ledger_serializes_turns_for_one_agent() {
    let mut ledger = AgentTurnLedger::new(false);
    ledger.start_turn(turn()).unwrap();

    let error = ledger.start_turn(AgentTurnRecord {
        turn_id: "turn-2".to_string(),
        conversation_id: "conversation-1".to_string(),
        ..turn()
    });

    assert_eq!(
        error.unwrap_err().kind(),
        crate::AgentTurnLedgerErrorKind::Conflict
    );

    ledger
        .finish_turn("turn-1", AgentTurnState::Completed)
        .unwrap();
    ledger
        .start_turn(AgentTurnRecord {
            turn_id: "turn-2".to_string(),
            conversation_id: "conversation-1".to_string(),
            ..turn()
        })
        .unwrap();

    assert_eq!(ledger.turns().len(), 2);
    assert_eq!(ledger.turns()[1].state, AgentTurnState::Running);
}
