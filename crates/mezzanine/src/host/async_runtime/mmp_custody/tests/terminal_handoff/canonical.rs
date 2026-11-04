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
    scene.start();
    assert!(observer.snapshot().unwrap().reserved_payload_bytes > 0);
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
