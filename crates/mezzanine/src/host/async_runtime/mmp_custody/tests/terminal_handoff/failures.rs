//! Actual handoff worker join/loss and original physical publication boundaries.

use super::*;
use sha2::Digest;

/// Assertion unwind cannot strand a real blocking handoff worker.
struct Gate(Arc<Probe>);
impl Drop for Gate {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Cancelling an observer leaves its actual worker, source/decoder reservation
/// and kernel fence owned by the root. Other executor work remains runnable;
/// Stop still drains the issued canonical job and preserves its exact proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_observer_cancellation_retains_actual_worker_and_source() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(false);
    let _gate = Gate(probe.clone());
    let mut scene = Scene::new(&fixture, probe.clone()).await;
    scene.start();
    probe.started.notified().await;
    let observer = {
        let root = scene.root.clone();
        tokio::spawn(async move { root.observe().await })
    };
    tokio::task::yield_now().await;
    observer.abort();
    let _ = observer.await;
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert!(
        scene
            .service
            .payload_observer()
            .unwrap()
            .snapshot()
            .unwrap()
            .reserved_payload_bytes
            > 0
    );
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(scene.root.storage_scope(), 600016, usize::MAX)
            .unwrap()
            .is_none()
    );
    assert_eq!(tokio::spawn(async { 7 }).await.unwrap(), 7);
    scene.root.request_stop();
    probe.release();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(
        scene
            .root
            .adopt_terminal_handoff(&scene.service)
            .unwrap()
            .unwrap()
            .record()
            .archive_observed
    );
}

/// A panic after actual h1 and receipt retirement cannot discard the positive
/// or its real archive observation. Retry after observed join reuses that exact
/// proof, without another journal generation or later canonical placement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_lost_positive_preserves_original_proof_through_stop() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(true);
    let mut scene = Scene::new(&fixture, probe.clone()).await;
    probe.panic_after_commit.store(true, Ordering::SeqCst);
    let before = Scene::phases(&fixture);
    scene.start();
    let CustodyWorkerStatus::TerminalHandoffRetryAt(due) = scene.root.observe().await.unwrap()
    else {
        panic!("actual joined loss required");
    };
    assert_eq!(Scene::phases(&fixture), (0, 1, before.2 + 2));
    scene.root.request_stop();
    assert!(
        !scene
            .root
            .retry_terminal_handoff(due - Duration::from_millis(1))
            .unwrap()
    );
    assert!(scene.root.retry_terminal_handoff(due).unwrap());
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
    assert_eq!(scene.transcript.inspect("native-sender").unwrap().len(), 2);
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .len(),
        1
    );
}

/// The first physical lease/plan/image survives failure before u1, preserving
/// its original final position and source decoder reservation through backoff.
/// Completed stages are reconciled from actual bytes rather than replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_stage_failure_keeps_first_plan_and_decoder_owner() {
    for fault in [
        CustodyReceiptPublicationFault::BeforeStage,
        CustodyReceiptPublicationFault::Partial,
        CustodyReceiptPublicationFault::Complete,
        CustodyReceiptPublicationFault::Linked,
    ] {
        let fixture = Fixture::new(true);
        let mut scene = Scene::new(&fixture, Probe::new(true)).await;
        scene
            .transcript
            .fail_custody_receipt_publication_for_tests(fault);
        let before = Scene::phases(&fixture);
        let observer = scene.service.payload_observer().unwrap();
        scene.start();
        let CustodyWorkerStatus::TerminalHandoffRetryAt(due) = scene.root.observe().await.unwrap()
        else {
            panic!("actual publication failure required");
        };
        assert_eq!(Scene::phases(&fixture), before);
        assert!(observer.snapshot().unwrap().reserved_payload_bytes > 0);
        assert!(scene.root.retry_terminal_handoff(due).unwrap());
        assert_eq!(
            scene.root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
        );
        let proof = scene
            .root
            .adopt_terminal_handoff(&scene.service)
            .unwrap()
            .unwrap();
        assert_eq!(proof.record().archive_sequence, 2);
        assert_eq!(Scene::phases(&fixture), (0, 1, before.2 + 2));
        assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
        assert_eq!(scene.transcript.inspect("native-sender").unwrap().len(), 2);
    }
}

