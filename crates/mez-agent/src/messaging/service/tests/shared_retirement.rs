//! Reusable recipient retirement must not delete another recipient's mail.
//!
//! All selectors share the same canonical envelope, indexes and acceptance
//! receipt. Ending one registration removes its authority, not shared source.
//! These controls deliberately do not promise retention through replay pressure.

use super::*;

/// Unread fanout survives retirement of any one matching registration across
/// session, role, capability, group, pane and window selectors. Checking the
/// exact queue/receipt and Arc identity catches deletion or synthetic redelivery.
#[test]
fn generic_retirement_preserves_unread_shared_occurrences() {
    assert_shared_retirement(false);
}

/// A fast recipient's genuine offered ACK followed by retirement cannot remove
/// a slow recipient's unread occurrence. Snapshot reconstruction must preserve
/// both immutable acceptance and independent cursor/offer evidence.
#[test]
fn generic_retirement_preserves_partially_acknowledged_fanout() {
    assert_shared_retirement(true);
}

/// Non-destructive retirement does not enlarge count/byte replay budgets.
/// Unacknowledged direct mail stays inaccessible after owner loss and retains
/// custody/receipt/floor pending an explicit terminal delivery disposition.
#[test]
fn generic_retirement_keeps_existing_replay_bounds() {
    let mut service = MessageService::with_limits(2, 32);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    let mail = |id: &str, target: &AgentId| Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:bounds".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "1234567890123456".into(),
        extension_fields: Vec::new(),
    };
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail("retired-direct", &target.agent_id),
            MessageScope::Session,
            1,
        )
        .unwrap();
    assert!(service.retire_agent_identity(&target.agent_id));
    assert_eq!(service.queue.len(), 1);
    assert!(service.receive_for(&target.agent_id, 2).is_empty());
    for index in 0..3 {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&format!("pressure-{index}"), &sender.agent_id),
                MessageScope::Session,
                2 + index,
            )
            .unwrap();
        assert!(service.queue.len() <= 2);
        assert!(service.queued_bytes <= 32);
        MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    }
    assert_eq!(service.queue.len(), 2);
    assert_eq!(service.queued_bytes, 32);
    assert!(service.accepted_messages.contains_key("retired-direct"));
    assert!(
        service
            .retired_delivery_floors
            .contains_key(&target.agent_id)
    );
}

/// Exercises the common retirement transition under each multi-target selector,
/// including exact ID reuse, a late matching registration and idempotent send.
fn assert_shared_retirement(ack_first: bool) {
    for selector in [
        Recipient::Session,
        Recipient::Role("worker".into()),
        Recipient::Capability("shared".into()),
        Recipient::Group("shared".into()),
        Recipient::Pane(PaneId::new('%', 1)),
        Recipient::Window(WindowId::new('@', 1)),
    ] {
        let mut service = MessageService::with_limits(2, 1024);
        let sender = service.register_agent(None, None, "sender", Vec::new());
        let mut worker = || {
            service.register_agent(
                Some(PaneId::new('%', 1)),
                Some(WindowId::new('@', 1)),
                "worker",
                vec!["shared".into()],
            )
        };
        let first = worker();
        let second = worker();
        for target in [&first, &second] {
            service.subscribe(&target.agent_id).unwrap();
        }
        let envelope = Envelope {
            protocol: MMP_PROTOCOL,
            id: "shared-original".into(),
            message_type: "send".into(),
            time: "fixture:retirement".into(),
            sender: sender.clone(),
            recipient: selector,
            correlation_id: Some("original-request".into()),
            ttl_ms: Some(100),
            content_type: "text/plain; charset=utf-8".into(),
            payload: "retained original".into(),
            extension_fields: Vec::new(),
        };
        let accepted = service
            .accept_at_with_scope(
                &sender.agent_id,
                envelope.clone(),
                MessageScope::Session,
                10,
            )
            .unwrap();
        let original = service.queue[0].envelope.clone();
        if ack_first {
            let offered = service
                .fanout_ready_for(&first.agent_id, 11, 1)
                .unwrap()
                .unwrap();
            service.acknowledge_fanout_batch(&offered).unwrap();
        }
        let before = service.snapshot_state();
        let slow_cursor = service.subscription(&second.agent_id).unwrap().clone();
        assert!(service.retire_agent_identity(&first.agent_id));
        let retired = service.snapshot_state();
        assert_eq!(retired.retained_messages, before.retained_messages);
        assert_eq!(retired.accepted_messages, before.accepted_messages);
        assert_eq!(service.subscription(&second.agent_id), Some(&slow_cursor));
        assert!(service.registered_identity(&first.agent_id).is_none());
        assert!(service.subscription(&first.agent_id).is_none());
        assert!(!service.retire_agent_identity(&first.agent_id));
        assert_eq!(service.snapshot_state(), retired);
        let slow = service
            .fanout_ready_for(&second.agent_id, 12, 1)
            .unwrap()
            .unwrap();
        assert_eq!(slow.batch.messages[0].sequence, accepted.sequence);
        assert!(Arc::ptr_eq(&slow.batch.messages[0].envelope, &original));
        assert_eq!(service.queued_bytes, original.payload.len());
        let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
        restored.ensure_agent_identity(first.clone(), 13).unwrap();
        restored
            .subscribe_from_retained_start(&first.agent_id)
            .unwrap();
        let late = restored.register_agent(
            Some(PaneId::new('%', 1)),
            Some(WindowId::new('@', 1)),
            "worker",
            vec!["shared".into()],
        );
        restored
            .subscribe_from_retained_start(&late.agent_id)
            .unwrap();
        for target in [&first, &late] {
            assert!(
                restored
                    .receive_subscribed(&target.agent_id, 14, 10)
                    .unwrap()
                    .messages
                    .is_empty()
            );
            assert!(
                restored
                    .historical_receive_through_subscribed(&target.agent_id, accepted.sequence)
                    .unwrap()
                    .is_empty()
            );
        }
        let before_retry = restored.snapshot_state();
        assert_eq!(
            restored
                .accept_at_with_scope(
                    &sender.agent_id,
                    envelope.clone(),
                    MessageScope::Session,
                    14
                )
                .unwrap(),
            accepted
        );
        assert_eq!(restored.snapshot_state(), before_retry);
        let mut conflicting = envelope;
        conflicting.payload.push_str(" changed");
        assert_eq!(
            restored
                .accept_at_with_scope(&sender.agent_id, conflicting, MessageScope::Session, 14)
                .unwrap_err()
                .kind(),
            MessageErrorKind::Conflict
        );
        assert_eq!(restored.snapshot_state(), before_retry);
        let ready = restored.fanout_ready(15, 10);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].recipient, second.agent_id);
        assert_eq!(
            ready[0].batch.messages[0].envelope.as_ref(),
            original.as_ref()
        );
        restored.acknowledge_fanout_batch(&ready[0]).unwrap();
        assert!(restored.fanout_ready(16, 10).is_empty());
        assert_eq!(restored.snapshot_state().retained_messages.len(), 1);
        MessageService::from_snapshot_state(&restored.snapshot_state()).unwrap();
    }
}
