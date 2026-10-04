//! Ordinary receipt identity and settlement regressions using real runtime ingress.

use super::*;

/// Kernel identity can change between observations within one actor turn.
/// Final transfer must not reserve a replacement-root owner, and failed root
/// admission must settle rather than restore the original queue as pending.
#[test]
fn steering_receipts_successive_root_observations_cannot_rebind_or_restore() {
    use crate::runtime::processes::{
        RuntimePaneProcessIdentityInjection as Injection, RuntimePaneProcessIdentityUnavailable,
    };
    for phase in ["transfer", "admission-replaced", "admission-unavailable"] {
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
        service.start_initial_pane_process(Some("cat")).unwrap();
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let epoch = service.agent_compaction_epoch("%1");
        service
            .queue_agent_compaction_steering(
                "%1",
                primary,
                conversation.clone(),
                epoch,
                "accepted".into(),
                "display".into(),
            )
            .unwrap();
        let entry = service.agent.agent_compaction_steering["%1"][0].clone();
        let original = entry.receipt.process.as_ref().unwrap();
        let identity = |start_token| Injection::Identity {
            role: original.role,
            generation: original.generation,
            process_id: original.process_id,
            start_token,
            executable_path: original.executable_path.clone(),
            live_start_token: None,
        };
        service.inject_pane_process_identity_for_tests("%1", identity(original.start_token));
        service.inject_pane_process_identity_for_tests(
            "%1",
            if phase == "admission-unavailable" {
                Injection::Unavailable(RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable)
            } else {
                identity(original.start_token + 1)
            },
        );
        if phase == "transfer" {
            let turn = AgentTurnRecord {
                turn_id: "transfer-test".into(),
                conversation_id: conversation,
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                trigger: mez_agent::AgentTurnTrigger::UserPrompt,
                started_at_unix_seconds: 1,
                deadline_at_unix_millis: 0,
                policy_profile: "runtime".into(),
                model_profile: "default".into(),
                parent_turn_id: None,
                state: mez_agent::AgentTurnState::Queued,
                cooperation_mode: None,
                initial_capability: None,
            };
            let result = service.check_deferred_steering_receipts(&turn, 1, &[entry.receipt]);
            service.terminate_all_pane_processes().unwrap();
            assert!(result.is_err());
            assert!(!service.agent.steering_receipts.contains_key(&turn.turn_id));
        } else {
            let result = service.resume_agent_compaction_steering("%1");
            service.terminate_all_pane_processes().unwrap();
            assert!(result.is_ok_and(|started| !started));
            assert!(service.take_agent_compaction_steering("%1").is_empty());
            let settled =
                &service.agent.settled_compaction_steering[&("%1".into(), conversation, epoch)];
            assert_eq!(settled[0].id, entry.receipt.id);
            assert_eq!(settled[0].status, Status::NotSent);
            assert!(service.take_pending_agent_prompt_history().is_empty());
        }
    }
}

/// Deferred guidance keeps its original root through queue release, history
/// claim and completion. A replacement must settle the same not-sent occurrence
/// without a new turn, even if conversation and command ownership still match.
#[test]
fn steering_receipts_deferred_root_replacement_fences_each_transition() {
    use crate::runtime::processes::RuntimePaneProcessIdentityInjection;
    for phase in ["queue", "claim", "complete", "unstarted"] {
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
        if phase != "unstarted" {
            service.start_initial_pane_process(Some("cat")).unwrap();
        }
        service.mark_agent_compacting_for_tests("%1", 1);
        service
            .execute_agent_shell_command(&primary, "accepted guidance")
            .unwrap();
        let entry = service.agent.agent_compaction_steering["%1"][0].clone();
        let replacement = entry.receipt.process.as_ref().map(|original| {
            RuntimePaneProcessIdentityInjection::Identity {
                role: original.role,
                generation: original.generation,
                process_id: original.process_id,
                start_token: original.start_token + 1,
                executable_path: original.executable_path.clone(),
                live_start_token: None,
            }
        });
        if phase == "queue" {
            service.inject_pane_process_identity_for_tests("%1", replacement.unwrap());
            service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            assert!(service.take_pending_agent_prompt_history().is_empty());
            let settled = &service.agent.settled_compaction_steering
                [&("%1".into(), entry.conversation, entry.epoch)];
            assert_eq!(settled[0].id, entry.receipt.id);
            assert_eq!(settled[0].status, Status::NotSent);
        } else {
            service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            let dispatch = service.take_pending_agent_prompt_history().remove(0);
            if phase == "complete" {
                assert!(service.claim_agent_prompt_history_preparation(&dispatch));
            }
            if phase == "unstarted" {
                service.start_initial_pane_process(Some("cat")).unwrap();
            } else {
                service.inject_pane_process_identity_for_tests("%1", replacement.unwrap());
            }
            if phase == "complete" {
                let history = crate::runtime::execute_runtime_agent_prompt_history_work(
                    dispatch.history_work.clone(),
                );
                assert!(
                    !service
                        .complete_agent_prompt_history_preparation(&dispatch, history)
                        .unwrap()
                );
            } else {
                assert!(!service.claim_agent_prompt_history_preparation(&dispatch));
            }
            let settled = service.settled_deferred_receipts_for_tests(&dispatch);
            assert_eq!(settled[0].id, entry.receipt.id);
            assert_eq!(settled[0].status, Status::NotSent);
        }
        assert!(service.agent_turn_ledger().turns().is_empty());
        assert!(service.pending_agent_provider_tasks().is_empty());
        service.terminate_all_pane_processes().unwrap();
    }
}

