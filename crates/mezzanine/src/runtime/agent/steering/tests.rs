//! Ordinary receipt identity and settlement regressions using real runtime ingress.

use super::*;

/// Creates a normal active turn without a provider or daemon process.
fn fixture() -> (RuntimeSessionService, AgentTurnRecord) {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "initial").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .unwrap()
        .clone();
    (service, turn)
}

/// Equal accepted text remains two chronological occurrences and two receipts.
/// Partial admission, repeated admission and terminal settlement must preserve
/// exact identities without confusing later equal text with earlier requests.
#[test]
fn steering_receipts_equal_text_partial_admission_and_settlement() {
    let (mut service, turn) = fixture();
    for display in ["first display", "second display"] {
        service
            .inject_agent_steering_with_display("%1", "same", display)
            .unwrap();
    }
    let receipts = service.steering_receipts_for_tests(&turn.turn_id);
    assert_eq!(receipts.len(), 2);
    assert_ne!(receipts[0].id, receipts[1].id);
    assert!(receipts[0].sequence < receipts[1].sequence);
    assert_eq!(receipts[0].input, "same");
    assert_eq!(receipts[1].display, "second display");
    let first = receipts[0].sequence;
    let owner = service
        .agent
        .steering_receipts
        .get_mut(&turn.turn_id)
        .unwrap();
    owner.admit(0, &BTreeSet::from([first]));
    assert_eq!(owner.entries[0].status, Status::Pending);
    owner.admit(7, &BTreeSet::from([first]));
    owner.admit(8, &BTreeSet::from([first]));
    owner.settle();
    assert_eq!(owner.entries[0].status, Status::Admitted(7));
    assert_eq!(owner.entries[1].status, Status::NotSent);
    owner.admit(
        9,
        &owner.entries.iter().map(|entry| entry.sequence).collect(),
    );
    assert_eq!(owner.entries[1].status, Status::NotSent);
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "same")
            .count(),
        2
    );
}

/// Capacity rejection happens before canonical mutation, and a foreign turn
/// cannot use another turn's receipt owner even with matching display text.
#[test]
fn steering_receipts_capacity_rejects_before_canonical_mutation() {
    let (mut service, turn) = fixture();
    for _ in 0..CAPACITY {
        service
            .inject_agent_steering_for_running_turn("%1", "same")
            .unwrap();
    }
    let before = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .clone();
    assert!(
        service
            .inject_agent_steering_for_running_turn("%1", "overflow")
            .is_err()
    );
    assert_eq!(
        service.agent_turn_contexts().get(&turn.turn_id).unwrap(),
        &before
    );
    let mut foreign = turn.clone();
    foreign.conversation_id = "replacement".into();
    assert!(!service.agent.steering_receipts[&turn.turn_id].belongs_to(&foreign));
}

/// Guidance belongs to an already retained blocked or waiting task. Accepting
/// it must preserve the scheduler lane and ledger status without admitting a
/// provider, resuming an approval, or treating text as model-mail wake authority.
#[test]
fn steering_receipts_blocked_and_waiting_guidance_preserves_scheduler_owner() {
    for waiting in [false, true] {
        let (mut service, turn) = fixture();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        if waiting {
            service
                .agent_scheduler_mut()
                .wait_running(&turn.turn_id)
                .unwrap();
        } else {
            service
                .agent_scheduler_mut()
                .block_running(&turn.turn_id)
                .unwrap();
        }
        service
            .agent_turn_ledger_mut()
            .finish_turn(&turn.turn_id, mez_agent::AgentTurnState::Blocked)
            .unwrap();
        let before = service.agent_scheduler().snapshot();
        assert_eq!(
            service
                .inject_agent_steering_with_display("%1", "guidance only", "display guidance",)
                .unwrap(),
            Some(turn.turn_id.clone())
        );
        assert_eq!(service.agent_scheduler().snapshot(), before);
        assert_eq!(service.agent_turn_ledger().turns().len(), 1);
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .state,
            mez_agent::AgentTurnState::Blocked
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert_eq!(
            service.steering_receipts_for_tests(&turn.turn_id)[0].status,
            Status::Pending
        );
    }
}
