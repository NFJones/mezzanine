//! Native metadata controls for original tracked storage reservation/adoption.
//! The small trusted lower port qualifies real opaque facts without claiming disk
//! durability; separate actor fixtures exercise actual SQLite and worker joins.

use super::*;
use crate::runtime::RuntimePreparedTrackedMessage;
use mez_agent::messaging::{
    Envelope, MessageAcceptanceCommit, MessageAcceptanceStore, MessageAcceptanceStoreError,
    MessageAcceptanceWrite, MessageAcceptanceWriteOutcome, Recipient,
};

/// Trusted lower fact boundary only; concrete store durability is tested elsewhere.
struct Commit;
impl MessageAcceptanceStore for Commit {
    /// Qualifies the exact issued original, without an I/O or kernel-fate claim.
    fn commit(
        &mut self,
        _: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        Ok(())
    }
}

/// Creates an original native request from real caller discovery identity.
fn source(service: &RuntimeSessionService, parent: &AgentTurnRecord, recipient: &str) -> Envelope {
    let sender = service
        .message_service()
        .registered_identity(&AgentId::opaque(parent.agent_id.clone()).unwrap())
        .unwrap()
        .clone();
    Envelope {
        protocol: "mmp/1",
        id: format!("{}:storage-question", parent.turn_id),
        message_type: "send".into(),
        time: "original native clock".into(),
        sender,
        recipient: Recipient::Agent(AgentId::opaque(recipient.to_string()).unwrap()),
        correlation_id: Some(parent.turn_id.clone()),
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "original native question".into(),
        extension_fields: vec![],
    }
}

/// Issues through the actual paired owner and returns a real private commit fact.
fn commit(
    service: &mut RuntimeSessionService,
    ticket: &RuntimePreparedTrackedMessage,
) -> MessageAcceptanceCommit {
    let mut committed = None;
    service
        .start_prepared_tracked_runtime_message(ticket, |messages, requests, lower| {
            assert!(requests.prepared_origin(lower).is_ok());
            let write = messages.begin_acceptance_write(lower)?;
            let MessageAcceptanceWriteOutcome::Committed(fact) = write.persist(&mut Commit) else {
                panic!("trusted port must qualify")
            };
            committed = Some(fact);
            Ok(())
        })
        .unwrap();
    committed.unwrap()
}

/// Reservation keeps native metadata invisible and charged. Restore cannot clear
/// it; duplicate adoption reuses the real receipt and cannot re-enter publication,
/// while consumed receipts cannot be recreated by an old original capability.
#[test]
fn native_storage_metadata_is_unpublished_until_paired_adoption_and_duplicate_is_inert() {
    let (mut service, parent, peer) = fixture();
    let envelope = source(&service, &parent, peer.agent_id.as_str());
    let ticket = service
        .prepare_tracked_runtime_message(
            &parent,
            envelope,
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    let usage = service.tracked_message_request_usage();
    assert_eq!(usage.0.0, 0);
    assert!(usage.0.1 > 0);
    assert_eq!(usage.1, 0);
    let before = service.message_service().snapshot_state().unwrap();
    assert!(
        service
            .retire_runtime_message_transfers_for_restore()
            .is_err()
    );
    assert_eq!(service.message_service().snapshot_state().unwrap(), before);
    let fact = commit(&mut service, &ticket);
    assert!(
        service
            .cancel_prepared_tracked_runtime_message(&ticket)
            .is_err()
    );
    let delivery = service
        .adopt_prepared_tracked_runtime_message(&ticket, |messages, requests| {
            Ok(requests
                .publish_acceptance(messages, &fact)?
                .delivery()
                .clone())
        })
        .unwrap();
    assert_eq!(service.tracked_message_request_usage().0.0, 1);
    let duplicate = service
        .adopt_prepared_tracked_runtime_message(&ticket, |_, _| {
            panic!("duplicate must not republish")
        })
        .unwrap();
    assert_eq!(duplicate, delivery);
    service
        .retire_runtime_message_transfers_for_restore()
        .unwrap();
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |_, _| panic!(
                "consumed owner must not replay"
            ))
            .is_err()
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
}

