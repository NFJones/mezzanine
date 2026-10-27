//! Terminal custody through real acceptance, offers, ACKs and restoration.
//!
//! Tiny budgets prove outcomes remain reserved until exact-owner consumption
//! or finite expiration. No fixture treats lifecycle loss as delivery success.

use super::*;
use crate::messaging::DeliveryTerminalReason;

/// Builds a two-recipient occurrence with independently bounded custody.
fn fixture(
    ttl_ms: Option<u64>,
) -> (
    MessageService,
    SenderIdentity,
    Vec<SenderIdentity>,
    Envelope,
) {
    let mut service = MessageService::with_pending_limits(
        1,
        128,
        PendingDeliveryLimits {
            messages: 1,
            ..PendingDeliveryLimits::default()
        },
    )
    .unwrap();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let recipients = (0..2)
        .map(|_| service.register_agent(None, None, "target", vec![]))
        .collect::<Vec<_>>();
    for recipient in &recipients {
        service.subscribe(&recipient.agent_id).unwrap();
    }
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: "original".into(),
        message_type: "send".into(),
        time: "fixture".into(),
        sender: sender.clone(),
        recipient: Recipient::Role("target".into()),
        correlation_id: None,
        ttl_ms,
        content_type: "text/plain".into(),
        payload: "private original payload".into(),
        extension_fields: vec![],
    };
    service
        .accept_at_with_scope(
            &sender.agent_id,
            envelope.clone(),
            MessageScope::Session,
            10,
        )
        .unwrap();
    (service, sender, recipients, envelope)
}

/// Partial fanout ACK and exact retirement generate one typed disposition,
/// never two or a fabricated commit. Both live and restored owners conserve
/// the full reservation until that exact sender consumes it, then admit one.
#[test]
fn terminal_retirement_is_exact_reserved_and_consumable_after_restore() {
    let (mut source, sender, recipients, original) = fixture(None);
    let offered = source
        .receive_subscribed(&recipients[0].agent_id, 11, 10)
        .unwrap();
    source
        .advance_subscription(&recipients[0].agent_id, offered.messages[0].sequence)
        .unwrap();
    source.retire_agent_identity(&recipients[1].agent_id);
    for restore in [false, true] {
        let mut service = if restore {
            MessageService::from_snapshot_state_with_pending_limits(
                &source.snapshot_state(),
                source.pending_limits,
            )
            .unwrap()
        } else {
            fixture(None).0
        };
        if !restore {
            service
                .receive_subscribed(&recipients[0].agent_id, 11, 10)
                .unwrap();
            service
                .advance_subscription(&recipients[0].agent_id, 1)
                .unwrap();
            service.retire_agent_identity(&recipients[1].agent_id);
        }
        assert_eq!(service.settle_delivery_outcomes_at(12).unwrap(), 1);
        assert_eq!(service.settle_delivery_outcomes_at(13).unwrap(), 0);
        let outcomes = service.delivery_outcomes_for(&sender.agent_id, 10).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].reason, DeliveryTerminalReason::RecipientRetired);
        assert_eq!(
            outcomes[0].recipient.recipient,
            recipients[1].agent_id.as_str()
        );
        assert_eq!(outcomes[0].observed_at_ms, 12);
        assert!(
            !serde_json::to_string(&outcomes[0])
                .unwrap()
                .contains(&original.payload)
        );
        assert!(
            service
                .delivery_outcomes_for(&recipients[0].agent_id, 10)
                .unwrap()
                .is_empty()
        );
        let before = service.snapshot_state();
        let mut next = original.clone();
        next.id = "next".into();
        assert_eq!(
            service
                .accept_at_with_scope(&sender.agent_id, next.clone(), MessageScope::Session, 13)
                .unwrap_err()
                .kind(),
            MessageErrorKind::Capacity
        );
        assert_eq!(service.snapshot_state(), before);
        assert_eq!(
            MessageService::from_snapshot_state_with_pending_limits(
                &before,
                service.pending_limits
            )
            .unwrap()
            .snapshot_state(),
            before
        );
        assert!(
            service
                .acknowledge_delivery_outcome(&recipients[0].agent_id, &outcomes[0])
                .is_err()
        );
        assert_eq!(service.snapshot_state(), before);
        assert!(
            service
                .acknowledge_delivery_outcome(&sender.agent_id, &outcomes[0])
                .unwrap()
        );
        assert!(
            !service
                .acknowledge_delivery_outcome(&sender.agent_id, &outcomes[0])
                .unwrap()
        );
        assert!(
            service
                .delivery_outcomes_for(&sender.agent_id, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            service
                .accept_at_with_scope(&sender.agent_id, next, MessageScope::Session, 13)
                .unwrap()
                .sequence,
            2
        );
    }
}

