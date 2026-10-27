//! Acceptance-time routing controls independent of disposable replay retention.
//!
//! A selector is resolved once. Registration, sparse metadata reconciliation,
//! and reuse of an opaque address must not create recipients for old mail.

use super::*;

/// Creates content-free session fanout with a stable occurrence identity.
fn session_mail(sender: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:binding".into(),
        sender: sender.clone(),
        recipient: Recipient::Role("worker".into()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "binding evidence".into(),
        extension_fields: Vec::new(),
    }
}

/// A late worker and a previously offline worker were not eligible at acceptance.
/// Neither may acquire retained fanout mail after becoming available, while the
/// original recipient retains its one accepted occurrence through snapshot restore.
#[test]
fn accepted_fanout_does_not_acquire_late_or_previously_offline_recipients() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let original = service.register_agent(None, None, "worker", Vec::new());
    let offline = service.register_agent(None, None, "worker", Vec::new());
    service
        .update_presence(&offline.agent_id, AgentPresenceStatus::Offline, 1)
        .unwrap();
    service
        .subscribe_from_retained_start(&original.agent_id)
        .unwrap();
    let accepted = service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "before-registration"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    assert_eq!(accepted.queued_recipients, 1);
    let late = service.register_agent(None, None, "worker", Vec::new());
    service
        .update_presence(&offline.agent_id, AgentPresenceStatus::Available, 3)
        .unwrap();
    for target in [&late, &offline] {
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap();
        assert!(
            service
                .receive_subscribed(&target.agent_id, 4, 10)
                .unwrap()
                .messages
                .is_empty()
        );
        assert!(service.receive_for(&target.agent_id, 4).is_empty());
        assert!(
            service
                .historical_receive_through_subscribed(&target.agent_id, accepted.sequence)
                .unwrap()
                .is_empty()
        );
    }
    let restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    assert_eq!(
        restored
            .receive_subscribed(&original.agent_id, 4, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    for target in [&late, &offline] {
        assert!(
            restored
                .receive_subscribed(&target.agent_id, 4, 10)
                .unwrap()
                .messages
                .is_empty()
        );
    }
}

/// Re-registering the same opaque ID, even with identical selector metadata,
/// creates a new delivery interval. Removing a reusable subscription floor must
/// not expose the old owner's mail through live, historical, or direct reads.
#[test]
fn accepted_fanout_does_not_transfer_to_reused_recipient_identity() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let old = service.register_agent(None, None, "worker", Vec::new());
    service
        .subscribe_from_retained_start(&old.agent_id)
        .unwrap();
    let accepted = service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "old-owner"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    assert!(service.retire_observational_identity(&old.agent_id));
    service.ensure_agent_identity(old.clone(), 2).unwrap();
    service
        .subscribe_from_retained_start(&old.agent_id)
        .unwrap();
    assert!(service.receive_for(&old.agent_id, 3).is_empty());
    assert!(
        service
            .historical_receive_through_subscribed(&old.agent_id, accepted.sequence)
            .unwrap()
            .is_empty()
    );
    let restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    assert!(restored.receive_for(&old.agent_id, 3).is_empty());
}

/// Filling sparse metadata or rebinding project membership after acceptance
/// cannot retroactively select an existing registration. The original eligible
/// recipient still receives mail and later sends use the new membership.
#[test]
fn accepted_fanout_does_not_acquire_reconciled_selector_membership() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let original = service.register_agent(None, None, "worker", Vec::new());
    let mut sparse = original.clone();
    sparse.agent_id = AgentId::opaque("sparse-recipient").unwrap();
    sparse.role = None;
    service.ensure_agent_identity(sparse.clone(), 0).unwrap();
    let message = session_mail(&sender, "before-reconcile");
    let accepted = service
        .accept_at_with_scope(&sender.agent_id, message.clone(), MessageScope::Session, 1)
        .unwrap();
    sparse.role = Some("worker".into());
    service.ensure_agent_identity(sparse.clone(), 2).unwrap();
    assert!(service.receive_for(&sparse.agent_id, 3).is_empty());
    assert_eq!(service.receive_for(&original.agent_id, 3).len(), 1);
    // Idempotency keeps the original recipient count and occurrence sequence.
    assert_eq!(
        service
            .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 4)
            .unwrap(),
        accepted
    );
    let next = service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "after-reconcile"),
            MessageScope::Session,
            4,
        )
        .unwrap();
    assert_eq!(next.queued_recipients, 2);
    assert_eq!(service.receive_for(&sparse.agent_id, 5).len(), 1);
}

