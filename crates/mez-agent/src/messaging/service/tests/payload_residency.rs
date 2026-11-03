//! Missing payloads must preserve metadata and stop incomplete projections.
//!
//! Cache release is test-only until durable reload/recovery owns an exact source.
//! These tests exercise the same canonical rows used by receipts, replay and
//! pending custody, including live delivery pins and independent recipients.

use super::*;

/// Builds fixture input without manufacturing delivery or storage evidence.
fn mail(sender: &SenderIdentity, target: &AgentId, id: &str, ttl_ms: Option<u64>) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:residency".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.clone()),
        correlation_id: Some("original-request".into()),
        ttl_ms,
        content_type: "text/plain; charset=utf-8".into(),
        payload: format!("original UTF-8 payload: λ {id}"),
        extension_fields: vec![("x-source".into(), "true".into())],
    }
}

/// A delivery Arc continues to pin the original bytes after the cache releases
/// its reference. Reacquisition must return that exact allocation and retain its
/// byte charge; once all external pins drop, receipt aliases cannot keep it hot.
#[test]
fn released_cache_reuses_external_pin_and_preserves_its_charge() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let original = mail(&sender, &target.agent_id, "pinned", None);
    let bytes = original.payload.len();
    let receipt = service
        .accept_at_with_scope(&sender.agent_id, original, MessageScope::Session, 10)
        .unwrap();
    let batch = service.receive_subscribed(&target.agent_id, 11, 1).unwrap();
    let source = service.pending.get(&receipt.sequence).unwrap();
    source.envelope.release_cache_for_test();
    assert_eq!(source.envelope.resident_bytes(), bytes);
    let acquired = source.envelope.resident().unwrap();
    assert!(Arc::ptr_eq(&acquired, &batch.messages[0].envelope));
    drop(acquired);
    // Snapshot projection borrows the genuine external pin; it does not reload.
    assert_eq!(service.snapshot_state().unwrap().retained_messages.len(), 1);
    drop(batch);
    assert_eq!(source.envelope.resident_bytes(), 0);
    assert_eq!(source.envelope.payload_bytes, bytes);
    assert_eq!(source.envelope.id, "pinned");
    assert_eq!(
        source.envelope.correlation_id.as_deref(),
        Some("original-request")
    );
    assert!(Arc::ptr_eq(
        source,
        &service.accepted_messages["pinned"].source
    ));
    assert_eq!(
        source.envelope.resident().unwrap_err().kind(),
        MessageErrorKind::InvalidState
    );
    assert_eq!(
        service.snapshot_state().unwrap_err().kind(),
        MessageErrorKind::InvalidState
    );
    assert_eq!(
        service
            .offered_uncommitted_deliveries(&target.agent_id)
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidState
    );
}

/// An unavailable unread prefix cannot be bypassed by a later resident message.
/// No offer or cursor is created by failed receive; the fair fanout cycle still
/// serves another recipient whose prefix is resident. Snapshot capture rejects
/// the incomplete state rather than dropping the cold occurrence or its receipt.
#[test]
fn absent_prefix_stops_its_recipient_without_stalling_unrelated_fanout() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    let healthy = service.register_agent(None, None, "healthy", Vec::new());
    for recipient in [&target.agent_id, &healthy.agent_id] {
        service.subscribe(recipient).unwrap();
    }
    for (recipient, id) in [
        (&target.agent_id, "cold"),
        (&target.agent_id, "later"),
        (&healthy.agent_id, "healthy"),
    ] {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&sender, recipient, id, None),
                MessageScope::Session,
                10,
            )
            .unwrap();
    }
    let first = service.pending.get(&1).unwrap().clone();
    first.envelope.release_cache_for_test();
    let cursor = service.subscription(&target.agent_id).unwrap().clone();
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 11, 10)
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidState
    );
    assert_eq!(
        super::super::offers::offered_at(&first, &target.agent_id).unwrap(),
        None
    );
    assert_eq!(
        super::super::offers::offered_at(&service.pending[&2], &target.agent_id).unwrap(),
        None
    );
    let batches = service.fanout_ready(11, 10);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].recipient, healthy.agent_id);
    assert_eq!(batches[0].batch.messages[0].envelope.id, "healthy");
    assert_eq!(service.subscription(&target.agent_id), Some(&cursor));
    assert_eq!(service.pending.len(), 3);
    assert_eq!(
        service.snapshot_state().unwrap_err().kind(),
        MessageErrorKind::InvalidState
    );
    assert_eq!(
        service
            .historical_receive_through_subscribed(&target.agent_id, 2)
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidState
    );
}

