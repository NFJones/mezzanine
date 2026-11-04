//! Actual lower source/end witnesses, exclusion, framing and retired horizons.

use super::*;

impl MessageNativeContinuityStore for Store {
    /// Trusted lower test port; concrete journal qualification lives in product.
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> Result<()> {
        write.snapshot().validate()
    }
}

/// Trusted lookup fixture; descriptive records alone cannot construct a witness.
struct Observer {
    end: Option<MessageNativeRetirementRecord>,
    canonical: Option<MessageNativeInputRecord>,
    end_calls: usize,
}
impl MessageNativeInputStore for Observer {
    /// Supplies only this fixture's explicit canonical observation.
    fn verify(&mut self, _: &MessageNativeInputAnchor) -> Result<Option<MessageNativeInputRecord>> {
        Ok(self.canonical.clone())
    }
    /// Records that end qualification followed absence rather than a positive.
    fn retired_owner(
        &mut self,
        _: &MessageNativeInputAnchor,
    ) -> Result<Option<MessageNativeRetirementRecord>> {
        self.end_calls += 1;
        Ok(self.end.clone())
    }
}

/// Actual guarded native tuple and accepted TTL-free original precede removal.
fn initial() -> (Scene, MessageNativeCheckpoint, MessageNativeIncarnation) {
    let mut scene = Scene::new(true, true);
    let epoch = scene.service.native_bindings[&scene.target.agent_id].continuity_incarnation();
    let request = scene
        .service
        .prepare_native_continuity(&scene.scope, &scene.target.agent_id)
        .unwrap();
    let native = request.write().snapshot().native.clone();
    let verified = request.write().persist(&mut Store::default()).unwrap();
    request.finish(&mut scene.service, verified).unwrap();
    scene.accept(Recipient::Agent(scene.target.agent_id.clone()), None);
    (scene, native, epoch)
}

/// Captures the genuine actual end; a trusted test observation supplies its
/// concrete-store observations, which product tests separately prove with I/O.
fn ended() -> (Scene, MessageNativeRetirementRecord) {
    let (mut scene, mut native, _) = initial();
    scene.service.retire_agent_identity(&scene.target.agent_id);
    let request = scene
        .service
        .prepare_registration_end(&scene.scope)
        .unwrap()
        .unwrap();
    native.admission_available = false;
    native.delivery_available = false;
    let record = MessageNativeRetirementRecord {
        store_id: [1; 16],
        native,
        native_publication: 1,
        end: request.write().snapshot().clone(),
        end_publication: 3,
        lifetime_sha256: [2; 32],
        end_sha256: [3; 32],
    };
    (scene, record)
}

/// Uses the actual accepted source and trusted lookup port to obtain a witness.
fn witness(scene: &Scene, record: MessageNativeRetirementRecord) -> MessageNativeRetirementWitness {
    let anchor = scene.service.capture_native_input_lookup(1, 0).unwrap();
    let mut port = Observer {
        end: Some(record),
        canonical: None,
        end_calls: 0,
    };
    let MessageNativeInputObservation::RecipientEnded(witness) = anchor.observe(&mut port).unwrap()
    else {
        panic!("actual source/end qualification required");
    };
    assert_eq!(port.end_calls, 1);
    witness
}

