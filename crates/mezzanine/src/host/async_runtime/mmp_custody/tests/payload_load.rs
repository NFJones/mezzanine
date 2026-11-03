//! Real SQLite cold-source reloads through the owned worker and actual joins.
//!
//! Cache release uses the committed original root qualification; every load
//! checks the real source chunks and original fingerprint. No fixture restores
//! authority from a snapshot, fabricates a loaded payload or acknowledges mail.

use super::*;

/// Commits one actual original and consumes the physical-start notification so
/// subsequent gates identify the read worker, rather than the earlier writer.
async fn accepted(
    fixture: &Fixture,
) -> (
    MmpCustodyRoot,
    MessageService,
    SenderIdentity,
    SenderIdentity,
    u64,
) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, sender, target, _) = fixture_service();
    service.subscribe(&target.agent_id).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "cold-original"),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(&mut service, None).unwrap();
    (root, service, sender, target, ticket.sequence())
}

/// An accepted source becomes genuinely unavailable after its cache pin drops,
/// then returns from verified SQLite bytes through actual worker/join/adoption.
/// Reload preserves exact source/receipt/cursor state and original TTL; neither
/// the read nor publication changes disk generation, sequence or delivery status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_stored_payload_reloads_without_reaccepting_or_renewing_ttl() {
    let fixture = Fixture::new(true);
    let (root, mut service, sender, target, sequence) = accepted(&fixture).await;
    let before = service.snapshot_state().unwrap();
    let disk = fixture.disk();
    service
        .release_stored_payload(sequence, root.storage_scope())
        .unwrap();
    assert!(service.snapshot_state().is_err());
    assert!(
        service
            .receive_subscribed(&target.agent_id, 120, 1)
            .is_err()
    );
    assert!(root.start_payload_load(&mut service, sequence).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::PayloadLoadAwaitingAdoption
    );
    assert!(
        service.snapshot_state().is_err(),
        "actual join alone cannot publish bytes"
    );
    assert!(root.publish_payload_load(&mut service).unwrap());
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(service.snapshot_state().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
    let batch = service
        .receive_subscribed(&target.agent_id, 120, 1)
        .unwrap();
    assert_eq!(batch.messages[0].sequence, sequence);
    assert_eq!(
        *batch.messages[0].envelope,
        envelope(&sender, &target, "cold-original")
    );
    assert!(!root.start_payload_load(&mut service, sequence).unwrap());
    assert!(
        service
            .receive_subscribed(&target.agent_id, 1335, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// Aborting an observer leaves the actual gated read owned, including its lane
/// and eventual decoded allocation. Another send cannot issue through that lane;
/// unrelated async work remains responsive and Stop preserves accepted drains.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payload_read_observer_abort_preserves_worker_lane_and_publication() {
    let fixture = Fixture::new(true);
    let (root, mut service, sender, target, sequence) = accepted(&fixture).await;
    service
        .release_stored_payload(sequence, root.storage_scope())
        .unwrap();
    *fixture.probe.gate.0.lock().unwrap() = false;
    assert!(root.start_payload_load(&mut service, sequence).unwrap());
    fixture.probe.started.notified().await;
    let observer_root = root.clone();
    let observer = tokio::spawn(async move { observer_root.observe().await });
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "unissued"),
            MessageScope::Session,
            101,
            4096,
            None,
        )
        .unwrap();
    assert!(root.start(&mut service, &ticket, None).is_err());
    service.cancel_acceptance(&ticket).unwrap();
    assert!(root.start_payload_load(&mut service, sequence).is_err());
    assert_eq!(tokio::spawn(async { 17 }).await.unwrap(), 17);
    root.request_stop();
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::PayloadLoadAwaitingAdoption
    );
    assert!(root.publish_payload_load(&mut service).unwrap());
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 120, 1)
            .unwrap()
            .messages[0]
            .sequence,
        sequence
    );
}

/// Lost actual read output cannot release the original nonce or start a second
/// live worker. A joined failure backs off, rejects foreign-owner recovery and
/// reloads the same source after the deadline without changing stored evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_lost_payload_read_retries_original_after_join_and_backoff() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, _, sequence) = accepted(&fixture).await;
    let before = service.snapshot_state().unwrap();
    let disk = fixture.disk();
    service
        .release_stored_payload(sequence, root.storage_scope())
        .unwrap();
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_payload_load(&mut service, sequence).unwrap();
    let CustodyWorkerStatus::PayloadLoadRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual failed read join required")
    };
    assert!(root.failure().unwrap().is_some());
    assert!(
        !root
            .retry_payload_load(&mut service, due - Duration::from_nanos(1))
            .unwrap()
    );
    assert!(
        root.retry_payload_load(&mut MessageService::default(), due)
            .is_err()
    );
    assert!(service.snapshot_state().is_err());
    assert!(root.retry_payload_load(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::PayloadLoadAwaitingAdoption
    );
    assert!(root.publish_payload_load(&mut service).unwrap());
    assert_eq!(service.snapshot_state().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 3);
}