/// Registration intervals and accepted recipient sets are mandatory coherent
/// schema5 data. Corruptions reject before any live owner is reconstructed and
/// cannot alter the source service or reinterpret unknown legacy recipients.
#[test]
fn recipient_binding_snapshot_rejects_incomplete_or_conflicting_evidence() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    service.register_agent(None, None, "worker", Vec::new());
    service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "snapshot"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let before = service.snapshot_state();
    let mut candidates = Vec::new();
    let mut candidate = before.clone();
    candidate.registration_floors = None;
    candidates.push(candidate);
    let mut candidate = before.clone();
    candidate.registration_floors.as_mut().unwrap().pop();
    candidates.push(candidate);
    let mut candidate = before.clone();
    let duplicate = candidate.registration_floors.as_ref().unwrap()[0].clone();
    candidate
        .registration_floors
        .as_mut()
        .unwrap()
        .push(duplicate);
    candidates.push(candidate);
    let mut candidate = before.clone();
    candidate.retained_messages[0].recipient_bindings = None;
    candidates.push(candidate);
    let mut candidate = before.clone();
    candidate.retained_messages[0]
        .recipient_bindings
        .as_mut()
        .unwrap()
        .clear();
    candidates.push(candidate);
    let mut candidate = before.clone();
    let duplicate = candidate.retained_messages[0]
        .recipient_bindings
        .as_ref()
        .unwrap()[0]
        .clone();
    candidate.retained_messages[0]
        .recipient_bindings
        .as_mut()
        .unwrap()
        .push(duplicate);
    candidates.push(candidate);
    let mut candidate = before.clone();
    candidate.retained_messages[0]
        .recipient_bindings
        .as_mut()
        .unwrap()[0]
        .registered_after_sequence = 1;
    candidates.push(candidate);
    let mut candidate = before.clone();
    candidate.accepted_messages[0].delivery.queued_recipients += 1;
    candidates.push(candidate);
    for version in 2..=4 {
        let mut legacy = before.clone();
        legacy.schema_version = version;
        legacy.registration_floors = None;
        legacy.retained_messages[0].recipient_bindings = None;
        candidates.push(legacy);
    }
    for candidate in candidates {
        let decoded = serde_json::from_slice::<MessageServiceSnapshot>(
            &serde_json::to_vec(&candidate).unwrap(),
        )
        .unwrap();
        assert!(MessageService::from_snapshot_state(&decoded).is_err());
        assert_eq!(service.snapshot_state(), before);
    }
    assert_eq!(
        MessageService::from_snapshot_state(&before)
            .unwrap()
            .snapshot_state(),
        before
    );
}

/// The entire fanout metadata reservation succeeds or fails before allocating
/// a sequence. A capacity failure preserves every existing receipt and payload;
/// ordinary FIFO replacement may reuse only the history it actually evicts.
#[test]
fn recipient_binding_capacity_is_atomic_and_reuses_evicted_metadata() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    for _ in 0..4096 {
        service.register_agent(None, None, "worker", Vec::new());
    }
    assert_eq!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                session_mail(&sender, "at-cap"),
                MessageScope::Session,
                1
            )
            .unwrap()
            .queued_recipients,
        4096
    );
    service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "replace-history"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    service.register_agent(None, None, "worker", Vec::new());
    let before = service.snapshot_state();
    let error = service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "not-accepted"),
            MessageScope::Session,
            3,
        )
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert_eq!(crate::messaging::mmp_error_code(&error), "capacity");
    assert_eq!(service.snapshot_state(), before);
    MessageService::from_snapshot_state(&before).unwrap();
}