/// A qualified end only reserves a disposition; the original write and positive
/// adoption are required before an outcome exists. TTL-free mail becomes retired
/// without ACK, cursor advancement or a renewed expiry clock.
#[test]
fn native_retirement_requires_exact_write_and_adoption_without_ack() {
    let (mut scene, record) = ended();
    let witness = witness(&scene, record);
    let ticket = scene
        .service
        .prepare_storage_native_retirement(&scene.scope, &witness, 2, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(ticket.kind(), MessageTerminalKind::RecipientRetirement);
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    let write = scene.write(&ticket);
    assert_eq!(write.encoded_bytes(), 144);
    assert_eq!(write.ttl_ms(), None);
    assert_eq!(write.observed_at_ms(), 10);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("original positive required");
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
    let outcomes = scene
        .service
        .delivery_outcomes_for(&scene.sender.agent_id, 10)
        .unwrap();
    assert_eq!(outcomes[0].reason, DeliveryTerminalReason::RecipientRetired);
    assert_eq!(outcomes[0].observed_at_ms, 10);
    assert_eq!(
        *scene.service.pending[&1].acknowledged.lock().unwrap(),
        vec![false]
    );
    assert!(
        !scene
            .service
            .native_input_anchor_is_unsettled(witness.anchor())
            .unwrap()
    );
}

/// An earlier genuine offered application anchor cannot be bypassed by a later
/// end lookup. Dropping that actual owner permits reservation; no presence or
/// generation test can replace this original slot protection.
#[test]
fn native_retirement_waits_for_preexisting_application_anchor() {
    let (mut scene, mut native, _) = initial();
    scene.offer();
    let application = scene
        .service
        .capture_native_input_anchor(&scene.target.agent_id, 1)
        .unwrap();
    scene.service.retire_agent_identity(&scene.target.agent_id);
    let end = scene
        .service
        .prepare_registration_end(&scene.scope)
        .unwrap()
        .unwrap();
    native.admission_available = false;
    native.delivery_available = false;
    let witness = witness(
        &scene,
        MessageNativeRetirementRecord {
            store_id: [1; 16],
            native,
            native_publication: 1,
            end: end.write().snapshot().clone(),
            end_publication: 3,
            lifetime_sha256: [2; 32],
            end_sha256: [3; 32],
        },
    );
    assert!(
        scene
            .service
            .prepare_storage_native_retirement(&scene.scope, &witness, 100, usize::MAX)
            .unwrap()
            .is_none()
    );
    assert!(!scene.service.has_unpublished_terminal());
    drop(application);
    assert!(
        scene
            .service
            .prepare_storage_native_retirement(&scene.scope, &witness, 100, usize::MAX)
            .unwrap()
            .is_some()
    );
}

/// Canonical proof takes precedence even if a port could also supply end data.
/// The end hook must not run, and no retired witness or terminal is produced.
#[test]
fn native_retirement_never_replaces_actual_canonical_positive() {
    let (scene, record) = ended();
    let mut port = Observer {
        end: Some(record),
        canonical: Some(MessageNativeInputRecord {
            store_id: [1; 16],
            occurrence: [2; 32],
            archive_sequence: 1,
            canonical_sha256: [3; 32],
            offered_at_ms: 11,
            archive_observed: false,
        }),
        end_calls: 0,
    };
    let anchor = scene.service.capture_native_input_lookup(1, 0).unwrap();
    assert!(matches!(
        anchor.observe(&mut port).unwrap(),
        MessageNativeInputObservation::Committed(_)
    ));
    assert_eq!(port.end_calls, 0);
    assert!(!scene.service.has_unpublished_terminal());
}

/// Reframed data must agree with the frozen source, native tuple, interval,
/// cutoff and strictly ordered publications. Validation cannot create a live
/// owner or mutate a slot; malformed trusted-port data refuses before selection.
#[test]
fn native_retirement_refuses_mismatched_descriptive_end_observations() {
    for fault in [
        "store",
        "agent",
        "floor",
        "cutoff",
        "publication",
        "availability",
        "lifetime-hash",
        "end-hash",
    ] {
        let (scene, mut record) = ended();
        match fault {
            "store" => record.store_id = [0; 16],
            "agent" => record.end.agent_id = "other".into(),
            "floor" => record.end.registered_after_sequence = 1,
            "cutoff" => record.end.ended_after_sequence = 0,
            "publication" => record.end_publication = record.native_publication,
            "availability" => record.native.admission_available = true,
            "lifetime-hash" => record.lifetime_sha256 = [0; 32],
            "end-hash" => record.end_sha256 = [0; 32],
            _ => unreachable!(),
        }
        let anchor = scene.service.capture_native_input_lookup(1, 0).unwrap();
        let mut port = Observer {
            end: Some(record),
            canonical: None,
            end_calls: 0,
        };
        assert!(anchor.observe(&mut port).is_err(), "{fault}");
        assert!(!scene.service.has_unpublished_terminal());
        assert_eq!(scene.service.pending.len(), 1);
    }
}

/// Unknown/lost write results preserve the exact original slot, proof and first
/// observation. Retry cannot resample a new clock or free the original source.
#[test]
fn native_retirement_unknown_write_retries_exact_first_frame() {
    let (mut scene, record) = ended();
    let witness = witness(&scene, record);
    let ticket = scene
        .service
        .prepare_storage_native_retirement(&scene.scope, &witness, 100, usize::MAX)
        .unwrap()
        .unwrap();
    let mut port = Store {
        original: None,
        fail: true,
    };
    let MessageTerminalWriteOutcome::Unknown(unknown) = scene.write(&ticket).persist(&mut port)
    else {
        panic!("actual uncertainty required");
    };
    assert!(scene.service.pending[&1].terminal.lock().unwrap()[0].is_none());
    port.fail = false;
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    assert_eq!(write.observed_at_ms(), 100);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut port) else {
        panic!("original retry must qualify");
    };
    scene.service.publish_terminal(&commit).unwrap();
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()[0]
            .observed_at_ms,
        100
    );
}

