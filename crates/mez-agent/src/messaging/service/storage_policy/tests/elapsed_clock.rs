//! Original-domain timing across real lower admission, receive and terminal APIs.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
struct Clock(AtomicU64);
impl Clock {
    fn at(now: u64) -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(now)))
    }
    fn set(&self, now: u64) {
        self.0.store(now, Ordering::Release);
    }
}
impl MessageStorageClock for Clock {
    fn millis(&self) -> Result<u64> {
        let now = self.0.load(Ordering::Acquire);
        (now != 0)
            .then_some(now)
            .ok_or_else(|| MessageError::invalid_state("fixture native clock unavailable"))
    }
}
impl MessageTerminalStore for Store {
    fn commit(&mut self, write: &MessageTerminalWrite) -> Result<()> {
        let mut bytes = Vec::new();
        write.write_record(&mut bytes).unwrap();
        assert_eq!(bytes.len(), write.encoded_bytes());
        Ok(())
    }
}

impl MessageNativeContinuityStore for Store {
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> Result<()> {
        write.snapshot().validate()
    }
}

/// Clock, native and registration source metadata share one admission ceiling.
/// Separate partial preflights must not admit a source whose combined charge
/// exceeds that ceiling, even by one byte. The positive boundary uses the same
/// actual lower storage issuance rather than constructing a receipt.
#[test]
fn elapsed_admission_reserves_combined_source_metadata_before_sequence() {
    fn scene(
        policy: MessageStorageLimits,
        native: bool,
        fanout: bool,
    ) -> (
        MessageService,
        MessageStorageScope,
        SenderIdentity,
        SenderIdentity,
        Store,
    ) {
        let (mut service, scope, sender, target, mut store) = fixture(policy);
        let mut identities = vec![sender.clone(), target.clone()];
        if fanout {
            identities.push(service.register_agent(None, None, "target", vec![]));
        }
        if native {
            for identity in &identities {
                let registration = service
                    .capture_sender_registration(&identity.agent_id)
                    .unwrap();
                service
                    .bind_native_conversation(
                        &registration,
                        "native-conversation",
                        &MessageNativeIncarnation::default(),
                    )
                    .unwrap();
            }
        }
        service.require_native_continuity(&scope).unwrap();
        if native {
            for identity in &identities {
                let request = service
                    .prepare_native_continuity(&scope, &identity.agent_id)
                    .unwrap();
                let verified = request.write().persist(&mut store).unwrap();
                assert!(request.finish(&mut service, verified).unwrap());
            }
        }
        service
            .install_storage_clock(&scope, [9; 16], Clock::at(100))
            .unwrap();
        if native {
            // Genuine pending originals fill metadata after the startup journal
            // scratch retires. The narrowed admission cap still permits those
            // actual earlier continuity writes; no policy is changed mid-run.
            for index in 0..5 {
                let mut envelope = mail(&sender, &target, &format!("seed-{index}"), 0);
                envelope.time = "t".repeat(110_000);
                accept(&mut service, &scope, &mut store, envelope);
            }
        }
        (service, scope, sender, target, store)
    }

    fn candidate(sender: &SenderIdentity, target: &SenderIdentity, fanout: bool) -> Envelope {
        let mut envelope = mail(sender, target, "combined", 8);
        if fanout {
            envelope.recipient = Recipient::Role("target".into());
        }
        envelope
    }

    for (native, fanout) in [(false, false), (false, true), (true, true)] {
        let (mut reference, scope, sender, target, mut store) = scene(limits(), native, fanout);
        accept(
            &mut reference,
            &scope,
            &mut store,
            candidate(&sender, &target, fanout),
        );
        let exact = reference.storage_usage().unwrap().unwrap().metadata_bytes;
        for (budget, accepted) in [(exact - 1, false), (exact, true)] {
            let mut policy = limits();
            policy.metadata_bytes = budget;
            let (mut service, scope, sender, target, mut store) = scene(policy, native, fanout);
            let envelope = candidate(&sender, &target, fanout);
            let before = service.storage_usage().unwrap().unwrap();
            let sequence = service.last_sequence();
            let pending = service.pending.len();
            let receipts = service.accepted_messages.len();
            let originals = store.originals.clone();
            if accepted {
                let delivery = accept(&mut service, &scope, &mut store, envelope);
                assert_eq!(service.last_sequence(), sequence + 1);
                assert_eq!(delivery.queued_recipients, if fanout { 2 } else { 1 });
                assert_eq!(
                    service.storage_usage().unwrap().unwrap().metadata_bytes,
                    exact
                );
                assert_eq!(store.originals.len(), originals.len() + 1);
            } else {
                for _ in 0..2 {
                    let error = service
                        .prepare_acceptance(
                            &sender.agent_id,
                            envelope.clone(),
                            MessageScope::Session,
                            10,
                            4096,
                            None,
                        )
                        .err()
                        .expect("combined metadata must refuse before reserving an occurrence");
                    assert_eq!(error.kind(), MessageErrorKind::Capacity);
                    assert_eq!(service.last_sequence(), sequence);
                    assert!(service.admission.is_none());
                    assert_eq!(service.pending.len(), pending);
                    assert_eq!(service.accepted_messages.len(), receipts);
                    let usage = service.storage_usage().unwrap().unwrap();
                    assert_eq!(usage.metadata_bytes, before.metadata_bytes);
                    assert!(usage.metadata_bytes <= budget);
                    assert_eq!(usage.cache, before.cache);
                    assert_eq!(store.originals, originals);
                }
            }
        }
    }
}

