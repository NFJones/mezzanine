//! Same-runtime acceptance evidence independent of replay and ACK lifetimes.
//!
//! Proofs establish authenticated occurrences, not answer quality, task success,
//! delivery commit, or authority to resume a product conversation.

use super::*;

/// Builds a direct session-scoped request or correlated reply without relying
/// on model-supplied registration floors or caller-supplied proof fields.
fn mail(
    sender: &SenderIdentity,
    recipient: &AgentId,
    id: &str,
    correlation: Option<&str>,
) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:proof".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(recipient.clone()),
        correlation_id: correlation.map(str::to_owned),
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "untrusted peer content".into(),
        extension_fields: Vec::new(),
    }
}

/// ACK and replay eviction must not erase already captured request evidence.
/// A genuinely authenticated reverse occurrence with the exact correlation
/// matches, while an unrelated correlation or another sender cannot match.
#[test]
fn acceptance_proof_survives_ack_and_replay_eviction() {
    let mut service = MessageService::with_limits(1, 4096);
    let parent = service.register_agent(None, None, "parent", vec![]);
    let child = service.register_agent(None, None, "child", vec![]);
    let stranger = service.register_agent(None, None, "stranger", vec![]);
    service.subscribe(&child.agent_id).unwrap();
    let request = service
        .accept_at_with_scope(
            &parent.agent_id,
            mail(&parent, &child.agent_id, "request", None),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let proof = service
        .acceptance_proof(&parent.agent_id, request.sequence)
        .unwrap();
    assert_eq!(proof.message_id(), "request");
    assert_eq!(
        proof.recipients().collect::<Vec<_>>(),
        vec![&child.agent_id]
    );
    assert!(proof.metadata_bytes() >= "request".len() + child.agent_id.to_string().len());
    service.receive_subscribed(&child.agent_id, 2, 1).unwrap();
    service
        .advance_subscription(&child.agent_id, request.sequence)
        .unwrap();
    let reply = service
        .accept_at_with_scope(
            &child.agent_id,
            mail(&child, &parent.agent_id, "reply", Some("request")),
            MessageScope::Session,
            3,
        )
        .unwrap();
    let reply_proof = service
        .acceptance_proof(&child.agent_id, reply.sequence)
        .unwrap();
    assert!(
        service
            .acceptance_proof(&parent.agent_id, request.sequence)
            .is_none()
    );
    assert!(service.acceptance_reply_matches(&proof, &child.agent_id, &reply_proof));
    for (sender, correlation) in [(&child, "different"), (&stranger, "request")] {
        let delivery = service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(sender, &parent.agent_id, correlation, Some(correlation)),
                MessageScope::Session,
                4,
            )
            .unwrap();
        let other = service
            .acceptance_proof(&sender.agent_id, delivery.sequence)
            .unwrap();
        assert!(!service.acceptance_reply_matches(&proof, &child.agent_id, &other));
    }
}