/// Expiry uses the established strict TTL boundary, pre-reserved slots and a
/// ten-minute first-observation horizon. Presence changes are not retirement;
/// rollback cannot revive mail or renew the horizon. Fully offered but not
/// acknowledged mail becomes an outcome, and subsequent ACK cannot erase it.
#[test]
fn terminal_expiry_is_bounded_without_fabricating_ack() {
    let (mut service, sender, recipients, _) = fixture(Some(5));
    service
        .update_presence(&recipients[0].agent_id, AgentPresenceStatus::Offline, 11)
        .unwrap();
    assert_eq!(service.settle_delivery_outcomes_at(15).unwrap(), 0);
    service
        .update_presence(&recipients[0].agent_id, AgentPresenceStatus::Available, 15)
        .unwrap();
    service
        .receive_subscribed(&recipients[0].agent_id, 15, 10)
        .unwrap();
    assert_eq!(service.settle_delivery_outcomes_at(16).unwrap(), 2);
    assert!(
        service
            .advance_subscription(&recipients[0].agent_id, 1)
            .is_err()
    );
    assert!(
        service
            .receive_subscribed(&recipients[0].agent_id, 14, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(service.settle_delivery_outcomes_at(14).unwrap(), 0);
    let outcomes = service.delivery_outcomes_for(&sender.agent_id, 1).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].reason, DeliveryTerminalReason::Expired);
    assert!(
        service.snapshot_state().retained_messages[0]
            .acknowledged_recipients
            .as_ref()
            .unwrap()
            .is_empty()
    );
    service.settle_delivery_outcomes_at(600_015).unwrap();
    assert_eq!(service.pending.len(), 1);
    assert_eq!(
        service
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .len(),
        2
    );
    service.settle_delivery_outcomes_at(600_016).unwrap();
    assert!(service.pending.is_empty());
    assert!(
        service
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        MessageService::from_snapshot_state(&service.snapshot_state())
            .unwrap()
            .snapshot_state(),
        service.snapshot_state()
    );
}

