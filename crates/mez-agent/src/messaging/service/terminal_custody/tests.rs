//! Real lower source/reservation tests; product tests must qualify actual I/O.
//!
//! Fixture stores exercise the trusted synchronous callback contract and inspect
//! real lower encodings. Their success is not filesystem/crash-durability proof.

use super::*;

/// Retains exact encoded data so retries must reconcile, not regenerate a plan.
#[derive(Default)]
struct Store {
    original: Option<Vec<u8>>,
    fail: bool,
}
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        _: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        Ok(())
    }
}
impl MessageAcknowledgementStore for Store {
    fn commit(&mut self, _: &MessageAcknowledgementWrite) -> Result<()> {
        Ok(())
    }
}
impl MessageTerminalStore for Store {
    fn commit(&mut self, write: &MessageTerminalWrite) -> Result<()> {
        let mut bytes = Vec::new();
        write.write_record(&mut bytes).unwrap();
        assert_eq!(bytes.len(), write.encoded_bytes());
        assert_eq!(
            &bytes[..8],
            match write.kind() {
                MessageTerminalKind::ExpiryObservation => b"MMPEXP01",
                MessageTerminalKind::RecipientRetirement => b"MMPRET01",
                MessageTerminalKind::HorizonRelease => b"MMPREL01",
                MessageTerminalKind::SenderConsumption => b"MMPCON01",
            }
        );
        if let Some(original) = &self.original {
            assert_eq!(original, &bytes);
        }
        self.original = Some(bytes);
        if self.fail {
            Err(MessageError::invalid_state("lower fixture lost reply"))
        } else {
            Ok(())
        }
    }
}

/// One genuine original sender/recipient and accepted source under a real lower
/// storage scope. No byte decoder or label creates the tickets/proofs below.
struct Scene {
    service: MessageService,
    scope: MessageStorageScope,
    sender: SenderIdentity,
    target: SenderIdentity,
}
impl Scene {
    fn new(guarded: bool, native: bool) -> Self {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "worker", vec![]);
        let scope = MessageStorageScope::default();
        service
            .enable_storage_custody(&scope, Default::default())
            .unwrap();
        if native {
            let registration = service
                .capture_sender_registration(&target.agent_id)
                .unwrap();
            service
                .bind_native_conversation(
                    &registration,
                    "original",
                    &MessageNativeIncarnation::default(),
                )
                .unwrap();
        }
        service.subscribe(&target.agent_id).unwrap();
        if guarded {
            service.require_native_continuity(&scope).unwrap();
        }
        Self {
            service,
            scope,
            sender,
            target,
        }
    }
    fn accept(&mut self, recipient: Recipient, ttl_ms: Option<u64>) {
        self.accept_named("actual-expiring-source", recipient, ttl_ms);
    }
    fn accept_named(&mut self, id: &str, recipient: Recipient, ttl_ms: Option<u64>) {
        let message = Envelope {
            protocol: MMP_PROTOCOL,
            id: id.into(),
            message_type: "send".into(),
            time: "original".into(),
            sender: self.sender.clone(),
            recipient,
            correlation_id: None,
            ttl_ms,
            content_type: "text/plain".into(),
            payload: "original untrusted payload".into(),
            extension_fields: vec![],
        };
        let ticket = self
            .service
            .prepare_acceptance(
                &self.sender.agent_id,
                message,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let write = self
            .service
            .begin_storage_acceptance_write(&ticket, &self.scope)
            .unwrap();
        let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
        else {
            panic!("fixture source must commit");
        };
        self.service.publish_acceptance(&commit).unwrap();
    }
    fn direct(&mut self) {
        self.accept(Recipient::Agent(self.target.agent_id.clone()), Some(5));
    }
    fn offer(&self) {
        assert_eq!(
            self.service
                .receive_subscribed(&self.target.agent_id, 11, 1)
                .unwrap()
                .messages
                .len(),
            1
        );
    }
    fn reserve(&mut self) -> MessageTerminalTicket {
        self.service
            .prepare_storage_expiry(&self.scope, 16, 8 * 1024 * 1024)
            .unwrap()
            .unwrap()
    }
    fn write(&mut self, ticket: &MessageTerminalTicket) -> MessageTerminalWrite {
        self.service
            .begin_storage_terminal_write(ticket, &self.scope)
            .unwrap()
    }
}

