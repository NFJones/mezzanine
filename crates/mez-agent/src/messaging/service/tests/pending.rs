//! Accepted obligations must outlive disposable replay history.
//!
//! Tiny replay limits expose loss without timing, processes or transport retries.
//! These fixtures preserve occurrence identity, independent fanout commits and
//! receipt conflicts while unrelated traffic removes the original replay row.

use super::*;

/// Builds complete fixture mail without manufacturing acceptance or offer state.
fn mail(sender: &SenderIdentity, id: &str, recipient: Recipient) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:pending".into(),
        sender: sender.clone(),
        recipient,
        correlation_id: Some("original-request".into()),
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: format!("source-{id}"),
        extension_fields: Vec::new(),
    }
}

/// Three direct sends accepted into a two-row replay window must all remain
/// deliverable in original order. An evicted pending ID still owns its exact
/// acceptance receipt and conflict check, including after snapshot restoration.
#[test]
fn pending_direct_mail_survives_replay_pressure_and_restore() {
    let mut service = MessageService::with_limits(2, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut originals = Vec::new();
    for index in 0..3 {
        let envelope = mail(
            &sender,
            &format!("direct-{index}"),
            Recipient::Agent(target.agent_id.clone()),
        );
        let receipt = service
            .accept_at_with_scope(
                &sender.agent_id,
                envelope.clone(),
                MessageScope::Session,
                index,
            )
            .unwrap();
        originals.push((envelope, receipt));
    }
    assert_eq!(service.queue.len(), 2);
    let batch = service
        .receive_subscribed(&target.agent_id, 10, 10)
        .unwrap();
    assert_eq!(
        batch
            .messages
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    assert_eq!(
        restored
            .accept_at_with_scope(
                &sender.agent_id,
                originals[0].0.clone(),
                MessageScope::Session,
                10
            )
            .unwrap(),
        originals[0].1
    );
    let mut changed = originals[0].0.clone();
    changed.payload.push('!');
    assert_eq!(
        restored
            .accept_at_with_scope(&sender.agent_id, changed, MessageScope::Session, 10)
            .unwrap_err()
            .kind(),
        MessageErrorKind::Conflict
    );
    let batch = restored
        .receive_subscribed(&target.agent_id, 10, 10)
        .unwrap();
    assert_eq!(
        batch
            .messages
            .iter()
            .map(|row| row.envelope.as_ref().clone())
            .collect::<Vec<_>>(),
        originals
            .iter()
            .map(|(envelope, _)| envelope.clone())
            .collect::<Vec<_>>()
    );
    restored.advance_subscription(&target.agent_id, 3).unwrap();
    assert!(
        restored
            .receive_subscribed(&target.agent_id, 11, 10)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// One fast recipient commits before unrelated traffic evicts shared replay.
/// The slow recipient still receives the exact original Arc and sequence; the
/// fast ACK cannot release another recipient's obligation or duplicate delivery.
#[test]
fn pending_partial_fanout_survives_unrelated_replay_pressure() {
    let mut service = MessageService::with_limits(2, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let fast = service.register_agent(None, None, "worker", Vec::new());
    let slow = service.register_agent(None, None, "worker", Vec::new());
    for target in [&fast, &slow] {
        service.subscribe(&target.agent_id).unwrap();
    }
    let original = mail(&sender, "shared", Recipient::Role("worker".into()));
    let receipt = service
        .accept_at_with_scope(&sender.agent_id, original.clone(), MessageScope::Session, 1)
        .unwrap();
    let fast_batch = service.receive_subscribed(&fast.agent_id, 2, 10).unwrap();
    let source = fast_batch.messages[0].envelope.clone();
    service
        .advance_subscription(&fast.agent_id, receipt.sequence)
        .unwrap();
    for index in 0..3 {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(
                    &sender,
                    &format!("noise-{index}"),
                    Recipient::Agent(sender.agent_id.clone()),
                ),
                MessageScope::Session,
                3 + index,
            )
            .unwrap();
    }
    assert_eq!(service.queue.len(), 2);
    let slow_batch = service.receive_subscribed(&slow.agent_id, 10, 10).unwrap();
    assert_eq!(slow_batch.messages.len(), 1);
    assert_eq!(slow_batch.messages[0].sequence, receipt.sequence);
    assert!(Arc::ptr_eq(&slow_batch.messages[0].envelope, &source));
    assert!(
        service
            .receive_subscribed(&fast.agent_id, 10, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    service
        .advance_subscription(&slow.agent_id, receipt.sequence)
        .unwrap();
    assert!(
        service
            .receive_subscribed(&slow.agent_id, 11, 10)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Per-recipient saturation must reject before sequence/receipt mutation. One
/// genuine ACK releases exactly one occurrence and admits one replacement;
/// replay eviction alone never frees an unread recipient's reservation.
#[test]
fn pending_recipient_capacity_is_atomic_and_ack_releases_one() {
    let mut service = MessageService::with_limits(2, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    for index in 0..256 {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(
                    &sender,
                    &format!("cap-{index}"),
                    Recipient::Agent(target.agent_id.clone()),
                ),
                MessageScope::Session,
                1,
            )
            .unwrap();
    }
    let candidate = mail(
        &sender,
        "cap-next",
        Recipient::Agent(target.agent_id.clone()),
    );
    let before = service.snapshot_state();
    assert_eq!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                candidate.clone(),
                MessageScope::Session,
                1
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    MessageService::from_snapshot_state(&before).unwrap();
    let batch = service.receive_subscribed(&target.agent_id, 2, 1).unwrap();
    service
        .advance_subscription(&target.agent_id, batch.messages[0].sequence)
        .unwrap();
    assert_eq!(service.pending.len(), 255);
    service
        .accept_at_with_scope(&sender.agent_id, candidate, MessageScope::Session, 3)
        .unwrap();
    assert_eq!(service.pending.len(), 256);
}

/// The aggregate envelope ceiling is independent of per-recipient and payload
/// budgets: five recipients each own only 200 small pending occurrences.
/// A malformed restored ledger cannot bypass that same count ceiling.
#[test]
fn pending_envelope_count_is_independent_and_snapshot_bounded() {
    let mut service = MessageService::with_limits(2, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let targets = (0..5)
        .map(|_| service.register_agent(None, None, "target", Vec::new()))
        .collect::<Vec<_>>();
    for index in 0..1000 {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(
                    &sender,
                    &format!("count-{index}"),
                    Recipient::Agent(targets[index % 5].agent_id.clone()),
                ),
                MessageScope::Session,
                1,
            )
            .unwrap();
    }
    let before = service.snapshot_state();
    assert_eq!(service.pending.len(), 1000);
    assert_eq!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(
                    &sender,
                    "count-next",
                    Recipient::Agent(targets[0].agent_id.clone())
                ),
                MessageScope::Session,
                1
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    MessageService::from_snapshot_state(&before).unwrap();
    let mut excessive = before.clone();
    let mut row = excessive.retained_messages.last().unwrap().clone();
    row.sequence = excessive.next_sequence;
    row.envelope.id = "forged-extra-source".into();
    let mut receipt = excessive
        .accepted_messages
        .iter()
        .find(|accepted| accepted.envelope.id == "count-999")
        .unwrap()
        .clone();
    receipt.envelope = row.envelope.clone();
    receipt.delivery.sequence = row.sequence;
    receipt.delivery.message_id = row.envelope.id.clone();
    excessive.custody_messages.as_mut().unwrap().push(row);
    excessive.accepted_messages.push(receipt);
    excessive.next_sequence += 1;
    assert!(
        MessageService::from_snapshot_state(&excessive)
            .unwrap_err()
            .message()
            .contains("capacity")
    );
}

/// Unique payload bytes saturate before count or metadata: two individually
/// valid messages cannot exceed the independent 1MiB pending budget. Rejection
/// is inert, then the original offered ACK frees bytes for the next admission.
#[test]
fn pending_payload_bytes_are_independent_and_reusable() {
    let mut service = MessageService::with_limits(1, 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut first = mail(
        &sender,
        "bytes-first",
        Recipient::Agent(target.agent_id.clone()),
    );
    first.payload = "x".repeat(600_000);
    service
        .accept_at_with_scope(&sender.agent_id, first, MessageScope::Session, 1)
        .unwrap();
    let mut second = mail(
        &sender,
        "bytes-next",
        Recipient::Agent(target.agent_id.clone()),
    );
    second.payload = "y".repeat(500_000);
    let before = service.snapshot_state();
    assert_eq!(
        service
            .accept_at_with_scope(&sender.agent_id, second.clone(), MessageScope::Session, 1)
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    service.receive_subscribed(&target.agent_id, 2, 1).unwrap();
    service.advance_subscription(&target.agent_id, 1).unwrap();
    service
        .accept_at_with_scope(&sender.agent_id, second, MessageScope::Session, 3)
        .unwrap();
    assert_eq!(service.pending.len(), 1);
    MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
}

/// A committed fanout slot survives recipient retirement and snapshot restore.
/// The remaining recipient's later ACK releases the shared custody reference;
/// reusing the retired ID neither inherits source nor blocks that release.
#[test]
fn pending_fast_commit_survives_retirement_until_slow_commit() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let fast = service.register_agent(None, None, "worker", Vec::new());
    let slow = service.register_agent(None, None, "worker", Vec::new());
    for target in [&fast, &slow] {
        service.subscribe(&target.agent_id).unwrap();
    }
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, "shared-retired", Recipient::Role("worker".into())),
            MessageScope::Session,
            1,
        )
        .unwrap();
    service.receive_subscribed(&fast.agent_id, 2, 1).unwrap();
    service.advance_subscription(&fast.agent_id, 1).unwrap();
    service.retire_agent_identity(&fast.agent_id);
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(
                &sender,
                "unrelated",
                Recipient::Agent(sender.agent_id.clone()),
            ),
            MessageScope::Session,
            3,
        )
        .unwrap();
    let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    restored.ensure_agent_identity(fast.clone(), 4).unwrap();
    restored
        .subscribe_from_retained_start(&fast.agent_id)
        .unwrap();
    assert!(
        restored
            .receive_subscribed(&fast.agent_id, 4, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    restored.receive_subscribed(&slow.agent_id, 4, 1).unwrap();
    restored.advance_subscription(&slow.agent_id, 1).unwrap();
    assert!(!restored.pending.contains_key(&1));
    assert!(!restored.accepted_messages.contains_key("shared-retired"));
    assert!(
        restored
            .snapshot_state()
            .custody_messages
            .unwrap()
            .is_empty()
    );
}

/// Snapshot source membership and commit evidence are cross-record contracts,
/// not permission to omit unread payload. Reject duplicate sources, absent or
/// foreign commits, commits without offers and commits beyond the live cursor.
#[test]
fn pending_snapshot_rejects_forged_sources_and_commits() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    for index in 0..2 {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(
                    &sender,
                    &format!("snapshot-{index}"),
                    Recipient::Agent(target.agent_id.clone()),
                ),
                MessageScope::Session,
                1,
            )
            .unwrap();
    }
    // Subscribing late cannot skip accepted unread occurrences.
    service.subscribe(&target.agent_id).unwrap();
    let selected = service.receive_subscribed(&target.agent_id, 2, 10).unwrap();
    assert_eq!(selected.messages.len(), 2);
    let before = service.snapshot_state();
    let mut candidates = Vec::new();
    let mut bad = before.clone();
    bad.custody_messages = None;
    candidates.push(bad);
    let mut bad = before.clone();
    bad.custody_messages
        .as_mut()
        .unwrap()
        .push(bad.retained_messages[0].clone());
    candidates.push(bad);
    let mut bad = before.clone();
    bad.custody_messages.as_mut().unwrap()[0].acknowledged_recipients = None;
    candidates.push(bad);
    let mut bad = before.clone();
    bad.custody_messages.as_mut().unwrap()[0].acknowledged_recipients =
        Some(vec![sender.agent_id.to_string()]);
    candidates.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0].acknowledged_recipients = Some(vec![target.agent_id.to_string()]);
    candidates.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0].offered_recipients = Some(Vec::new());
    bad.retained_messages[0].acknowledged_recipients = Some(vec![target.agent_id.to_string()]);
    candidates.push(bad);
    for candidate in candidates {
        assert!(MessageService::from_snapshot_state(&candidate).is_err());
        assert_eq!(service.snapshot_state(), before);
    }
    service.advance_subscription(&target.agent_id, 2).unwrap();
    assert!(service.pending.is_empty());
    assert_eq!(
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap()
            .last_sequence,
        2
    );
    assert_eq!(
        service
            .receive_through_subscribed(&target.agent_id, 2, 3)
            .unwrap()
            .len(),
        1
    );
}

/// An advanced schema7 cursor cannot conceal explicitly uncommitted live mail.
/// Such a restore would expose zero deliverable rows while retaining saturated
/// custody that duplicate/regressive ACKs can never release.
#[test]
fn pending_restore_rejects_cursor_hiding_live_uncommitted_mail() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    for index in 0..256 {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(
                    &sender,
                    &format!("hidden-{index}"),
                    Recipient::Agent(target.agent_id.clone()),
                ),
                MessageScope::Session,
                1,
            )
            .unwrap();
    }
    let before = service.snapshot_state();
    let mut forged = before.clone();
    forged.subscriptions[0].last_sequence = 256;
    assert!(MessageService::from_snapshot_state(&forged).is_err());
    assert_eq!(service.snapshot_state(), before);
}

/// Actual schema5 has no offer evidence even for genuinely committed mail.
/// Advanced exact legacy cursors without retained offers must explicitly refuse
/// migration, not silently adopt inaccessible pending capacity or invent ACKs.
/// Schema6 unknown-offer/advanced-cursor state needs the same refusal policy.
#[test]
fn pending_restore_refuses_unprovable_legacy_advanced_cursors() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    for index in 0..256 {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(
                    &sender,
                    &format!("legacy-{index}"),
                    Recipient::Agent(target.agent_id.clone()),
                ),
                MessageScope::Session,
                1,
            )
            .unwrap();
    }
    service
        .receive_subscribed(&target.agent_id, 2, 256)
        .unwrap();
    service.advance_subscription(&target.agent_id, 256).unwrap();
    let committed = service.snapshot_state();
    let mut proven = committed.clone();
    proven.schema_version = 6;
    proven.custody_messages = None;
    for row in &mut proven.retained_messages {
        row.acknowledged_recipients = None;
    }
    let mut upgraded = MessageService::from_snapshot_state(&proven).unwrap();
    assert!(upgraded.pending.is_empty());
    upgraded
        .accept_at_with_scope(
            &sender.agent_id,
            mail(
                &sender,
                "proven-next",
                Recipient::Agent(target.agent_id.clone()),
            ),
            MessageScope::Session,
            3,
        )
        .unwrap();
    for schema in [5, 6] {
        let mut legacy = committed.clone();
        legacy.schema_version = schema;
        legacy.custody_messages = None;
        for row in &mut legacy.retained_messages {
            row.acknowledged_recipients = None;
            row.offered_recipients = if schema == 5 { None } else { Some(Vec::new()) };
        }
        assert!(
            MessageService::from_snapshot_state(&legacy).is_err(),
            "schema {schema}"
        );
    }
    assert_eq!(service.snapshot_state(), committed);
}