/// Reusing either opaque ID cannot acquire a former interval's mailbox or
/// terminal receipts. Invalid/forged consumption is fully non-mutating.
#[test]
fn terminal_outcomes_reject_reused_sender_and_recipient() {
    let (mut service, sender, recipients, _) = fixture(None);
    service.retire_agent_identity(&recipients[0].agent_id);
    service
        .ensure_agent_identity(recipients[0].clone(), 11)
        .unwrap();
    service
        .subscribe_from_retained_start(&recipients[0].agent_id)
        .unwrap();
    service.settle_delivery_outcomes_at(12).unwrap();
    let outcome = service
        .delivery_outcomes_for(&sender.agent_id, 10)
        .unwrap()
        .remove(0);
    assert!(
        service
            .receive_subscribed(&recipients[0].agent_id, 12, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let mut forged = outcome.clone();
    forged.observed_at_ms += 1;
    let before = service.snapshot_state();
    assert!(
        service
            .acknowledge_delivery_outcome(&sender.agent_id, &forged)
            .is_err()
    );
    assert_eq!(service.snapshot_state(), before);
    service.retire_agent_identity(&sender.agent_id);
    service.ensure_agent_identity(sender.clone(), 13).unwrap();
    service
        .subscribe_from_retained_start(&sender.agent_id)
        .unwrap();
    assert!(
        service
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    let before = service.snapshot_state();
    assert!(
        service
            .acknowledge_delivery_outcome(&sender.agent_id, &outcome)
            .is_err()
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(
        MessageService::from_snapshot_state(&before)
            .unwrap()
            .snapshot_state(),
        before
    );
}

/// Every restored terminal slot must have a genuine binding, exclusive ACK
/// state and reason-consistent TTL/retirement evidence. Sender intervals cannot
/// be replaced by the current floor of a later registration. A version downgrade
/// cannot hide newer authority, and no rejection mutates the live source owner.
#[test]
fn terminal_snapshot_rejects_contradictions_and_preserves_legacy_unknown_owner() {
    let (mut service, sender, recipients, _) = fixture(Some(5));
    service
        .receive_subscribed(&recipients[0].agent_id, 11, 10)
        .unwrap();
    service.settle_delivery_outcomes_at(16).unwrap();
    let before = service.snapshot_state();
    let mut corruptions = Vec::new();
    let mut bad = before.clone();
    bad.retained_messages[0].terminal_recipients = None;
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0]
        .terminal_recipients
        .as_mut()
        .unwrap()
        .pop();
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0]
        .terminal_recipients
        .as_mut()
        .unwrap()[0]
        .as_mut()
        .unwrap()
        .observed_at_ms = 15;
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0]
        .terminal_recipients
        .as_mut()
        .unwrap()[0]
        .as_mut()
        .unwrap()
        .reason = DeliveryTerminalReason::RecipientRetired;
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0]
        .acknowledged_recipients
        .as_mut()
        .unwrap()
        .push(recipients[0].agent_id.to_string());
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0].sender_registered_after_sequence = Some(1);
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.schema_version = 7;
    corruptions.push(bad);
    for bad in corruptions {
        let decoded: MessageServiceSnapshot =
            serde_json::from_slice(&serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(MessageService::from_snapshot_state(&decoded).is_err());
        assert_eq!(service.snapshot_state(), before);
    }
    let (mut legacy_source, _, _, _) = fixture(None);
    legacy_source.retire_agent_identity(&sender.agent_id);
    let mut legacy = legacy_source.snapshot_state();
    legacy.schema_version = 7;
    for row in &mut legacy.retained_messages {
        row.terminal_recipients = None;
        row.sender_registered_after_sequence = None;
    }
    let mut upgraded = MessageService::from_snapshot_state(&legacy).unwrap();
    upgraded.ensure_agent_identity(sender.clone(), 11).unwrap();
    upgraded
        .subscribe_from_retained_start(&sender.agent_id)
        .unwrap();
    upgraded.retire_agent_identity(&recipients[0].agent_id);
    upgraded.settle_delivery_outcomes_at(12).unwrap();
    assert!(
        upgraded
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        upgraded.snapshot_state().retained_messages[0].sender_registered_after_sequence,
        None
    );
    assert_eq!(
        MessageService::from_snapshot_state(&upgraded.snapshot_state())
            .unwrap()
            .snapshot_state(),
        upgraded.snapshot_state()
    );
}

/// A terminal prefix may be crossed by a later genuinely offered ACK even if
/// the clock rolled back or the endpoint was offered earlier. The original
/// outcome remains reserved/visible and no ACK is invented for that prefix.
#[test]
fn terminal_prefix_does_not_block_later_real_commit() {
    let mut service = MessageService::with_pending_limits(
        1,
        128,
        PendingDeliveryLimits {
            messages: 2,
            ..PendingDeliveryLimits::default()
        },
    )
    .unwrap();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let mut message = fixture(Some(5)).3;
    message.sender = sender.clone();
    service
        .accept_at_with_scope(&sender.agent_id, message.clone(), MessageScope::Session, 10)
        .unwrap();
    message.id = "later".into();
    message.ttl_ms = None;
    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 11)
        .unwrap();
    service
        .receive_subscribed(&target.agent_id, 15, 10)
        .unwrap();
    assert_eq!(service.settle_delivery_outcomes_at(16).unwrap(), 1);
    service.advance_subscription(&target.agent_id, 2).unwrap();
    let snapshot = service.snapshot_state();
    assert_eq!(snapshot.custody_messages.as_ref().unwrap().len(), 1);
    assert!(
        snapshot.custody_messages.as_ref().unwrap()[0]
            .acknowledged_recipients
            .as_ref()
            .unwrap()
            .is_empty()
    );
    let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    assert_eq!(restored.snapshot_state(), snapshot);
    let outcome = restored
        .delivery_outcomes_for(&sender.agent_id, 10)
        .unwrap()
        .remove(0);
    restored
        .acknowledge_delivery_outcome(&sender.agent_id, &outcome)
        .unwrap();
    assert!(restored.pending.is_empty());
    assert!(
        restored
            .snapshot_state()
            .custody_messages
            .as_ref()
            .unwrap()
            .is_empty()
    );
}