/// Selection and trusted store success remain distinct from actor adoption.
/// Rollback cannot reoffer a reserved slot, and only adoption exposes the first
/// outcome. Guarded volatile consumption/aging cannot erase the durable owner.
#[test]
fn expiry_original_requires_store_positive_and_exact_adoption() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    scene.offer();
    assert!(
        scene
            .service
            .prepare_storage_expiry(&scene.scope, 15, 8 * 1024 * 1024)
            .unwrap()
            .is_none()
    );
    let before = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    let ticket = scene.reserve();
    assert!(ticket.metadata_bytes() > 4096);
    assert!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes
            > before
    );
    assert!(scene.service.has_unpublished_terminal());
    assert!(scene.service.snapshot_state().is_err());
    assert!(
        scene
            .service
            .prepare_storage_snapshot(&scene.scope)
            .is_err()
    );
    assert!(
        scene
            .service
            .receive_subscribed(&scene.target.agent_id, 11, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(
        scene
            .service
            .receive_for(&scene.target.agent_id, 11)
            .is_empty()
    );
    assert!(
        scene
            .service
            .prepare_acknowledgement(&scene.target.agent_id, 1, 512 * 1024)
            .is_err()
    );
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    let write = scene.write(&ticket);
    assert_eq!(write.sequence(), 1);
    assert_eq!(write.accepted_at_ms(), 10);
    assert_eq!(write.ttl_ms(), Some(5));
    assert_eq!(write.binding_indices().collect::<Vec<_>>(), vec![0]);
    assert!(write.belongs_to(&scene.scope));
    assert!(!write.belongs_to(&MessageStorageScope::default()));
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("trusted lower port positive required");
    };
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 0);
    assert!(!scene.service.has_unpublished_terminal());
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    let outcomes = scene
        .service
        .delivery_outcomes_for(&scene.sender.agent_id, 10)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].reason, DeliveryTerminalReason::Expired);
    assert_eq!(outcomes[0].observed_at_ms, 16);
    assert!(
        scene
            .service
            .acknowledge_delivery_outcome(&scene.sender.agent_id, &outcomes[0])
            .is_err()
    );
    assert_eq!(
        scene.service.settle_delivery_outcomes_at(u64::MAX).unwrap(),
        0
    );
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap(),
        outcomes
    );
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
}

