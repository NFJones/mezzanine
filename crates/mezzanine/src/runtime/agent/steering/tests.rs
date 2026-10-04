//! Ordinary receipt identity and settlement regressions using real runtime ingress.

use super::*;

/// Queued and claimed history cancellation settles accepted occurrences on the
/// actor without a worker reply. Late callbacks cannot recreate evidence after
/// terminal retention eviction or disturb a replacement command owner.
#[test]
fn steering_receipts_history_cancellation_is_actor_owned() {
    for claimed in [false, true] {
        for shutdown in [false, true] {
            let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
            let primary = service
                .attach_primary(
                    "primary",
                    true,
                    mez_mux::layout::Size::new(80, 24).unwrap(),
                    120,
                )
                .unwrap();
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            service.mark_agent_compacting_for_tests("%1", 1);
            service
                .execute_agent_shell_command(&primary, "accepted guidance")
                .unwrap();
            service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            let dispatch = service.take_pending_agent_prompt_history().remove(0);
            if claimed {
                assert!(service.claim_agent_prompt_history_preparation(&dispatch));
            }
            if shutdown {
                service.agent.cancel_all_agent_commands();
            } else {
                assert!(service.agent.cancel_agent_command("%1"));
            }
            let receipts = service.settled_deferred_receipts_for_tests(&dispatch);
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].id, dispatch.steering_receipts[0].id);
            assert_eq!(receipts[0].status, Status::NotSent);
            assert!(service.agent.pending_deferred_steering.is_empty());
            service.agent.settled_deferred_steering.clear();
            let replacement = service
                .begin_agent_command_claim("%1", &dispatch.conversation_id)
                .unwrap();
            assert!(replacement > dispatch.claim_generation);
            assert!(
                !service
                    .complete_agent_prompt_history_preparation(
                        &dispatch,
                        Err(MezError::invalid_state("late worker failure"))
                    )
                    .unwrap()
            );
            assert!(
                service
                    .settled_deferred_receipts_for_tests(&dispatch)
                    .is_empty()
            );
            assert!(service.agent_command_is_active("%1"));
            assert!(service.agent_turn_ledger().turns().is_empty());
        }
    }
}

/// Failed history preparation settles the exact accepted occurrences once,
/// without a canonical turn, input replay, or ordinary admission evidence.
#[test]
fn steering_receipts_deferred_history_failure_is_not_sent_once() {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let primary = service
        .attach_primary(
            "primary",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);
    for _ in 0..2 {
        service
            .execute_agent_shell_command(&primary, "same")
            .unwrap();
    }
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    let dispatch = service.take_pending_agent_prompt_history().remove(0);
    assert!(service.claim_agent_prompt_history_preparation(&dispatch));
    assert!(!service.claim_agent_prompt_history_preparation(&dispatch));
    assert!(
        service
            .settled_deferred_receipts_for_tests(&dispatch)
            .is_empty()
    );
    assert!(
        service
            .complete_agent_prompt_history_preparation(
                &dispatch,
                Err(MezError::invalid_state("fixture history failure"))
            )
            .is_err()
    );
    assert!(
        !service
            .complete_agent_prompt_history_preparation(
                &dispatch,
                Err(MezError::invalid_state("duplicate failure"))
            )
            .unwrap()
    );
    let receipts = service.settled_deferred_receipts_for_tests(&dispatch);
    assert_eq!(receipts.len(), 2);
    assert_ne!(receipts[0].id, receipts[1].id);
    assert_eq!(receipts[0].id, dispatch.steering_receipts[0].id);
    assert!(
        receipts
            .iter()
            .all(|entry| entry.status == Status::NotSent && entry.sequence == 0)
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Producer-assigned prompt identity survives equal historical content and
/// interrupted-context reconstruction. Binding must use the newly appended
/// occurrence, never the earlier equal-text event or a snapshot high-water.
#[test]
fn steering_receipts_prompt_identity_tracks_interrupted_remapping() {
    let (mut service, turn) = fixture();
    service
        .inject_agent_steering_with_display("%1", "same", "same")
        .unwrap();
    let previous = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .event_sequence_high_water_mark();
    service.retain_interrupted_agent_continuation(&turn);
    let history = service.runtime_agent_history_epoch_context("%1").unwrap();
    let prepared = service
        .agent_context_for_pane_prompt_with_history("%1", "same", true, history)
        .unwrap();
    let event = prepared
        .context
        .chronology()
        .iter()
        .find(|event| event.sequence() == prepared.prompt_sequence)
        .unwrap();
    assert_eq!(event.block().content, "same");
    assert_eq!(
        event.semantic_kind(),
        mez_agent::ContextSemanticKind::UserEvent
    );
    let (context, continued, _, sequence) = service
        .prepare_interrupted_agent_continuation_context(
            &turn.agent_id,
            &turn.conversation_id,
            prepared.context,
            prepared.imported_history_sequence_high_water,
            prepared.prompt_sequence,
        )
        .unwrap();
    assert!(continued);
    assert!(sequence.get() > previous);
    let event = context
        .chronology()
        .iter()
        .find(|event| event.sequence() == sequence)
        .unwrap();
    assert_eq!(event.block().content, "same");
    assert_eq!(
        context
            .chronology()
            .iter()
            .filter(|event| event.block().content == "same")
            .count(),
        2
    );
}

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
