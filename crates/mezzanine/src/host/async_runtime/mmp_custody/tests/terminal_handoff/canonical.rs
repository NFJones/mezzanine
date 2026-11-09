//! Actual canonical sender journal/receipt order and historical proof controls.

use super::*;

/// A real typed neutral row and h1 precede lower proof; h1 retires its image but
/// leaves the outcome pending. Historical lookup after native epoch replacement
/// and legitimate archive cleanup returns only old original proof, with no new
/// archive, current sender rights, task, provider or delivery ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_journals_original_fact_without_consuming_outcome() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    let before = Scene::phases(&fixture);
    let observer = scene.service.payload_observer().unwrap();
    let before_metadata = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    scene.start();
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes,
        before_metadata,
        "the first worker uses the already charged scratch reserve"
    );
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(Scene::phases(&fixture), (0, 1, before.2 + 2));
    let proof = scene
        .root
        .adopt_terminal_handoff(&scene.service)
        .unwrap()
        .unwrap();
    assert!(proof.record().archive_observed);
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert!(
        !scene
            .transcript
            .has_unsettled_append_receipts("native-sender")
            .unwrap()
    );
    let rows = scene.transcript.inspect("native-sender").unwrap();
    assert_eq!(rows.len(), 2);
    let Some(TranscriptContextEvent::TerminalHandoff { event }) =
        TranscriptContextEvent::from_transcript_content(&rows[1].content)
    else {
        panic!("typed original sender fact required");
    };
    assert!(
        event
            .source
            .matches_anchor(&scene.source, &proof.record().store_id)
    );
    let accounted =
        mez_agent::transcript::TranscriptContextRecord::from_transcript_content(&rows[1].content)
            .unwrap()
            .unwrap();
    assert_eq!(accounted.record(), scene.record);
    assert_eq!(event.event_sequence, 11);
    assert_eq!(rows[1].sequence, proof.record().archive_sequence);
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .len(),
        1
    );
    assert!(
        scene
            .service
            .acknowledge_delivery_outcome(&scene.sender.agent_id, &scene.source.outcome())
            .is_err()
    );
    // Actual cold inventory validates h1 against original source/t1, without
    // treating it as ACK, native input or a released terminal obligation.
    let io = scene
        .service
        .reserve_terminal_handoff_io(&scene.source)
        .unwrap();
    let store = MmpCustodyStore::open_existing(
        &fixture.path,
        "original-session",
        CustodyStoreLimits::default(),
    )
    .unwrap();
    let history = store.fold_custody_history(io.decode_limits()).unwrap();
    assert_eq!(history.terminal_observations.len(), 1);
    assert_eq!(
        history.terminal_observations[0].release_observed_at_ms,
        None
    );
    assert!(history.acknowledgements.is_empty());
    drop(store);
    drop(io);
    scene
        .service
        .bind_native_conversation(
            &scene.registration,
            "native-sender",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    scene.transcript.delete("native-sender").unwrap();
    let lookup = scene
        .service
        .capture_terminal_handoff_lookup(scene.source.sequence(), 0)
        .unwrap();
    assert!(!scene.service.terminal_handoff_owner_is_current(&lookup));
    scene
        .root
        .start_terminal_handoff(
            &mut scene.service,
            lookup.clone(),
            scene.transcript.clone(),
            None,
        )
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    let historical = scene
        .root
        .adopt_terminal_handoff(&scene.service)
        .unwrap()
        .unwrap();
    assert!(!historical.record().archive_observed);
    assert_eq!(historical.record().occurrence, proof.record().occurrence);
    assert_eq!(
        historical.record().canonical_sha256,
        proof.record().canonical_sha256
    );
    assert_eq!(Scene::phases(&fixture), (0, 1, before.2 + 2));
    assert!(!fixture.path.join("canonical/native-sender").exists());
    assert!(!lookup.terminal().released);
}

/// Qualified original absence changes no archive or generation and grants no
/// consumption rights. Lookup cannot be upgraded into fresh application work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_absence_is_not_fresh_projection_authority() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    let before = Scene::phases(&fixture);
    let lookup = scene
        .service
        .capture_terminal_handoff_lookup(scene.source.sequence(), 0)
        .unwrap();
    assert!(
        scene
            .root
            .start_terminal_handoff(
                &mut scene.service,
                lookup.clone(),
                scene.transcript.clone(),
                Some(scene.projection.clone())
            )
            .is_err()
    );
    scene
        .root
        .start_terminal_handoff(&mut scene.service, lookup, scene.transcript.clone(), None)
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(
        scene
            .root
            .adopt_terminal_handoff(&scene.service)
            .unwrap()
            .is_none()
    );
    assert_eq!(Scene::phases(&fixture), before);
    assert_eq!(scene.transcript.inspect("native-sender").unwrap().len(), 1);
}

/// An unrelated accepted delivery pins the entire one-MiB cache while the old
/// original is cold and optional snapshots exhaust ordinary metadata space.
/// Actual canonical handoff and its
/// separate consumed transaction must still settle that old terminal source,
/// retaining the pin and publishing exactly one neutral sender fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_handoff_and_consumption_progress_with_entire_cache_pinned() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    let sequence = scene.source.sequence();
    let target = scene.source.recipient().clone();
    scene.service.subscribe(&target).unwrap();
    scene
        .service
        .release_stored_payload(sequence, scene.root.storage_scope())
        .unwrap();
    let mut original = envelope(&scene.sender, &scene.sender, "independent-cache-pin");
    original.recipient = mez_agent::messaging::Recipient::Agent(target.clone());
    original.payload = "x".repeat(1024 * 1024);
    original.ttl_ms = None;
    let ticket = scene
        .service
        .prepare_acceptance(
            &scene.sender.agent_id,
            original,
            MessageScope::Session,
            20,
            4096,
            None,
        )
        .unwrap();
    scene.root.start(&mut scene.service, &ticket, None).unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    scene.root.adopt(&mut scene.service, None).unwrap();
    drop(ticket);
    let delivery = scene.service.receive_subscribed(&target, 21, 1).unwrap();
    assert_eq!(delivery.messages.len(), 1);
    let observer = scene.service.payload_observer().unwrap();
    let pinned = observer.snapshot().unwrap();
    assert_eq!(pinned.live_payload_bytes, 1024 * 1024);
    assert_eq!(pinned.reserved_payload_bytes, 0);
    let before = Scene::phases(&fixture);
    let captures =
        actor::hold_snapshot_metadata_until_capacity(&scene.service, scene.root.storage_scope());
    let pressured = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    scene.start();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    let proof = scene
        .root
        .adopt_terminal_handoff(&scene.service)
        .unwrap()
        .unwrap();
    assert!(proof.record().archive_observed);
    assert_eq!(Scene::phases(&fixture), (0, 1, before.2 + 2));
    assert_eq!(observer.snapshot().unwrap(), pinned);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes,
        pressured
    );
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
        .unwrap();
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(observer.snapshot().unwrap(), pinned);
    assert_eq!(delivery.messages[0].envelope.payload.len(), 1024 * 1024);
    assert_eq!(scene.transcript.inspect("native-sender").unwrap().len(), 2);
    assert!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes
            <= 8 * 1024 * 1024
    );
    drop(captures);
}