/// A possible committed write retains its original bytes and slot fence through
/// retry; only a new attempt nonce changes. No preissuance cancel or replacement
/// recipient can turn uncertainty into expiry or receive/cursor authority.
#[test]
fn expiry_uncertainty_preserves_first_observation_and_original_slots() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    scene.offer();
    let ticket = scene.reserve();
    let write = scene.write(&ticket);
    let original = write.attempt();
    let mut store = Store {
        fail: true,
        ..Default::default()
    };
    let MessageTerminalWriteOutcome::Unknown(unknown) = write.persist(&mut store) else {
        panic!("actual fixture uncertainty required");
    };
    assert!(unknown.error().message().contains("lost reply"));
    assert!(scene.service.cancel_storage_terminal(&ticket).is_err());
    assert!(
        scene
            .service
            .begin_storage_terminal_write(&ticket, &scene.scope)
            .is_err()
    );
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    assert!(!write.attempt().same_attempt(&original));
    assert!(scene.service.validate_terminal_attempt(&original).is_err());
    assert_eq!(write.observed_at_ms(), 16);
    let replacement = scene
        .service
        .register_agent(None, None, "replacement", vec![]);
    let replacement_cursor = scene.service.subscribe(&replacement.agent_id).unwrap();
    store.fail = false;
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut store) else {
        panic!("retry must reconcile");
    };
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    assert_eq!(
        scene
            .service
            .subscription(&replacement.agent_id)
            .unwrap()
            .last_sequence,
        replacement_cursor.last_sequence
    );
    assert!(
        scene
            .service
            .receive_subscribed(&replacement.agent_id, 11, 10)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// The lower callback shape requires the exact prior worker escrow. A refused
/// join leaves its nonce live; a qualified joined loss issues a new nonce and
/// makes an old positive stale until the current original reconciles.
#[test]
fn expiry_joined_loss_and_stale_positive_require_exact_attempt() {
    struct Join {
        expected: MessageTerminalAttempt,
        allow: bool,
    }
    impl MessageTerminalJoin for Join {
        fn qualify_joined(&mut self, actual: &MessageTerminalAttempt) -> Result<()> {
            if self.allow && self.expected.same_attempt(actual) {
                Ok(())
            } else {
                Err(changed())
            }
        }
    }
    let mut scene = Scene::new(true, false);
    scene.direct();
    let ticket = scene.reserve();
    let write = scene.write(&ticket);
    let old = write.attempt();
    let mut store = Store::default();
    let MessageTerminalWriteOutcome::Committed(stale) = write.persist(&mut store) else {
        panic!("fixture positive required");
    };
    let mut join = Join {
        expected: old.clone(),
        allow: false,
    };
    assert!(
        scene
            .service
            .recover_joined_terminal_write(&old, &mut join)
            .is_err()
    );
    scene.service.validate_terminal_attempt(&old).unwrap();
    join.allow = true;
    let write = scene
        .service
        .recover_joined_terminal_write(&old, &mut join)
        .unwrap();
    assert!(scene.service.publish_terminal(&stale).is_err());
    let mut foreign = Scene::new(true, false);
    foreign.direct();
    assert!(foreign.service.publish_terminal(&stale).is_err());
    let MessageTerminalWriteOutcome::Committed(current) = write.persist(&mut store) else {
        panic!("actual lower retry required");
    };
    assert_eq!(scene.service.publish_terminal(&current).unwrap(), 1);
}

/// Only a never-issued reservation may release its fence. Foreign scope/budget
/// refusal must not mutate source or create an owner; an absent TTL never yields
/// expiry even at the largest representable clock.
#[test]
fn expiry_preflight_and_cancel_leave_source_eligible_without_terminal_fact() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    scene.offer();
    assert!(
        scene
            .service
            .prepare_storage_expiry(&MessageStorageScope::default(), 16, usize::MAX)
            .is_err()
    );
    assert!(
        scene
            .service
            .prepare_storage_expiry(&scene.scope, 16, 1)
            .is_err()
    );
    assert!(!scene.service.has_unpublished_terminal());
    let ticket = scene.reserve();
    assert!(
        scene
            .service
            .begin_storage_terminal_write(&ticket, &MessageStorageScope::default())
            .is_err()
    );
    scene.service.cancel_storage_terminal(&ticket).unwrap();
    scene.offer();
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    let mut unbounded = Scene::new(true, false);
    unbounded.accept(Recipient::Agent(unbounded.target.agent_id.clone()), None);
    assert!(
        unbounded
            .service
            .prepare_storage_expiry(&unbounded.scope, u64::MAX, usize::MAX)
            .unwrap()
            .is_none()
    );
}