/// Offline presence leaves the registration intact, whereas retirement and
/// same-ID reuse end it. Snapshot restoration cannot replay an old runtime's
/// proof or manufacture fresh acceptance evidence from serialized history.
#[test]
fn acceptance_proof_rejects_reuse_and_snapshot_reconstruction() {
    let mut service = MessageService::default();
    let parent = service.register_agent(None, None, "parent", vec![]);
    let child = service.register_agent(None, None, "child", vec![]);
    let sent = service
        .accept_at_with_scope(
            &parent.agent_id,
            mail(&parent, &child.agent_id, "request", None),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let proof = service
        .acceptance_proof(&parent.agent_id, sent.sequence)
        .unwrap();
    assert!(service.acceptance_recipient_is_current(&proof, &child.agent_id));
    service
        .update_presence(&child.agent_id, AgentPresenceStatus::Offline, 2)
        .unwrap();
    assert!(service.acceptance_recipient_is_current(&proof, &child.agent_id));
    let restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    assert!(!restored.acceptance_recipient_is_current(&proof, &child.agent_id));
    assert!(
        restored
            .acceptance_proof(&parent.agent_id, sent.sequence)
            .is_none()
    );
    assert!(service.retire_observational_identity(&child.agent_id));
    service.ensure_agent_identity(child.clone(), 3).unwrap();
    assert!(!service.acceptance_recipient_is_current(&proof, &child.agent_id));
    let reply = service
        .accept_at_with_scope(
            &child.agent_id,
            mail(
                &child,
                &parent.agent_id,
                "replacement-reply",
                Some("request"),
            ),
            MessageScope::Session,
            4,
        )
        .unwrap();
    let replacement = service
        .acceptance_proof(&child.agent_id, reply.sequence)
        .unwrap();
    assert!(!service.acceptance_reply_matches(&proof, &child.agent_id, &replacement));
    assert!(
        service
            .acceptance_proof(&child.agent_id, sent.sequence)
            .is_none()
    );
}

/// Frozen fanout membership authenticates only the selected original worker.
/// A later worker cannot acquire the request through role discovery, and a
/// genuine reply captured before retirement remains occurrence evidence after
/// both owners retire. Queries do not offer mail, advance cursors, or mutate
/// the service's serializable state.
#[test]
fn acceptance_proof_fanout_and_pre_retirement_reply_are_exact() {
    let mut service = MessageService::default();
    let parent = service.register_agent(None, None, "parent", vec![]);
    let first = service.register_agent(None, None, "worker", vec![]);
    let second = service.register_agent(None, None, "worker", vec![]);
    let mut envelope = mail(&parent, &first.agent_id, "fanout-request", None);
    envelope.recipient = Recipient::Role("worker".into());
    let sent = service
        .accept_at_with_scope(&parent.agent_id, envelope, MessageScope::Session, 1)
        .unwrap();
    let before = service.snapshot_state();
    let proof = service
        .acceptance_proof(&parent.agent_id, sent.sequence)
        .unwrap();
    assert_eq!(proof.recipients().count(), 2);
    assert_eq!(service.snapshot_state(), before);
    let late = service.register_agent(None, None, "worker", vec![]);
    assert!(!service.acceptance_recipient_is_current(&proof, &late.agent_id));
    let reply = service
        .accept_at_with_scope(
            &second.agent_id,
            mail(
                &second,
                &parent.agent_id,
                "second-reply",
                Some("fanout-request"),
            ),
            MessageScope::Session,
            2,
        )
        .unwrap();
    let reply_proof = service
        .acceptance_proof(&second.agent_id, reply.sequence)
        .unwrap();
    assert!(!service.acceptance_reply_matches(&proof, &first.agent_id, &reply_proof));
    assert!(service.acceptance_reply_matches(&proof, &second.agent_id, &reply_proof));
    service.retire_observational_identity(&second.agent_id);
    service.retire_observational_identity(&parent.agent_id);
    assert!(service.acceptance_reply_matches(&proof, &second.agent_id, &reply_proof));
    assert!(!service.acceptance_recipient_is_current(&proof, &second.agent_id));
}

/// A fresh response addressed to a reused requester ID cannot answer the old
/// request, even if its sender is the original worker and correlation text is
/// exact. Restore similarly permits proofs only for new genuine acceptances.
#[test]
fn acceptance_proof_rejects_reused_requester_and_allows_fresh_restored_sends() {
    let mut service = MessageService::default();
    let parent = service.register_agent(None, None, "parent", vec![]);
    let child = service.register_agent(None, None, "child", vec![]);
    let sent = service
        .accept_at_with_scope(
            &parent.agent_id,
            mail(&parent, &child.agent_id, "request", None),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let proof = service
        .acceptance_proof(&parent.agent_id, sent.sequence)
        .unwrap();
    let snapshot = service.snapshot_state();
    service.retire_observational_identity(&parent.agent_id);
    service.ensure_agent_identity(parent.clone(), 2).unwrap();
    let reply = service
        .accept_at_with_scope(
            &child.agent_id,
            mail(&child, &parent.agent_id, "reply-new-owner", Some("request")),
            MessageScope::Session,
            3,
        )
        .unwrap();
    let reply_proof = service
        .acceptance_proof(&child.agent_id, reply.sequence)
        .unwrap();
    assert!(!service.acceptance_reply_matches(&proof, &child.agent_id, &reply_proof));
    let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    let fresh = restored
        .accept_at_with_scope(
            &parent.agent_id,
            mail(&parent, &child.agent_id, "fresh-request", None),
            MessageScope::Session,
            4,
        )
        .unwrap();
    let fresh_proof = restored
        .acceptance_proof(&parent.agent_id, fresh.sequence)
        .unwrap();
    assert!(restored.acceptance_recipient_is_current(&fresh_proof, &child.agent_id));
    assert!(!service.acceptance_recipient_is_current(&fresh_proof, &child.agent_id));
    assert!(service.acceptance_recipient_is_current(&proof, &child.agent_id));
    assert!(!restored.acceptance_reply_matches(&proof, &child.agent_id, &reply_proof));
}

/// Tracking metadata is reserved before sequence allocation or delivery
/// acceptance. A rejected request leaves every snapshot field unchanged, does
/// not block an ordinary bounded send, and cannot retrospectively upgrade that
/// ordinary occurrence. Exact headroom accepts a fresh tracked occurrence.
#[test]
fn acceptance_proof_budget_is_reserved_before_acceptance() {
    let mut service = MessageService::default();
    let parent = service.register_agent(None, None, "parent", vec![]);
    let child = service.register_agent(None, None, "child", vec![]);
    let envelope = mail(&parent, &child.agent_id, "request", None);
    let before = service.snapshot_state();
    let error = service
        .accept_at_with_scope_and_proof(
            &parent.agent_id,
            envelope.clone(),
            MessageScope::Session,
            1,
            1,
        )
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert!(error.message().len() < 100);
    assert_eq!(service.snapshot_state(), before);
    let ordinary = service
        .accept_at_with_scope(&parent.agent_id, envelope.clone(), MessageScope::Session, 2)
        .unwrap();
    assert_eq!(ordinary.sequence, 1);
    let before = service.snapshot_state();
    let error = service
        .accept_at_with_scope_and_proof(&parent.agent_id, envelope, MessageScope::Session, 3, 4096)
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Conflict);
    assert_eq!(service.snapshot_state(), before);
    let envelope = mail(&parent, &child.agent_id, "tracked", None);
    let bindings = vec![(child.agent_id.clone(), 0)];
    let charge = service
        .reserve_acceptance_proof(
            &envelope,
            &bindings,
            MessageAcceptanceReservation {
                metadata_bytes: 4096,
                recipients: 1,
                handoff_bytes_per_recipient: 0,
            },
        )
        .unwrap()
        .metadata_bytes();
    let before = service.snapshot_state();
    let error = service
        .accept_at_with_scope_and_proof(
            &parent.agent_id,
            envelope.clone(),
            MessageScope::Session,
            4,
            charge - 1,
        )
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert_eq!(service.snapshot_state(), before);
    let (delivery, proof) = service
        .accept_at_with_scope_and_proof(
            &parent.agent_id,
            envelope.clone(),
            MessageScope::Session,
            4,
            charge,
        )
        .unwrap();
    assert_eq!(proof.message_id(), delivery.message_id);
    assert_eq!(proof.metadata_bytes(), charge);
    assert_eq!(delivery.sequence, 2);
    assert_eq!(proof.sequence(), delivery.sequence);
    assert_eq!(
        service
            .accept_at_with_scope(&parent.agent_id, envelope, MessageScope::Session, 5)
            .unwrap(),
        delivery
    );
}

/// Failed sender authentication and lower delivery-capacity failure must not
/// publish a proof or allocate a sequence even when metadata headroom exists.
/// The same transaction remains usable by a later valid bounded acceptance.
#[test]
fn acceptance_proof_transaction_preserves_authentication_and_payload_limits() {
    let mut service = MessageService::with_limits(1, 32);
    let parent = service.register_agent(None, None, "parent", vec![]);
    let child = service.register_agent(None, None, "child", vec![]);
    let mut forged = mail(&parent, &child.agent_id, "forged", None);
    forged.sender = child.clone();
    let before = service.snapshot_state();
    assert_eq!(
        service
            .accept_at_with_scope_and_proof(
                &parent.agent_id,
                forged,
                MessageScope::Session,
                1,
                4096
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Forbidden
    );
    let mut oversized = mail(&parent, &child.agent_id, "too-large", None);
    oversized.payload = "x".repeat(33);
    assert_eq!(
        service
            .accept_at_with_scope_and_proof(
                &parent.agent_id,
                oversized,
                MessageScope::Session,
                2,
                4096
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidArgs
    );
    assert_eq!(service.snapshot_state(), before);
    assert!(service.acceptance_proof(&parent.agent_id, 1).is_none());
    let (delivery, _) = service
        .accept_at_with_scope_and_proof(
            &parent.agent_id,
            mail(&parent, &child.agent_id, "valid", None),
            MessageScope::Session,
            3,
            4096,
        )
        .unwrap();
    assert_eq!(delivery.sequence, 1);
}

/// A genuine accepted reply from the original interval remains a delivery
/// obligation after its sender retires. The read-only pending check must defer
/// lifecycle handoff without creating offers/ACKs, and must stop after actual
/// recipient acknowledgement or a trusted adapter rejects that message kind.
#[test]
fn acceptance_pending_reply_defers_retirement_without_delivery_authority() {
    let mut service = MessageService::default();
    let parent = service.register_agent(None, None, "parent", vec![]);
    let peer = service.register_agent(None, None, "peer", vec![]);
    service.subscribe(&parent.agent_id).unwrap();
    let sent = service
        .accept_at_with_scope(
            &parent.agent_id,
            mail(&parent, &peer.agent_id, "pending-request", None),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let proof = service
        .acceptance_proof(&parent.agent_id, sent.sequence)
        .unwrap();
    let reply = service
        .accept_at_with_scope(
            &peer.agent_id,
            mail(
                &peer,
                &parent.agent_id,
                "pending-reply",
                Some("pending-request"),
            ),
            MessageScope::Session,
            2,
        )
        .unwrap();
    service.retire_observational_identity(&peer.agent_id);
    assert!(!service.acceptance_recipient_is_current(&proof, &peer.agent_id));
    assert!(service.has_pending_acceptance_reply(&proof, &peer.agent_id, 3, |_| true));
    assert!(!service.has_pending_acceptance_reply(&proof, &peer.agent_id, 3, |_| false));
    assert!(
        service
            .offered_uncommitted_deliveries(&parent.agent_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        service
            .advance_subscription(&parent.agent_id, reply.sequence)
            .is_err(),
        "a read-only pending match must not invent an offer"
    );
    service.receive_subscribed(&parent.agent_id, 3, 10).unwrap();
    assert!(service.has_pending_acceptance_reply(&proof, &peer.agent_id, 3, |_| true));
    service
        .advance_subscription(&parent.agent_id, reply.sequence)
        .unwrap();
    assert!(!service.has_pending_acceptance_reply(&proof, &peer.agent_id, 3, |_| true));
}

/// Foreign correlations, another sender, same-address replacement and restored
/// history cannot reserve precedence over lifecycle outcomes. A genuinely
/// matching source keeps ordinary strict acceptance-relative TTL semantics.
#[test]
fn acceptance_pending_reply_rejects_foreign_reused_expired_and_restored_sources() {
    let mut service = MessageService::default();
    let parent = service.register_agent(None, None, "parent", vec![]);
    let peer = service.register_agent(None, None, "peer", vec![]);
    let stranger = service.register_agent(None, None, "stranger", vec![]);
    service.subscribe(&parent.agent_id).unwrap();
    let sent = service
        .accept_at_with_scope(
            &parent.agent_id,
            mail(&parent, &peer.agent_id, "pending-request", None),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let proof = service
        .acceptance_proof(&parent.agent_id, sent.sequence)
        .unwrap();
    for (sender, id, correlation) in [
        (&peer, "wrong-correlation", "other"),
        (&stranger, "wrong-sender", "pending-request"),
    ] {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(sender, &parent.agent_id, id, Some(correlation)),
                MessageScope::Session,
                2,
            )
            .unwrap();
    }
    assert!(!service.has_pending_acceptance_reply(&proof, &peer.agent_id, 3, |_| true));
    let mut expiring = mail(
        &peer,
        &parent.agent_id,
        "expiring-reply",
        Some("pending-request"),
    );
    expiring.ttl_ms = Some(1);
    service
        .accept_at_with_scope(&peer.agent_id, expiring, MessageScope::Session, 3)
        .unwrap();
    assert!(service.has_pending_acceptance_reply(&proof, &peer.agent_id, 4, |_| true));
    assert!(!service.has_pending_acceptance_reply(&proof, &peer.agent_id, 5, |_| true));
    service.settle_delivery_outcomes_at(5).unwrap();
    assert!(
        !service.has_pending_acceptance_reply(&proof, &peer.agent_id, 4, |_| true),
        "clock rollback cannot revive a terminal reply as pending precedence"
    );
    let restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    assert!(!restored.has_pending_acceptance_reply(&proof, &peer.agent_id, 4, |_| true));
    service.retire_observational_identity(&peer.agent_id);
    service.ensure_agent_identity(peer.clone(), 6).unwrap();
    service
        .accept_at_with_scope(
            &peer.agent_id,
            mail(
                &peer,
                &parent.agent_id,
                "replacement-reply",
                Some("pending-request"),
            ),
            MessageScope::Session,
            6,
        )
        .unwrap();
    assert!(!service.has_pending_acceptance_reply(&proof, &peer.agent_id, 6, |_| true));
}