/// A readable foreground fallback cannot establish an unreadable pane root.
/// Reject the first occurrence before reserving an owner or inserting input.
#[test]
fn steering_receipts_foreground_fallback_cannot_bind_root() {
    use crate::runtime::processes::{RuntimePaneProcessIdentityInjection, RuntimePaneProcessRole};
    let (mut service, turn) = fixture();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let before = service.agent_turn_contexts()[&turn.turn_id].clone();
    service.inject_pane_process_identity_for_tests(
        "%1",
        RuntimePaneProcessIdentityInjection::Identity {
            role: RuntimePaneProcessRole::ForegroundProcessGroupLeader,
            generation: None,
            process_id: 12345,
            start_token: 1,
            executable_path: "/bin/sh".into(),
            live_start_token: None,
        },
    );
    let rejected = service
        .inject_agent_steering_with_display("%1", "must reject", "display")
        .is_err();
    service.terminate_all_pane_processes().unwrap();
    assert!(rejected);
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], before);
    assert!(!service.agent.steering_receipts.contains_key(&turn.turn_id));
}

/// An existing root must remain the same kernel incarnation between accepted
/// guidance occurrences. Replacement and unreadable evidence reject before
/// chronology changes; an in-place executable change is not a new incarnation.
#[test]
fn steering_receipts_existing_root_replacement_rejects_before_insertion() {
    use crate::runtime::processes::{
        RuntimePaneProcessIdentityInjection, RuntimePaneProcessIdentityUnavailable,
    };
    let (mut service, turn) = fixture();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .inject_agent_steering_with_display("%1", "accepted", "display")
        .unwrap();
    let original = service.agent.steering_receipts[&turn.turn_id]
        .process
        .clone()
        .unwrap();
    let mut exec = original.clone();
    exec.executable_path = "/different/in-place-executable".into();
    assert!(RuntimeSessionService::steering_process_matches(
        Some(&original),
        Some(&exec)
    ));
    let before = service.agent_turn_contexts()[&turn.turn_id].clone();
    service.inject_pane_process_identity_for_tests(
        "%1",
        RuntimePaneProcessIdentityInjection::Identity {
            role: original.role,
            generation: original.generation,
            process_id: original.process_id,
            start_token: original.start_token + 1,
            executable_path: original.executable_path.clone(),
            live_start_token: None,
        },
    );
    assert!(
        service
            .inject_agent_steering_with_display("%1", "replaced", "replaced")
            .is_err()
    );
    service.inject_pane_process_identity_for_tests(
        "%1",
        RuntimePaneProcessIdentityInjection::Unavailable(
            RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable,
        ),
    );
    assert!(
        service
            .inject_agent_steering_with_display("%1", "unreadable", "unreadable")
            .is_err()
    );
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], before);
    assert_eq!(service.steering_receipts_for_tests(&turn.turn_id).len(), 1);
    service.terminate_all_pane_processes().unwrap();
}

/// Pre-history discard, stale epoch filtering and shutdown all consume the
/// actor's accepted queue and retain exact not-sent occurrence evidence. None
/// may start a turn, join the input again or lose its independent display source.
#[test]
fn steering_receipts_compaction_queue_teardown_preserves_occurrences() {
    for mode in ["discard", "stale", "shutdown"] {
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
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let epoch = service.agent_compaction_epoch("%1");
        let queued_epoch = if mode == "stale" { epoch + 1 } else { epoch };
        for display in ["first display", "second display"] {
            service
                .queue_agent_compaction_steering(
                    "%1",
                    primary.clone(),
                    conversation.clone(),
                    queued_epoch,
                    "same".into(),
                    display.into(),
                )
                .unwrap();
        }
        let ids = service.agent.agent_compaction_steering["%1"]
            .iter()
            .map(|entry| entry.receipt.id.clone())
            .collect::<Vec<_>>();
        match mode {
            "discard" => service.discard_agent_compaction_steering("%1"),
            "stale" => assert!(!service.resume_agent_compaction_steering("%1").unwrap()),
            _ => {
                service.agent.cancel_all_agent_commands();
            }
        }
        assert!(service.take_agent_compaction_steering("%1").is_empty());
        let settled =
            &service.agent.settled_compaction_steering[&("%1".into(), conversation, queued_epoch)];
        assert_eq!(
            settled
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert_ne!(ids[0], ids[1]);
        assert_eq!(settled[1].display, "second display");
        assert!(
            settled
                .iter()
                .all(|entry| entry.status == Status::NotSent && entry.sequence == 0)
        );
        assert!(service.agent_turn_ledger().turns().is_empty());
        assert!(service.take_pending_agent_prompt_history().is_empty());
        assert!(service.pending_agent_provider_tasks().is_empty());
        service.discard_agent_compaction_steering("%1");
        assert_eq!(
            service
                .agent
                .settled_compaction_steering
                .values()
                .map(Vec::len)
                .sum::<usize>(),
            2
        );
    }
}

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
