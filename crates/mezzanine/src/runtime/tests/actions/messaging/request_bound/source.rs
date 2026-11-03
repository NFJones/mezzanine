//! Original semantic-source custody before physical worker issuance.
//!
//! These are controller metadata and actual lower reservation controls, not disk
//! durability or provider-claim proofs. Actor tests separately run real planning,
//! private SQLite and worker joins. No accepted fact is fabricated here.

use super::super::fixtures::messaging_test_execution;
use super::*;

/// Captures one genuinely planned allowed action in its original chronology.
/// Only unissued admission/cancellation is exercised by this metadata fixture.
fn queued(
    tracked: bool,
) -> (
    RuntimeSessionService,
    AgentTurnRecord,
    crate::runtime::agent::RuntimeMessageActionSource,
    mez_core::ids::ClientId,
) {
    let (mut service, parent, peer, primary) = fixture_with_primary();
    service.enable_stored_message_actions().unwrap();
    let action = AgentAction {
        id: "queued-source".into(),
        payload: AgentActionPayload::SendMessage {
            recipient: format!("agent:{}", peer.agent_id),
            scope: Some("session".into()),
            content_type: "text/plain; charset=utf-8".into(),
            payload: "original metadata-only source".into(),
            correlation_id: None,
            expect_reply: tracked,
        },
    };
    let planned = mez_agent::plan_action_result(
        &parent,
        &action,
        mez_agent::ActionPlanningInput {
            approval_policy: mez_agent::ApprovalPolicy::AutoAllow,
            message_rule_decision: Some(mez_agent::permissions::RuleDecision::Allow),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(planned.status, ActionStatus::Running);
    let mut execution =
        messaging_test_execution(&parent, &action, planned, AgentTurnState::Running);
    service
        .append_agent_execution_chronology(&parent, &execution)
        .unwrap();
    service
        .execute_running_message_actions_for_turn(&parent, &mut execution)
        .unwrap();
    let source = service.pending_stored_message_actions().pop().unwrap();
    (service, parent, source, primary)
}

/// Prepared originals cannot be called rejected before their exact cancellation.
/// Re-selection retains semantic permission but allocates a new preparation; an
/// old cancelled capability cannot cancel its replacement or free its budget.
#[test]
fn native_message_source_preparation_reselection_requires_exact_allocation() {
    for tracked in [false, true] {
        let (mut service, _, source, _) = queued(tracked);
        let first = service
            .prepare_stored_message_action(&source, current_unix_millis())
            .unwrap();
        assert!(service.message_service().has_unpublished_acceptance());
        assert!(
            service
                .reject_queued_stored_message_action(
                    &source,
                    mez_agent::messaging::MessageError::invalid_state("not absence")
                )
                .is_err()
        );
        service.cancel_stored_message_preparation(&first).unwrap();
        assert!(!service.message_service().has_unpublished_acceptance());
        let second = service
            .prepare_stored_message_action(&source, current_unix_millis())
            .unwrap();
        let usage = service.tracked_message_request_usage();
        assert!(service.cancel_stored_message_preparation(&first).is_err());
        assert_eq!(service.tracked_message_request_usage(), usage);
        assert!(service.message_service().has_unpublished_acceptance());
        service.cancel_stored_message_preparation(&second).unwrap();
        assert!(!service.message_service().has_unpublished_acceptance());
    }
}

/// A trusted project-authority change cannot move an unissued original grant to
/// the new sender binding even when its public ID and numeric floor are stable.
/// No lower occurrence or receiver input is admitted by this rejected selection.
#[test]
fn native_message_source_rejects_sender_authority_rebinding_before_selection() {
    let (mut service, parent, source, _) = queued(true);
    let sender = AgentId::opaque(parent.agent_id.clone()).unwrap();
    service
        .message_service_mut()
        .rebind_agent_project_scope(
            &sender,
            Some(
                mez_agent::messaging::ProjectScopeId::from_canonical_root_bytes(
                    b"/different/source-project",
                ),
            ),
        )
        .unwrap();
    assert!(
        service
            .prepare_stored_message_action(&source, current_unix_millis())
            .is_err()
    );
    assert!(!service.message_service().has_unpublished_acceptance());
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
}

/// Snapshot replacement must fence queued semantics before any lower reservation.
/// Real Stop cancels both a queued source and a prepared never-issued original;
/// no private capability or lower charge may survive as replacement authority.
#[test]
fn native_message_source_restore_fence_precedes_reservation_and_stop_cancels_prepared() {
    for prepared in [false, true] {
        let (mut service, parent, source, primary) = queued(true);
        let admission = prepared.then(|| {
            service
                .prepare_stored_message_action(&source, current_unix_millis())
                .unwrap()
        });
        let before = service.message_service().snapshot_state().unwrap();
        assert!(
            service
                .retire_runtime_message_transfers_for_restore()
                .is_err()
        );
        assert_eq!(service.message_service().snapshot_state().unwrap(), before);
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(!service.message_service().has_unpublished_acceptance());
        assert!(service.pending_stored_message_actions().is_empty());
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        if let Some(admission) = admission {
            assert!(
                service
                    .cancel_stored_message_preparation(&admission)
                    .is_err()
            );
        }
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Interrupted
        );
    }
}

/// A completed original keeps its exact wake when scheduler admission is full,
/// then reacquires capacity without repeating acceptance when the limit changes.
/// This controlled lower port exercises scheduler ownership only; the separate
/// actor tests prove the actual SQLite commit and worker lifecycle.
#[test]
fn native_message_source_keeps_original_continuation_when_ready_queue_is_full() {
    use mez_agent::messaging::{
        MessageAcceptanceStore, MessageAcceptanceStoreError, MessageAcceptanceWrite,
        MessageAcceptanceWriteOutcome,
    };
    struct Commit;
    impl MessageAcceptanceStore for Commit {
        /// Qualifies one lower test fact; this is not a disk durability proof.
        fn commit(
            &mut self,
            _: &MessageAcceptanceWrite,
        ) -> std::result::Result<(), MessageAcceptanceStoreError> {
            Ok(())
        }
    }

    let (mut service, parent, source, _) = queued(false);
    service
        .agent_scheduler_mut()
        .wait_running(&parent.turn_id)
        .unwrap();
    service
        .agent_turn_ledger_mut()
        .finish_turn(&parent.turn_id, AgentTurnState::Blocked)
        .unwrap();
    service
        .configure_agent_scheduler_queue_limits(1, 1)
        .unwrap();
    let admission = service
        .prepare_stored_message_action(&source, current_unix_millis())
        .unwrap();
    let mut commit = None;
    service
        .start_stored_message_action(&admission, |messages, ticket, requests| {
            assert!(requests.is_none());
            let write = messages.begin_acceptance_write(ticket)?;
            let MessageAcceptanceWriteOutcome::Committed(fact) = write.persist(&mut Commit) else {
                panic!("lower test port must qualify the original")
            };
            commit = Some(fact);
            Ok(())
        })
        .unwrap();
    let fact = commit.unwrap();
    service
        .adopt_stored_message_action(&admission, |messages, _| {
            Ok(messages.publish_acceptance(&fact)?.0)
        })
        .unwrap();
    service.project_stored_message_action(&source).unwrap();
    assert!(service.stored_message_continuation_needs_wake());
    service.recover_stored_message_continuations().unwrap();
    assert!(service.stored_message_continuation_needs_wake());
    assert!(
        service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == parent.turn_id)
    );
    let accepted = service
        .message_service()
        .snapshot_state()
        .unwrap()
        .accepted_messages
        .len();
    service
        .configure_agent_scheduler_queue_limits(1000, 1024 * 1024)
        .unwrap();
    service.recover_stored_message_continuations().unwrap();
    assert!(!service.stored_message_continuation_needs_wake());
    assert!(
        !service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == parent.turn_id)
    );
    service
        .adopt_stored_message_action(&admission, |_, _| panic!("recovery must not republish"))
        .unwrap();
    assert_eq!(
        service
            .message_service()
            .snapshot_state()
            .unwrap()
            .accepted_messages
            .len(),
        accepted
    );
}