/// An independently opened root and a restored snapshot cannot acquire the
/// original root's live storage qualification, even with the same source IDs.
/// Rejected starts leave both lanes unchanged; the actual original still loads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_root_and_snapshot_restore_cannot_qualify_payload_reload() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, _, sequence) = accepted(&fixture).await;
    let mut restored =
        MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
    assert!(
        restored
            .release_stored_payload(sequence, root.storage_scope())
            .is_err()
    );
    assert!(root.start_payload_load(&mut restored, sequence).is_err());
    service
        .release_stored_payload(sequence, root.storage_scope())
        .unwrap();
    let foreign_fixture = Fixture::new(true);
    let foreign = foreign_fixture.root();
    foreign.observe().await.unwrap();
    assert!(foreign.start_payload_load(&mut service, sequence).is_err());
    assert_eq!(foreign.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(foreign_fixture.disk(), (0, 0));
    root.start_payload_load(&mut service, sequence).unwrap();
    root.observe().await.unwrap();
    assert!(root.publish_payload_load(&mut restored).is_err());
    assert!(service.snapshot_state().is_err());
    assert!(root.publish_payload_load(&mut service).unwrap());
}

/// Corrupt stored bytes fail closed, retaining the cold canonical source and
/// original attempt. Restoring the exact checksummed chunk permits paced retry;
/// no empty payload, guessed receipt or new occurrence is created on failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_payload_chunk_keeps_original_until_exact_repair_and_retry() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, _, sequence) = accepted(&fixture).await;
    let before = service.snapshot_state().unwrap();
    let disk = fixture.disk();
    service
        .release_stored_payload(sequence, root.storage_scope())
        .unwrap();
    let connection = rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap();
    let (key, value): (String, Vec<u8>) = connection
        .query_row(
            "SELECT key,value FROM records WHERE key LIKE '%chunk:00000000'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE records SET value=zeroblob(length(value)) WHERE key=?1",
            [&key],
        )
        .unwrap();
    root.start_payload_load(&mut service, sequence).unwrap();
    let CustodyWorkerStatus::PayloadLoadRetryAt(due) = root.observe().await.unwrap() else {
        panic!("corrupt source must fail closed")
    };
    assert!(root.failure().unwrap().is_some());
    assert!(root.publish_payload_load(&mut service).is_err());
    assert!(service.snapshot_state().is_err());
    connection
        .execute(
            "UPDATE records SET value=?1 WHERE key=?2",
            rusqlite::params![value, key],
        )
        .unwrap();
    assert!(root.retry_payload_load(&mut service, due).unwrap());
    root.observe().await.unwrap();
    assert!(root.publish_payload_load(&mut service).unwrap());
    assert_eq!(service.snapshot_state().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
}

/// A changed directory after the actual source read cannot qualify bytes under
/// the original root. A same-mode/same-label replacement is insufficient; only
/// restoring the exact original inode lets the owned read retry and publish.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moved_root_after_actual_payload_read_cannot_publish_until_restored() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, _, sequence) = accepted(&fixture).await;
    let before = service.snapshot_state().unwrap();
    service
        .release_stored_payload(sequence, root.storage_scope())
        .unwrap();
    let retained = fixture.path.with_extension("retained");
    *fixture.probe.relocate_after_commit.lock().unwrap() =
        Some((fixture.path.clone(), retained.clone()));
    root.start_payload_load(&mut service, sequence).unwrap();
    let CustodyWorkerStatus::PayloadLoadRetryAt(due) = root.observe().await.unwrap() else {
        panic!("moved root must not publish read bytes")
    };
    assert!(root.publish_payload_load(&mut service).is_err());
    assert!(!fixture.path.join("custody.sqlite").exists());
    assert!(service.snapshot_state().is_err());
    std::fs::remove_dir(&fixture.path).unwrap();
    std::fs::rename(retained, &fixture.path).unwrap();
    assert!(root.retry_payload_load(&mut service, due).unwrap());
    root.observe().await.unwrap();
    assert!(root.publish_payload_load(&mut service).unwrap());
    assert_eq!(service.snapshot_state().unwrap(), before);
}

/// Last-root loss must still retain the real blocking read handle and kernel
/// lock. A replacement root remains unavailable until the read is released and
/// its fallback drain actually retires; dropping an observer is not retirement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_root_keeps_kernel_owner_until_actual_payload_worker_retires() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, _, sequence) = accepted(&fixture).await;
    service
        .release_stored_payload(sequence, root.storage_scope())
        .unwrap();
    *fixture.probe.gate.0.lock().unwrap() = false;
    root.start_payload_load(&mut service, sequence).unwrap();
    fixture.probe.started.notified().await;
    drop(root);
    let competing = fixture.root();
    assert_eq!(
        competing.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    drop(competing);
    fixture.probe.release();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let next = fixture.root();
            if next.observe().await.unwrap() == CustodyWorkerStatus::Idle {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        service.snapshot_state().is_err(),
        "fallback drain is not source publication"
    );
}