/// Real retired outcomes retain the same finite horizon and release semantics
/// as expiry. Their distinct reason remains encoded in the exact before-image;
/// neither source release nor an ACK is inferred before its own positive.
#[test]
fn native_retirement_horizon_preserves_retired_reason_and_exact_release() {
    let (mut scene, record) = ended();
    let witness = witness(&scene, record);
    let ticket = scene
        .service
        .prepare_storage_native_retirement(&scene.scope, &witness, 100, usize::MAX)
        .unwrap()
        .unwrap();
    let MessageTerminalWriteOutcome::Committed(commit) =
        scene.write(&ticket).persist(&mut Store::default())
    else {
        panic!("original retirement positive required");
    };
    scene.service.publish_terminal(&commit).unwrap();
    drop((ticket, commit, witness));
    assert_eq!(
        scene
            .service
            .next_storage_horizon_at_ms(&scene.scope)
            .unwrap(),
        Some(600100)
    );
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600099, usize::MAX)
            .unwrap()
            .is_none()
    );
    let ticket = scene
        .service
        .prepare_storage_horizon_release(&scene.scope, 600100, usize::MAX)
        .unwrap()
        .unwrap();
    let write = scene.write(&ticket);
    assert_eq!(
        write.terminal_entries().next().unwrap().1.reason,
        DeliveryTerminalReason::RecipientRetired
    );
    assert_eq!(scene.service.pending.len(), 1);
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("original release positive required");
    };
    scene.service.publish_terminal(&commit).unwrap();
    assert!(scene.service.pending.is_empty());
}

/// Temporary shell displacement and exact rollback create no namespace end.
/// Canonical absence alone stays an absence; neither path retires TTL-free mail.
#[test]
fn native_retirement_does_not_infer_end_from_displacement_or_lookup_absence() {
    let (mut scene, _, parent) = initial();
    let registration = scene
        .service
        .capture_sender_registration(&scene.target.agent_id)
        .unwrap();
    for epoch in [MessageNativeIncarnation::default(), parent] {
        scene
            .service
            .bind_native_conversation(&registration, "original", &epoch)
            .unwrap();
        let anchor = scene.service.capture_native_input_lookup(1, 0).unwrap();
        let mut port = Observer {
            end: None,
            canonical: None,
            end_calls: 0,
        };
        assert!(matches!(
            anchor.observe(&mut port).unwrap(),
            MessageNativeInputObservation::Absent
        ));
        assert!(!scene.service.has_pending_registration_end().unwrap());
        assert!(!scene.service.has_unpublished_terminal());
        assert_eq!(scene.service.pending.len(), 1);
    }
}

/// Repeated observations through clones retain one bounded charged record.
/// Canonical lookup still precedes every read, and a changed immutable end
/// image cannot replace the first trusted observation or enlarge retained state.
#[test]
fn native_retirement_requalification_shares_first_record_and_refuses_replacement() {
    let (scene, record) = ended();
    let source = scene.service.capture_native_input_lookup(1, 0).unwrap();
    let mut port = Observer {
        end: Some(record),
        canonical: None,
        end_calls: 0,
    };
    let MessageNativeInputObservation::RecipientEnded(first) =
        source.clone().observe(&mut port).unwrap()
    else {
        panic!("actual ended original required");
    };
    let MessageNativeInputObservation::RecipientEnded(second) =
        source.clone().observe(&mut port).unwrap()
    else {
        panic!("actual ended original required");
    };
    assert!(std::ptr::eq(first.record(), second.record()));
    assert_eq!(port.end_calls, 2);
    port.end.as_mut().unwrap().end_sha256 = [9; 32];
    assert!(source.observe(&mut port).is_err());
    assert_eq!(first.record(), second.record());
}