/// A changed wall sample cannot expire or revive custody. Source4 carries the
/// original domain and acceptance time; receive/duplicate/expiry all use the
/// installed reader and preserve the original strict TTL boundary.
#[test]
fn elapsed_clock_owns_acceptance_offer_expiry_and_duplicate_status() {
    let (mut service, scope, sender, target, mut store) = fixture(limits());
    service.require_native_continuity(&scope).unwrap();
    let clock = Clock::at(100);
    let domain = [9; 16];
    service
        .install_storage_clock(&scope, domain, clock.clone())
        .unwrap();
    let mut envelope = mail(&sender, &target, "elapsed", 8);
    envelope.ttl_ms = Some(5);
    let delivery = accept(&mut service, &scope, &mut store, envelope.clone());
    let source = decode_message_acceptance(
        store.originals[&delivery.sequence].as_slice(),
        MessageAcceptanceDecodeLimits {
            payload_bytes: 64,
            auxiliary_bytes: 4096,
            recipient_count: 16,
            recipient_bytes: 4096,
        },
    )
    .unwrap();
    let metadata = crate::messaging::decode_message_acceptance_metadata(
        store.originals[&delivery.sequence].as_slice(),
        MessageAcceptanceDecodeLimits {
            payload_bytes: 64,
            auxiliary_bytes: 4096,
            recipient_count: 16,
            recipient_bytes: 4096,
        },
    )
    .unwrap();
    assert_eq!(metadata.custody_clock_domain(), Some(domain));
    assert_eq!(
        metadata.recipient_registration_namespace(0),
        source.recipient_registration_namespace(0)
    );
    assert_eq!(metadata.native_sender(), source.native_sender());
    assert_eq!(metadata.native_recipient(0), source.native_recipient(0));
    assert_eq!(metadata.envelope().payload.bytes(), envelope.payload.len());
    assert_eq!(source.custody_clock_domain(), Some(domain));
    assert_eq!(source.accepted_at_ms(), 100);
    assert!(
        String::from_utf8(store.originals[&delivery.sequence].clone())
            .unwrap()
            .contains("\"schema_version\":4")
    );
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, u64::MAX, 1)
            .unwrap()
            .messages
            .len(),
        1
    );
    clock.set(105);
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 0, 1)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        service
            .prepare_storage_expiry(&scope, u64::MAX, 8 * 1024 * 1024)
            .unwrap()
            .is_none()
    );
    clock.set(106);
    assert!(
        service
            .receive_subscribed(&target.agent_id, 0, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        service
            .storage_qualified_duplicate(
                &sender.agent_id,
                &envelope,
                &scope,
                MessageScope::Session,
                0
            )
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Expired
    );
    let ticket = service
        .prepare_storage_expiry(&scope, 0, 8 * 1024 * 1024)
        .unwrap()
        .unwrap();
    let write = service
        .begin_storage_terminal_write(&ticket, &scope)
        .unwrap();
    assert_eq!(write.observed_at_ms(), 106);
}

