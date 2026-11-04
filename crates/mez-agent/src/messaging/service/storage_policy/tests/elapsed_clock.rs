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