/// Actual lower publication followed by a lost callback result must retain native
/// slots. The next adoption recognizes the exact live ticket-bound receipt and
/// installs metadata without re-entering storage or publishing another send.
#[test]
fn native_storage_recovers_actual_paired_publication_before_lost_callback_result() {
    let (mut service, parent, peer) = fixture();
    let envelope = source(&service, &parent, peer.agent_id.as_str());
    let ticket = service
        .prepare_tracked_runtime_message(
            &parent,
            envelope,
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    let fact = commit(&mut service, &ticket);
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |messages, requests| {
                requests.publish_acceptance(messages, &fact)?;
                Err(MezError::invalid_state(
                    "injected loss after actual paired publication",
                ))
            })
            .is_err()
    );
    assert_eq!(service.tracked_message_request_usage().0.0, 1);
    assert!(
        service
            .retire_runtime_message_transfers_for_restore()
            .is_err()
    );
    let accepted = service.message_service().snapshot_state().unwrap();
    let delivery = service
        .adopt_prepared_tracked_runtime_message(&ticket, |_, _| {
            panic!("must reconcile actual published receipt")
        })
        .unwrap();
    assert!(delivery.accepted);
    assert_eq!(
        service.message_service().snapshot_state().unwrap(),
        accepted
    );
    assert_eq!(service.tracked_message_request_usage().0.0, 1);
    service
        .retire_runtime_message_transfers_for_restore()
        .unwrap();
}

/// An owned child's genuine terminal execution between actual lower publication
/// and native metadata installation remains attached to its captured task. This
/// isolates the upper publication gap; earlier I/O overlap also needs bridge
/// continuation integration. Child retirement before installation must preserve
/// the captured terminal rather than replacing it with later unavailability.
#[test]
fn native_storage_retains_original_child_terminal_before_native_metadata_publication() {
    let (mut service, parent, _) = fixture();
    service.configure_agent_scheduler_limit(2).unwrap();
    let child = spawn_child(&mut service, &parent);
    let envelope = source(&service, &parent, &child.agent_id);
    let ticket = service
        .prepare_tracked_runtime_message(
            &parent,
            envelope,
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    let fact = commit(&mut service, &ticket);
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |messages, requests| {
                requests.publish_acceptance(messages, &fact)?;
                Err(MezError::invalid_state(
                    "injected native metadata publication gap",
                ))
            })
            .is_err()
    );
    execute(
        &mut service,
        &child,
        AgentAction {
            id: "terminal-during-storage".into(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "captured terminal while storage pending".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        },
    );
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "retire-before-native-publication".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    let delivery = service
        .adopt_prepared_tracked_runtime_message(&ticket, |_, _| {
            panic!("original published receipt must settle native metadata without replay")
        })
        .unwrap();
    wait(&mut service, &parent, &delivery.message_id, None);
    let result = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("peer_terminal")
    );
    assert!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block
                .content
                .contains("captured terminal while storage pending"))
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
}