/// Terminal observation/release uses the same finite elapsed interval. Native
/// failure after issuance cannot replace its first clock or prevent adoption;
/// fresh admission/receive fails closed and the reader cannot be replaced.
#[test]
fn elapsed_clock_terminal_first_attempt_and_horizon_survive_wall_rollback() {
    let (mut service, scope, sender, target, mut store) = fixture(limits());
    service.require_native_continuity(&scope).unwrap();
    let clock = Clock::at(100);
    service
        .install_storage_clock(&scope, [9; 16], clock.clone())
        .unwrap();
    let mut envelope = mail(&sender, &target, "terminal", 8);
    envelope.ttl_ms = Some(5);
    accept(&mut service, &scope, &mut store, envelope);
    clock.set(106);
    let ticket = service
        .prepare_storage_expiry(&scope, 0, 8 * 1024 * 1024)
        .unwrap()
        .unwrap();
    let write = service
        .begin_storage_terminal_write(&ticket, &scope)
        .unwrap();
    clock.set(0);
    assert!(
        service
            .receive_subscribed(&target.agent_id, u64::MAX, 1)
            .is_err()
    );
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                mail(&sender, &target, "denied", 8),
                MessageScope::Session,
                1,
                4096,
                None
            )
            .is_err()
    );
    assert!(
        service
            .install_storage_clock(&scope, [9; 16], Clock::at(1))
            .is_err()
    );
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut store) else {
        panic!("original terminal must settle")
    };
    assert_eq!(service.publish_terminal(&commit).unwrap(), 1);
    clock.set(106 + MESSAGE_TERMINAL_HANDOFF_MS - 1);
    assert!(
        service
            .prepare_storage_horizon_release(&scope, u64::MAX, 8 * 1024 * 1024)
            .unwrap()
            .is_none()
    );
    clock.set(106 + MESSAGE_TERMINAL_HANDOFF_MS);
    let ticket = service
        .prepare_storage_horizon_release(&scope, 0, 8 * 1024 * 1024)
        .unwrap()
        .unwrap();
    let write = service
        .begin_storage_terminal_write(&ticket, &scope)
        .unwrap();
    assert_eq!(write.observed_at_ms(), 106 + MESSAGE_TERMINAL_HANDOFF_MS);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut store) else {
        panic!("original horizon must settle")
    };
    assert_eq!(service.publish_terminal(&commit).unwrap(), 1);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        0
    );
}

/// A later source spills normally. Its reloader must budget and verify clock
/// metadata before discarding the decoded wrapper, so substituted domain bytes
/// cannot be masked by recomputing the digest from the original canonical row.
#[test]
fn elapsed_clock_payload_reload_preserves_domain_and_rejects_substitution() {
    let (mut service, scope, sender, target, mut store) = fixture(limits());
    service.require_native_continuity(&scope).unwrap();
    service
        .install_storage_clock(&scope, [9; 16], Clock::at(100))
        .unwrap();
    let first = accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "first", 40),
    );
    let second = accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "second", 40),
    );
    let mut first_loaded = service
        .begin_payload_load(first.sequence, &scope)
        .unwrap()
        .unwrap()
        .load(&mut store);
    assert!(service.publish_payload_load(&mut first_loaded).unwrap());
    let batch = service.receive_subscribed(&target.agent_id, 0, 1).unwrap();
    assert_eq!(batch.messages[0].sequence, first.sequence);
    acknowledge(&mut service, &scope, &mut store, &target, first.sequence);
    drop(batch);
    let original = store.originals[&second.sequence].clone();
    // Preserve original canonical key ordering; replacing a fixed array byte
    // changes only descriptive domain attribution, not parser framing/order.
    let text = String::from_utf8(original.clone()).unwrap();
    let changed = text
        .replacen("\"custody_clock\":[9,", "\"custody_clock\":[8,", 1)
        .into_bytes();
    assert_ne!(changed, original);
    store.originals.insert(second.sequence, changed);
    let mut loaded = service
        .begin_payload_load(second.sequence, &scope)
        .unwrap()
        .unwrap()
        .load(&mut store);
    assert!(service.publish_payload_load(&mut loaded).is_err());
    assert!(
        service.snapshot_state().is_err(),
        "generic capture must not lose clock attribution"
    );
}