/// Selector-based retained recipients reserve generated IDs even when the raw
/// owner authored nothing and left no sender floor. Restore must not allocate
/// a late identity with that same opaque address or expose its historical mail.
#[test]
fn recipient_binding_snapshot_reserves_retired_fanout_recipient_ids() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "fanout-retired"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    service
        .retire_fresh_observational_identity(&target.agent_id)
        .unwrap();
    let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    let fresh = restored.register_agent(None, None, "worker", Vec::new());
    assert_ne!(fresh.agent_id, target.agent_id);
    assert!(restored.receive_for(&fresh.agent_id, 2).is_empty());
}

/// A project rebind cannot make an existing foreign registration an accepted
/// recipient after the fact. Returning the original recipient from offline
/// presence preserves its captured entitlement, unlike an offline-at-send peer.
#[test]
fn recipient_binding_project_rebind_and_original_presence_recovery_are_exact() {
    let mut service = MessageService::default();
    let project = ProjectScopeId::from_canonical_root_bytes(b"/binding/alpha");
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let sender = service
        .rebind_agent_project_scope(&sender.agent_id, Some(project.clone()))
        .unwrap();
    let original = service.register_agent(None, None, "worker", Vec::new());
    service
        .rebind_agent_project_scope(&original.agent_id, Some(project.clone()))
        .unwrap();
    let foreign = service.register_agent(None, None, "worker", Vec::new());
    service
        .accept_at(
            &sender.agent_id,
            session_mail(&sender, "project-before-rebind"),
            1,
        )
        .unwrap();
    service
        .rebind_agent_project_scope(&foreign.agent_id, Some(project))
        .unwrap();
    assert!(service.receive_for(&foreign.agent_id, 2).is_empty());
    service
        .update_presence(&original.agent_id, AgentPresenceStatus::Offline, 2)
        .unwrap();
    assert!(service.receive_for(&original.agent_id, 2).is_empty());
    service
        .update_presence(&original.agent_id, AgentPresenceStatus::Available, 3)
        .unwrap();
    assert_eq!(service.receive_for(&original.agent_id, 3).len(), 1);
    service
        .subscribe_from_retained_start(&original.agent_id)
        .unwrap();
    assert_eq!(
        service
            .fanout_ready_for(&original.agent_id, 3, 10)
            .unwrap()
            .unwrap()
            .batch
            .messages
            .len(),
        1
    );
    service
        .subscribe_from_retained_start(&foreign.agent_id)
        .unwrap();
    assert!(
        service
            .fanout_ready_for(&foreign.agent_id, 3, 10)
            .unwrap()
            .is_none()
    );
}

/// Payload bytes cannot hide oversized recipient IDs in a tiny message. Both
/// acceptance and snapshot restoration enforce the independent metadata charge,
/// reject without sequence/receipt mutation, and do not echo the large ID.
#[test]
fn recipient_binding_byte_capacity_is_independent_of_payload_size() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let mut target = sender.clone();
    target.agent_id = AgentId::opaque("r".repeat(1024 * 1024)).unwrap();
    target.role = Some("worker".into());
    service.ensure_agent_identity(target, 0).unwrap();
    let before = service.snapshot_state();
    let error = service
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "tiny-payload"),
            MessageScope::Session,
            1,
        )
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert!(error.message().len() < 100);
    assert_eq!(service.snapshot_state(), before);

    let mut bounded = MessageService::default();
    let sender = bounded.register_agent(None, None, "sender", Vec::new());
    bounded.register_agent(None, None, "worker", Vec::new());
    bounded
        .accept_at_with_scope(
            &sender.agent_id,
            session_mail(&sender, "snapshot-byte-bound"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let mut snapshot = bounded.snapshot_state();
    snapshot.retained_messages[0]
        .recipient_bindings
        .as_mut()
        .unwrap()[0]
        .recipient = "r".repeat(1024 * 1024);
    assert!(MessageService::from_snapshot_state(&snapshot).is_err());
}