/// Real original ACK custody excludes expiry, and a live opaque native input
/// anchor protects its source even after the acceptance TTL boundary. Removing
/// the final native anchor permits expiry without transferring native authority.
#[test]
fn expiry_respects_actual_ack_and_native_input_protections() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    scene.offer();
    let ack = scene
        .service
        .prepare_acknowledgement(&scene.target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    assert!(
        scene
            .service
            .prepare_storage_expiry(&scene.scope, 16, usize::MAX)
            .unwrap()
            .is_none()
    );
    let write = scene
        .service
        .begin_storage_acknowledgement_write(&ack, &scene.scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut Store::default())
    else {
        panic!("lower ACK required");
    };
    scene.service.publish_acknowledgement(&commit).unwrap();
    assert!(
        scene
            .service
            .prepare_storage_expiry(&scene.scope, 16, usize::MAX)
            .unwrap()
            .is_none()
    );
    let mut native = Scene::new(false, true);
    native.direct();
    native.offer();
    let anchor = native
        .service
        .capture_native_input_anchor(&native.target.agent_id, 1)
        .unwrap();
    assert!(
        native
            .service
            .prepare_storage_expiry(&native.scope, 16, usize::MAX)
            .unwrap()
            .is_none()
    );
    drop(anchor);
    let ticket = native.reserve();
    assert!(native.service.capture_native_input_lookup(1, 0).is_err());
    assert!(
        native
            .service
            .next_native_input_lookup(None)
            .unwrap()
            .is_none()
    );
    let write = native.write(&ticket);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("lower expiry required");
    };
    native.service.publish_terminal(&commit).unwrap();
    assert!(
        native
            .service
            .next_native_input_lookup(None)
            .unwrap()
            .is_none()
    );
}

/// One source with more than64 recipients requires multiple bounded operations.
/// An unselected recipient retains its genuine offered ACK rights while the
/// selected exact slots wait; later selection excludes that actual positive.
#[test]
fn expiry_batch_bound_preserves_unselected_fanout_and_independent_ack() {
    let mut scene = Scene::new(false, false);
    let mut targets = vec![scene.target.clone()];
    for _ in 1..70 {
        targets.push(scene.service.register_agent(None, None, "worker", vec![]));
    }
    scene.accept(Recipient::Role("worker".into()), Some(5));
    let row = scene.service.pending[&1].clone();
    let last = row.recipient_bindings[69].0.clone();
    scene.service.subscribe(&last).unwrap();
    assert_eq!(
        scene
            .service
            .receive_subscribed(&last, 11, 1)
            .unwrap()
            .messages
            .len(),
        1
    );
    let ticket = scene.reserve();
    let write = scene.write(&ticket);
    assert_eq!(write.binding_indices().count(), 64);
    assert!(
        scene
            .service
            .receive_subscribed(&last, 11, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(write.encoded_bytes() < 1024);
    let ack = scene
        .service
        .prepare_acknowledgement(&last, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let ack_write = scene
        .service
        .begin_storage_acknowledgement_write(&ack, &scene.scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(ack) =
        ack_write.persist(&mut Store::default())
    else {
        panic!("unselected real ACK required");
    };
    scene.service.publish_acknowledgement(&ack).unwrap();
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("first lower batch required");
    };
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 64);
    let ticket = scene
        .service
        .prepare_storage_expiry(&scene.scope, 11, usize::MAX)
        .unwrap()
        .unwrap();
    let write = scene.write(&ticket);
    assert_eq!(
        write.observed_at_ms(),
        16,
        "rollback must preserve the source's real first expiry observation"
    );
    assert_eq!(
        write.binding_indices().collect::<Vec<_>>(),
        vec![64, 65, 66, 67, 68]
    );
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("remaining lower batch required");
    };
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 5);
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 100)
            .unwrap()
            .len(),
        69
    );
    assert_eq!(scene.service.subscription(&last).unwrap().last_sequence, 1);
}