/// Foreign allocations cannot consume originals even with compatible live setup.
/// Never-issued cancellation releases both charges, and an old capability cannot
/// cancel a replacement reservation or invoke a foreign storage callback.
#[test]
fn native_storage_wrong_owner_and_stale_cancel_preserve_original_reservations() {
    let (mut service, parent, peer) = fixture();
    let (mut foreign, foreign_parent, foreign_peer) = fixture();
    let first = service
        .prepare_tracked_runtime_message(
            &parent,
            source(&service, &parent, peer.agent_id.as_str()),
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    let other = foreign
        .prepare_tracked_runtime_message(
            &foreign_parent,
            source(&foreign, &foreign_parent, foreign_peer.agent_id.as_str()),
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    let usage = service.tracked_message_request_usage();
    let foreign_usage = foreign.tracked_message_request_usage();
    assert!(
        foreign
            .start_prepared_tracked_runtime_message(&first, |_, _, _| panic!(
                "foreign callback must not run"
            ))
            .is_err()
    );
    assert!(
        service
            .cancel_prepared_tracked_runtime_message(&other)
            .is_err()
    );
    assert_eq!(service.tracked_message_request_usage(), usage);
    assert_eq!(foreign.tracked_message_request_usage(), foreign_usage);
    service
        .cancel_prepared_tracked_runtime_message(&first)
        .unwrap();
    let replacement = service
        .prepare_tracked_runtime_message(
            &parent,
            source(&service, &parent, peer.agent_id.as_str()),
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    assert!(
        service
            .cancel_prepared_tracked_runtime_message(&first)
            .is_err()
    );
    service
        .cancel_prepared_tracked_runtime_message(&replacement)
        .unwrap();
    foreign
        .cancel_prepared_tracked_runtime_message(&other)
        .unwrap();
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
}

/// Conversation replacement cannot discard an issued original or attach its
/// acceptance to a new conversation. The lower receipt keeps the original origin;
/// later ordinary retirement may consume it, after which no old callback replays.
#[test]
fn native_storage_keeps_original_caller_across_conversation_replacement() {
    let (mut service, parent, peer, primary) = fixture_with_primary();
    let envelope = source(&service, &parent, peer.agent_id.as_str());
    let ticket = service
        .prepare_tracked_runtime_message(
            &parent,
            envelope,
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    let fact = commit(&mut service, &ticket);
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "unrelated replacement conversation")
        .unwrap();
    let replacement = service.agent_turn_ledger().turns().last().unwrap().clone();
    assert_ne!(replacement.conversation_id, parent.conversation_id);
    let delivery = service
        .adopt_prepared_tracked_runtime_message(&ticket, |messages, requests| {
            let receipt = requests.publish_acceptance(messages, &fact)?;
            assert_eq!(receipt.origin().conversation_id, parent.conversation_id);
            assert_eq!(receipt.origin().turn_id, parent.turn_id);
            Ok(receipt.delivery().clone())
        })
        .unwrap();
    assert!(delivery.accepted);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(service.tracked_message_request_usage().0.0, 0);
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |_, _| panic!(
                "retired original cannot replay into replacement"
            ))
            .is_err()
    );
}

/// Snapshot replacement must also fence lower raw acceptance and ACK work that
/// has no upper native metadata record. Both never-issued controls release only
/// by their exact cancellation; refusal must leave current native identity intact.
#[test]
fn native_restore_guard_also_preserves_unpublished_raw_acceptance_and_ack() {
    let (mut service, parent, peer) = fixture();
    let envelope = source(&service, &parent, peer.agent_id.as_str());
    let sender = envelope.sender.agent_id.clone();
    let ticket = service
        .message_service_mut()
        .prepare_acceptance(
            &sender,
            envelope.clone(),
            MessageScope::Session,
            current_unix_millis(),
            4096,
            None,
        )
        .unwrap();
    let before = service.message_service().snapshot_state().unwrap();
    assert!(
        service
            .retire_runtime_message_transfers_for_restore()
            .is_err()
    );
    assert_eq!(service.message_service().snapshot_state().unwrap(), before);
    assert!(
        service
            .message_service()
            .registered_identity(&sender)
            .is_some()
    );
    service
        .message_service_mut()
        .cancel_acceptance(&ticket)
        .unwrap();
    if service
        .message_service()
        .subscription(&peer.agent_id)
        .is_none()
    {
        service
            .message_service_mut()
            .subscribe(&peer.agent_id)
            .unwrap();
    }
    let now = current_unix_millis();
    let delivery = service
        .message_service_mut()
        .accept_at_with_scope(&sender, envelope, MessageScope::Session, now)
        .unwrap();
    service
        .message_service()
        .receive_subscribed(&peer.agent_id, now, 10)
        .unwrap();
    let ack = service
        .message_service_mut()
        .prepare_acknowledgement(&peer.agent_id, delivery.sequence, 512 * 1024)
        .unwrap()
        .unwrap();
    let before = service.message_service().snapshot_state().unwrap();
    assert!(
        service
            .retire_runtime_message_transfers_for_restore()
            .is_err()
    );
    assert_eq!(service.message_service().snapshot_state().unwrap(), before);
    assert!(
        service
            .message_service()
            .registered_identity(&sender)
            .is_some()
    );
    service
        .message_service_mut()
        .cancel_acknowledgement(&ack)
        .unwrap();
    service
        .retire_runtime_message_transfers_for_restore()
        .unwrap();
}

/// Lost callback recovery must survive genuine caller retirement plus ordinary
/// pruning. Another old receipt in the same conversation must still retire;
/// protecting only one generation cannot let bulk retirement delete both.
#[test]
fn native_storage_lost_callback_survives_conversation_pruning_of_other_receipts() {
    let (mut service, parent, peer, primary) = fixture_with_primary();
    let older = send(&mut service, &parent, peer.agent_id.as_str(), true);
    let envelope = source(&service, &parent, peer.agent_id.as_str());
    let ticket = service
        .prepare_tracked_runtime_message(
            &parent,
            envelope,
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap();
    let fact = commit(&mut service, &ticket);
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |messages, requests| {
                requests.publish_acceptance(messages, &fact)?;
                Err(MezError::invalid_state(
                    "injected lost native callback before caller retirement",
                ))
            })
            .is_err()
    );
    assert_eq!(service.tracked_message_request_usage().0.0, 2);
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert_ne!(
        service
            .agent_shell_store()
            .get(&parent.pane_id)
            .unwrap()
            .session_id,
        parent.conversation_id
    );
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert!(
        service
            .retire_runtime_message_transfers_for_restore()
            .is_err()
    );
    assert_eq!(
        service.tracked_message_request_usage().0.0,
        1,
        "only exact pending native receipt must remain"
    );
    let accepted = service.message_service().snapshot_state().unwrap();
    let delivery = service
        .adopt_prepared_tracked_runtime_message(&ticket, |_, _| {
            panic!("retired caller must reconcile original receipt without republishing")
        })
        .unwrap();
    assert_ne!(delivery.message_id, older);
    assert_eq!(
        service.message_service().snapshot_state().unwrap(),
        accepted
    );
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |_, _| panic!(
                "consumed original cannot replay"
            ))
            .is_err()
    );
}