/// TTL and ACK-prefix policy depend on immutable metadata, even when the expired
/// payload is absent. A fresh later offer may prove the earlier occurrence was
/// expired, without fabricating an earlier offer or treating expiry as delivery.
#[test]
fn absent_expired_prefix_uses_original_ttl_and_offer_rules() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    for (id, ttl) in [("expires", Some(5)), ("later", None)] {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&sender, &target.agent_id, id, ttl),
                MessageScope::Session,
                10,
            )
            .unwrap();
    }
    let first = service.pending[&1].clone();
    first.envelope.release_cache_for_test();
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 15, 10)
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidState
    );
    let later = service
        .receive_subscribed(&target.agent_id, 16, 10)
        .unwrap();
    assert_eq!(later.messages.len(), 1);
    assert_eq!(later.messages[0].sequence, 2);
    service.advance_subscription(&target.agent_id, 2).unwrap();
    assert!(!service.recipient_has_committed(&first, &target.agent_id));
    assert_eq!(
        super::super::offers::offered_at(&first, &target.agent_id).unwrap(),
        None
    );
}

/// A duplicate ID requires the original bytes for exact comparison. Absent
/// bytes cannot authorize success or a guessed content-conflict result; neither
/// failure reserves a new occurrence or overwrites the retained receipt.
#[test]
fn absent_payload_cannot_qualify_a_duplicate_acceptance() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    let original = mail(&sender, &target.agent_id, "duplicate", None);
    let receipt = service
        .accept_at_with_scope(
            &sender.agent_id,
            original.clone(),
            MessageScope::Session,
            10,
        )
        .unwrap();
    service.pending[&receipt.sequence]
        .envelope
        .release_cache_for_test();
    for payload in [
        &original,
        &Envelope {
            payload: "different".into(),
            ..original.clone()
        },
    ] {
        assert_eq!(
            service
                .accept_at_with_scope(&sender.agent_id, payload.clone(), MessageScope::Session, 11)
                .unwrap_err()
                .kind(),
            MessageErrorKind::InvalidState
        );
    }
    assert_eq!(service.last_sequence(), receipt.sequence);
    assert_eq!(service.accepted_messages["duplicate"].delivery, receipt);
}

/// Replay eviction leaves the canonical pending row and receipt intact. If its
/// payload is absent, capture must fail even though every row in the disposable
/// replay window is resident. ACK hashing likewise cannot substitute a digest
/// of metadata or an empty payload for the immutable original source.
#[test]
fn custody_only_absent_source_rejects_snapshot_and_ack_hashing() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    for id in ["custody-only", "replay"] {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&sender, &target.agent_id, id, None),
                MessageScope::Session,
                10,
            )
            .unwrap();
    }
    let first = service.pending[&1].clone();
    assert_eq!(service.queue.len(), 1);
    assert_eq!(service.queue[0].sequence, 2);
    assert!(super::super::admission::acknowledgement_source_digest(&first).is_ok());
    first.envelope.release_cache_for_test();
    assert_eq!(
        service.snapshot_state().unwrap_err().kind(),
        MessageErrorKind::InvalidState
    );
    assert!(super::super::admission::acknowledgement_source_digest(&first).is_err());
    assert_eq!(service.pending.len(), 2);
    assert_eq!(service.accepted_messages.len(), 2);
}

/// A bounded recovery projection needs only sources through its requested
/// occurrence. An unavailable later source cannot prevent reconstruction of an
/// earlier complete prefix or cause work beyond the requested boundary.
#[test]
fn recovery_prefix_does_not_acquire_a_later_absent_payload() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    for id in ["earlier", "later-absent"] {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&sender, &target.agent_id, id, None),
                MessageScope::Session,
                10,
            )
            .unwrap();
    }
    service.pending[&2].envelope.release_cache_for_test();
    let earlier = service
        .receive_through_subscribed(&target.agent_id, 1, 11)
        .unwrap();
    assert_eq!(earlier.len(), 1);
    assert_eq!(earlier[0].envelope.id, "earlier");
    assert_eq!(
        service
            .receive_through_subscribed(&target.agent_id, 2, 11)
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidState
    );
}
