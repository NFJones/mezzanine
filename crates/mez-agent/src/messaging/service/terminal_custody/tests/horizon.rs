//! Exact lower horizon release, using fixture callbacks rather than physical I/O.

use super::*;

/// Publishes genuine lower expiry facts; no released bit is manufactured.
fn observe(scene: &mut Scene) {
    while let Some(ticket) = scene
        .service
        .prepare_storage_expiry(&scene.scope, 16, usize::MAX)
        .unwrap()
    {
        let write = scene.write(&ticket);
        let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
        else {
            panic!("fixture expiry callback required");
        };
        scene.service.publish_terminal(&commit).unwrap();
    }
}
fn release(scene: &mut Scene, now: u64) -> MessageTerminalTicket {
    scene
        .service
        .prepare_storage_horizon_release(&scene.scope, now, usize::MAX)
        .unwrap()
        .unwrap()
}

/// Horizon selection is exact at first observation plus600000ms. Real replay
/// eviction cannot drop pending source, while actual release can retire only
/// that source after adoption. Its last proof still retains/charges metadata.
#[test]
fn horizon_requires_exact_positive_before_releasing_source_and_last_proof() {
    let mut scene = Scene::new(true, false);
    scene.service.retention_messages = 1;
    scene.direct();
    observe(&mut scene);
    scene.accept_named(
        "independent-original",
        Recipient::Agent(scene.target.agent_id.clone()),
        None,
    );
    assert_eq!(scene.service.queue.len(), 1);
    let weak = Arc::downgrade(&scene.service.pending[&1]);
    assert_eq!(
        scene
            .service
            .next_storage_horizon_at_ms(&scene.scope)
            .unwrap(),
        Some(600016)
    );
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600015, usize::MAX)
            .unwrap()
            .is_none()
    );
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 11, usize::MAX)
            .unwrap()
            .is_none()
    );
    // The independent pending original retains its shared raw end intent after
    // this release's final proof disappears. Only the ticket's delta may retire.
    let source_end_charge = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .checkpoint_bytes
        .load(Ordering::Acquire);
    assert!(source_end_charge > 0);
    let ticket = release(&mut scene, 600016);
    let charge = ticket.metadata_bytes();
    let write = scene.write(&ticket);
    assert_eq!(write.kind(), MessageTerminalKind::HorizonRelease);
    assert_eq!(
        write.terminal_entries().collect::<Vec<_>>(),
        vec![(
            0,
            MessageDeliveryTerminalSnapshot {
                reason: DeliveryTerminalReason::Expired,
                observed_at_ms: 16,
                released: false
            }
        )]
    );
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("fixture release callback required");
    };
    assert_eq!(scene.service.pending.len(), 2);
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 0);
    assert_eq!(scene.service.pending.len(), 1);
    assert!(scene.service.pending.contains_key(&2));
    assert!(!scene.service.queued_by_sequence.contains_key(&1));
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .checkpoint_bytes
            .load(Ordering::Acquire),
        source_end_charge
            + charge.saturating_sub(super::super::super::storage_policy::WORKER_METADATA_QUANTUM)
    );
    let loan = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .worker_metadata_loan
        .clone();
    assert!(loan.load(Ordering::Acquire));
    drop(ticket);
    assert!(weak.upgrade().is_some());
    drop(commit);
    assert!(weak.upgrade().is_none());
    assert!(!loan.load(Ordering::Acquire));
    assert_eq!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .checkpoint_bytes
            .load(Ordering::Acquire),
        source_end_charge
    );
}

/// Actual lower unknown input retains the same exact first release clock and
/// before-images. A new nonce after retry cannot authorize a stale attempt or a
/// foreign service, and no uncertainty releases the original sender's outcome.
#[test]
fn horizon_unknown_retries_immutable_before_images_and_original_clock() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    observe(&mut scene);
    let ticket = release(&mut scene, 600016);
    let write = scene.write(&ticket);
    let original = write.attempt();
    let mut store = Store {
        fail: true,
        ..Default::default()
    };
    let MessageTerminalWriteOutcome::Unknown(unknown) = write.persist(&mut store) else {
        panic!("fixture error required");
    };
    assert!(scene.service.cancel_storage_terminal(&ticket).is_err());
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .len(),
        1
    );
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    assert_eq!(write.observed_at_ms(), 600016);
    assert!(!write.attempt().same_attempt(&original));
    assert!(scene.service.validate_terminal_attempt(&original).is_err());
    store.fail = false;
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut store) else {
        panic!("same release callback required");
    };
    let mut foreign = Scene::new(true, false);
    assert!(foreign.service.publish_terminal(&commit).is_err());
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
}

/// Bounded release batches share the source's actual first horizon observation.
/// After the first64 slots release, rollback cannot indefinitely strand the six
/// remaining slots or renew their first expiry times. No ACK is fabricated.
#[test]
fn horizon_batches_preserve_observed_eligibility_across_clock_rollback() {
    let mut scene = Scene::new(true, false);
    for _ in 1..70 {
        scene.service.register_agent(None, None, "worker", vec![]);
    }
    scene.accept(Recipient::Role("worker".into()), Some(5));
    observe(&mut scene);
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 100)
            .unwrap()
            .len(),
        70
    );
    let ticket = release(&mut scene, 600016);
    let write = scene.write(&ticket);
    assert_eq!(write.terminal_entries().count(), 64);
    assert!(write.encoded_bytes() + 24 <= 916);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("first release required");
    };
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 64);
    drop(ticket);
    drop(commit);
    assert!(scene.service.pending.contains_key(&1));
    assert_eq!(
        scene
            .service
            .next_storage_horizon_at_ms(&scene.scope)
            .unwrap(),
        Some(0)
    );
    let ticket = release(&mut scene, 11);
    let write = scene.write(&ticket);
    assert_eq!(write.observed_at_ms(), 600016);
    assert_eq!(write.terminal_entries().count(), 6);
    assert!(
        write
            .terminal_entries()
            .all(|(_, value)| value.observed_at_ms == 16)
    );
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("remaining release required");
    };
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 6);
    assert!(scene.service.pending.is_empty());
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// Never-issued ownership stays cancellable. Foreign scope/budget failures do
/// not release any original. A finite deadline beyond u64 cannot become an
/// immediate horizon through arithmetic overflow or a descriptive receipt.
#[test]
fn horizon_preflight_and_cancel_preserve_actual_terminal_before_images() {
    let mut scene = Scene::new(true, false);
    scene.direct();
    observe(&mut scene);
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&MessageStorageScope::default(), u64::MAX, usize::MAX)
            .is_err()
    );
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600016, 1)
            .is_err()
    );
    assert!(!scene.service.has_unpublished_terminal());
    let ticket = release(&mut scene, 600016);
    scene.service.cancel_storage_terminal(&ticket).unwrap();
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        scene
            .service
            .next_storage_horizon_at_ms(&MessageStorageScope::default())
            .is_err()
    );
    let mut other = Scene::new(true, false);
    other.direct();
    let ticket = other
        .service
        .prepare_storage_expiry(&other.scope, u64::MAX, usize::MAX)
        .unwrap()
        .unwrap();
    let write = other.write(&ticket);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("extreme observation required");
    };
    other.service.publish_terminal(&commit).unwrap();
    assert_eq!(
        other
            .service
            .next_storage_horizon_at_ms(&other.scope)
            .unwrap(),
        None
    );
    assert!(
        other
            .service
            .prepare_storage_horizon_release(&other.scope, u64::MAX, usize::MAX)
            .unwrap()
            .is_none()
    );
}
