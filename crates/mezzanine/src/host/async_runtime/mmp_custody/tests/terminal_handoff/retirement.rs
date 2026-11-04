//! Common H1/S1 custody accepts genuine recipient retirement without ACKs.

use super::*;
use mez_agent::messaging::DeliveryTerminalReason;

/// Actual d1 has the same canonical sender handoff and distinct s1 boundary
/// as expiry. Cold folding preserves its reason instead of reinterpreting it
/// as TTL expiry or successful native input, and consumption alone releases it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_sender_handoff_and_consumption_preserve_original_reason() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::retired(&fixture, Probe::new(true)).await;
    assert_eq!(
        scene.source.terminal().reason,
        DeliveryTerminalReason::RecipientRetired
    );
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
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
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
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        0
    );
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let history = store
        .fold_custody_history(mez_agent::messaging::MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        })
        .unwrap();
    assert!(history.acknowledgements.is_empty());
    assert_eq!(history.terminal_observations.len(), 1);
    let terminal = &history.terminal_observations[0];
    assert_eq!(terminal.reason, DeliveryTerminalReason::RecipientRetired);
    assert_eq!(terminal.observed_at_ms, 16);
    assert!(terminal.sender_consumed);
    assert_eq!(terminal.release_observed_at_ms, None);
}