/// A genuine unoffered expired prefix may remain below an advanced cursor.
/// Retain its bounded exclusion witness even after the later ACK endpoint has
/// lost both replay and pending references. Restore must not reject this valid
/// state, invent a recipient commit, or require the evicted endpoint payload.
#[test]
fn pending_restore_preserves_expired_prefix_after_ack_endpoint_eviction() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut expiring = mail(
        &sender,
        "expired-prefix",
        Recipient::Agent(target.agent_id.clone()),
    );
    expiring.ttl_ms = Some(5);
    service
        .accept_at_with_scope(&sender.agent_id, expiring, MessageScope::Session, 1)
        .unwrap();
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(
                &sender,
                "ack-endpoint",
                Recipient::Agent(target.agent_id.clone()),
            ),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let selected = service.receive_subscribed(&target.agent_id, 7, 10).unwrap();
    assert_eq!(selected.messages.len(), 1);
    assert_eq!(selected.messages[0].sequence, 2);
    service.advance_subscription(&target.agent_id, 2).unwrap();
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(
                &sender,
                "evict-endpoint",
                Recipient::Agent(sender.agent_id.clone()),
            ),
            MessageScope::Session,
            8,
        )
        .unwrap();
    let snapshot = service.snapshot_state();
    assert!(!snapshot.canonical_messages().any(|row| row.sequence == 2));
    let expired = &snapshot.custody_messages.as_ref().unwrap()[0];
    assert_eq!(expired.ack_expiry_observed_at_ms, Some(7));
    assert!(expired.acknowledged_recipients.as_ref().unwrap().is_empty());
    assert!(expired.offered_recipients.as_ref().unwrap().is_empty());
    let restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    assert_eq!(restored.snapshot_state(), snapshot);
    assert!(
        restored
            .receive_subscribed(&target.agent_id, 9, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    for observed in [0, 6] {
        let mut forged = snapshot.clone();
        forged.custody_messages.as_mut().unwrap()[0].ack_expiry_observed_at_ms = Some(observed);
        assert!(MessageService::from_snapshot_state(&forged).is_err());
    }
    let mut missing = snapshot.clone();
    missing.custody_messages.as_mut().unwrap()[0].ack_expiry_observed_at_ms = None;
    assert!(MessageService::from_snapshot_state(&missing).is_err());
}

/// A later endpoint may have been genuinely selected before an earlier ACK
/// excluded an expired prefix. Subsequent advancement must not revalidate that
/// already excluded source against the later endpoint's older selection time.
/// All selection clocks increase; no clock rollback or synthetic offer is used.
#[test]
fn pending_ack_preserves_prior_expiry_exclusion_with_preoffered_endpoint() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    for index in 1..=3 {
        let mut message = mail(
            &sender,
            &format!("preoffered-{index}"),
            Recipient::Agent(target.agent_id.clone()),
        );
        if index == 1 {
            message.ttl_ms = Some(5);
        }
        service
            .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 1)
            .unwrap();
    }
    let later = service
        .receive_after(
            &DeliveryCursor {
                recipient: target.agent_id.clone(),
                last_sequence: 2,
            },
            4,
            1,
        )
        .unwrap();
    assert_eq!(later.messages[0].sequence, 3);
    let earlier = service.receive_subscribed(&target.agent_id, 7, 1).unwrap();
    assert_eq!(earlier.messages[0].sequence, 2);
    service.advance_subscription(&target.agent_id, 2).unwrap();
    let excluded = service.snapshot_state().custody_messages.unwrap()[0].clone();
    assert_eq!(excluded.ack_expiry_observed_at_ms, Some(7));
    // Verify both live and restored owners can consume the already offered
    // endpoint without reselection or replacement of the original exclusion.
    let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    for owner in [&mut service, &mut restored] {
        owner.advance_subscription(&target.agent_id, 3).unwrap();
        assert_eq!(
            owner.subscription(&target.agent_id).unwrap().last_sequence,
            3
        );
        assert_eq!(
            owner.snapshot_state().custody_messages.unwrap(),
            vec![excluded.clone()]
        );
        assert!(
            owner
                .receive_subscribed(&target.agent_id, 8, 10)
                .unwrap()
                .messages
                .is_empty()
        );
        MessageService::from_snapshot_state(&owner.snapshot_state()).unwrap();
    }
}