/// A committed cumulative ACK can record an expired source without ACKing any
/// of its fanout slots. After adoption removes the transient ACK reservation,
/// another original recipient still must not receive that source on rollback.
/// Later terminal selection reuses the actual first expiry witness instead of
/// accepting the rolled-back wall clock as a new lifetime.
#[test]
fn committed_ack_expiry_exclusion_survives_rollback_for_other_recipient() {
    let mut scene = Scene::new(true, false);
    let other = scene.service.register_agent(None, None, "worker", vec![]);
    scene.service.subscribe(&other.agent_id).unwrap();
    scene.accept(Recipient::Role("worker".into()), Some(5));
    scene.accept_named(
        "unexpired-ack-endpoint",
        Recipient::Agent(scene.target.agent_id.clone()),
        None,
    );
    let offered = scene
        .service
        .receive_subscribed(&scene.target.agent_id, 16, 10)
        .unwrap();
    assert_eq!(
        offered
            .messages
            .iter()
            .map(|m| m.sequence)
            .collect::<Vec<_>>(),
        vec![2]
    );
    drop(offered);
    let ack = scene
        .service
        .prepare_acknowledgement(&scene.target.agent_id, 2, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = scene
        .service
        .begin_storage_acknowledgement_write(&ack, &scene.scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut Store::default())
    else {
        panic!("actual lower ACK callback required");
    };
    scene.service.publish_acknowledgement(&commit).unwrap();
    assert!(
        scene
            .service
            .receive_subscribed(&other.agent_id, 11, 10)
            .unwrap()
            .messages
            .is_empty(),
        "a committed source expiry cannot become a fresh offer after rollback"
    );
    let ticket = scene
        .service
        .prepare_storage_expiry(&scene.scope, 11, usize::MAX)
        .unwrap()
        .unwrap();
    let write = scene.write(&ticket);
    assert_eq!(write.observed_at_ms(), 16);
    assert_eq!(write.binding_indices().count(), 2);
}

/// An externally retained real delivery cannot disappear from cache accounting
/// when the worker needs its full decoder quantum. Pre-issuance refusal leaves
/// the reservation cancellable; a genuine retry can issue after the pin retires.
/// Worker escrow retains that quantum until its final owner is actually dropped.
#[test]
fn expiry_decoder_capacity_refusal_preserves_unissued_owner_and_escrow_charge() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    let delivery = scene
        .service
        .receive_subscribed(&scene.target.agent_id, 11, 1)
        .unwrap();
    let observer = scene.service.payload_observer().unwrap();
    let ticket = scene.reserve();
    assert!(
        scene
            .service
            .begin_storage_terminal_write(&ticket, &scene.scope)
            .is_err()
    );
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    scene.service.cancel_storage_terminal(&ticket).unwrap();
    drop(ticket);
    drop(delivery);
    let baseline = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    let ticket = scene.reserve();
    let charge = ticket.metadata_bytes();
    let write = scene.write(&ticket);
    let escrow = write.attempt();
    assert_eq!(
        observer.snapshot().unwrap().reserved_payload_bytes,
        1024 * 1024
    );
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("actual lower expiry callback required");
    };
    scene.service.publish_terminal(&commit).unwrap();
    drop(ticket);
    drop(commit);
    assert_eq!(
        observer.snapshot().unwrap().reserved_payload_bytes,
        1024 * 1024
    );
    assert!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes
            >= baseline + charge
    );
    drop(escrow);
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes,
        baseline
    );
}

/// Idle expiry scheduling uses the strict original deadline, never a renewed
/// acceptance clock. Actual ACK protection suppresses a wake, cancellation
/// restores it, and committed terminal observations remove the eligible slot.
#[test]
fn expiry_deadline_tracks_strict_original_boundary_and_actual_slot_protection() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    assert_eq!(
        scene
            .service
            .next_storage_expiry_at_ms(&scene.scope)
            .unwrap(),
        Some(16)
    );
    scene.offer();
    let ack = scene
        .service
        .prepare_acknowledgement(&scene.target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    assert_eq!(
        scene
            .service
            .next_storage_expiry_at_ms(&scene.scope)
            .unwrap(),
        None
    );
    scene.service.cancel_acknowledgement(&ack).unwrap();
    assert_eq!(
        scene
            .service
            .next_storage_expiry_at_ms(&scene.scope)
            .unwrap(),
        Some(16)
    );
    let ticket = scene.reserve();
    let write = scene.write(&ticket);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("actual lower callback required");
    };
    scene.service.publish_terminal(&commit).unwrap();
    assert_eq!(
        scene
            .service
            .next_storage_expiry_at_ms(&scene.scope)
            .unwrap(),
        None
    );
    assert!(
        scene
            .service
            .next_storage_expiry_at_ms(&MessageStorageScope::default())
            .is_err()
    );
}

mod horizon;
mod retirement;