/// Trigger damage during either real lifecycle transaction must roll back all
/// non-target effects and retain the same first plan. Removing the actual fault
/// permits exact retry, without reseating the archive or inventing consumption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_sql_damage_rolls_back_and_retries_original_transaction() {
    for phase in ["u1", "h1"] {
        let fixture = Fixture::new(true);
        let mut scene = Scene::new(&fixture, Probe::new(true)).await;
        let before = Scene::phases(&fixture);
        fixture.connection().execute_batch(&format!("CREATE TRIGGER damage_handoff AFTER INSERT ON records WHEN NEW.key LIKE '{phase}:%' BEGIN DELETE FROM records WHERE key LIKE 't1:%'; END;")).unwrap();
        scene.start();
        let CustodyWorkerStatus::TerminalHandoffRetryAt(due) = scene.root.observe().await.unwrap()
        else {
            panic!("actual rollback required");
        };
        let expected = if phase == "u1" {
            before
        } else {
            (1, 0, before.2 + 1)
        };
        assert_eq!(Scene::phases(&fixture), expected);
        let terminals: i64 = fixture
            .connection()
            .query_row(
                "SELECT count(*) FROM records WHERE key LIKE 't1:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(terminals, 1);
        fixture
            .connection()
            .execute_batch("DROP TRIGGER damage_handoff")
            .unwrap();
        assert!(scene.root.retry_terminal_handoff(due).unwrap());
        assert_eq!(
            scene.root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
        );
        let proof = scene
            .root
            .adopt_terminal_handoff(&scene.service)
            .unwrap()
            .unwrap();
        assert_eq!(proof.record().archive_sequence, 2);
        assert_eq!(Scene::phases(&fixture), (0, 1, before.2 + 2));
        assert_eq!(scene.transcript.inspect("native-sender").unwrap().len(), 2);
    }
}

/// Recomputed record checksums do not qualify altered journal framing, original
/// before-images, allocation or native sender target. Each exact restored image
/// can reconcile through the retained original root without another append.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_rejects_tampered_journal_and_recovers_original_bytes() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    scene.start();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    scene
        .root
        .adopt_terminal_handoff(&scene.service)
        .unwrap()
        .unwrap();
    let (key, bytes): (String, Vec<u8>) = fixture
        .connection()
        .query_row(
            "SELECT key,value FROM records WHERE key LIKE 'h1:%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let before = Scene::phases(&fixture);
    for variant in 0..5 {
        let mut changed = original.clone();
        match variant {
            0 => changed["version"] = serde_json::json!(2),
            1 => changed["source"]["store_id"] = serde_json::to_value([9u8; 16]).unwrap(),
            2 => changed["source"]["observed_at_ms"] = serde_json::json!(17),
            3 => changed["source"]["sequence"] = serde_json::json!(0),
            _ => changed["transcript"]["conversation_id"] = serde_json::json!("replacement-target"),
        }
        let changed = serde_json::to_vec(&changed).unwrap();
        let digest: [u8; 32] = sha2::Sha256::digest(&changed).into();
        fixture
            .connection()
            .execute(
                "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                rusqlite::params![changed, digest.as_slice(), &key],
            )
            .unwrap();
        let lookup = scene
            .service
            .capture_terminal_handoff_lookup(scene.source.sequence(), 0)
            .unwrap();
        scene
            .root
            .start_terminal_handoff(&mut scene.service, lookup, scene.transcript.clone(), None)
            .unwrap();
        let CustodyWorkerStatus::TerminalHandoffRetryAt(due) = scene.root.observe().await.unwrap()
        else {
            panic!("tampered journal must refuse variant {variant}");
        };
        let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
        fixture
            .connection()
            .execute(
                "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                rusqlite::params![&bytes, digest.as_slice(), &key],
            )
            .unwrap();
        assert!(scene.root.retry_terminal_handoff(due).unwrap());
        assert_eq!(
            scene.root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
        );
        assert!(
            !scene
                .root
                .adopt_terminal_handoff(&scene.service)
                .unwrap()
                .unwrap()
                .record()
                .archive_observed
        );
        assert_eq!(Scene::phases(&fixture), before);
        assert_eq!(scene.transcript.inspect("native-sender").unwrap().len(), 2);
    }
}