/// Finite expiry may retire a request only after pending native installation has
/// settled. A sweep after actual lower publication and callback loss must retain
/// its original receipt/charge, without renewing the original TTL or resending.
#[test]
fn native_storage_lost_callback_survives_finite_expiry_until_metadata_installs() {
    let (mut service, parent, peer) = fixture();
    let now = current_unix_millis();
    if service
        .message_service()
        .subscription(&peer.agent_id)
        .is_none()
    {
        service
            .message_service_mut()
            .subscribe(&peer.agent_id)
            .unwrap();
    }
    let mut envelope = source(&service, &parent, peer.agent_id.as_str());
    envelope.ttl_ms = Some(10_000);
    let ticket = service
        .prepare_tracked_runtime_message(&parent, envelope, MessageScope::Session, now)
        .unwrap();
    let fact = commit(&mut service, &ticket);
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |messages, requests| {
                requests.publish_acceptance(messages, &fact)?;
                Err(MezError::invalid_state(
                    "injected lost native callback before expiry sweep",
                ))
            })
            .is_err()
    );
    let offered = service
        .message_service()
        .receive_subscribed(&peer.agent_id, now, 10)
        .unwrap();
    let sequence = offered.messages.last().unwrap().sequence;
    service
        .message_service_mut()
        .advance_subscription(&peer.agent_id, sequence)
        .unwrap();
    let usage = service.tracked_message_request_usage();
    service
        .deliver_pending_runtime_agent_messages(now + 20_000)
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(now + 620_001)
        .unwrap();
    assert_eq!(
        service.tracked_message_request_usage(),
        usage,
        "pending actual receipt must remain charged through expiry"
    );
    assert!(
        service
            .retire_runtime_message_transfers_for_restore()
            .is_err()
    );
    let accepted = service.message_service().snapshot_state().unwrap();
    assert!(
        service
            .adopt_prepared_tracked_runtime_message(&ticket, |_, _| panic!(
                "expiry cannot require another send/publication"
            ))
            .unwrap()
            .accepted
    );
    assert_eq!(
        service.message_service().snapshot_state().unwrap(),
        accepted
    );
    service
        .deliver_pending_runtime_agent_messages(now + 620_001)
        .unwrap();
    assert_eq!(
        service.tracked_message_request_usage(),
        ((0, 0, 0), 0),
        "original expired tracking must retire after installation"
    );
}
