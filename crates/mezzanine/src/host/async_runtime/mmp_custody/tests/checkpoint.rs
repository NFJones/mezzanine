//! Original-store checkpoint workers across cancellation, loss and replacement.

use super::*;

/// Commits a real original with storage policy enabled before acceptance, then
/// releases its cache pin. The returned lower state genuinely cannot load it.
async fn cold(fixture: &Fixture) -> (MmpCustodyRoot, MessageService) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, sender, target, _) = fixture_service();
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "checkpoint-original"),
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
    service
        .release_stored_payload(ticket.sequence(), root.storage_scope())
        .unwrap();
    (root, service)
}

/// Losing only the observation future cannot release the real read, its capture
/// or exclusive lane. Adopting later yields exact cold refs and changes neither
/// disk state nor cache usage; stopping still permits settlement of this capture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_observer_abort_preserves_original_capture_and_worker_lane() {
    let fixture = Fixture::new(true);
    let (root, service) = cold(&fixture).await;
    let before = service.storage_usage().unwrap();
    let disk = fixture.disk();
    *fixture.probe.gate.0.lock().unwrap() = false;
    root.start_checkpoint(&service).unwrap();
    fixture.probe.started.notified().await;
    let observing = root.clone();
    let observer = tokio::spawn(async move { observing.observe().await });
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert!(root.start_checkpoint(&service).is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), root.observe())
            .await
            .is_err()
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    root.request_stop();
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotAwaitingAdoption
    );
    let (snapshot, lease) = root.adopt_checkpoint().unwrap();
    snapshot.validate_storage_snapshot().unwrap();
    assert_eq!(snapshot.storage_snapshot.unwrap().sources[0].sequence, 1);
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(fixture.disk(), disk);
    drop(lease);
    assert_eq!(service.storage_usage().unwrap(), before);
}

/// Actual post-read thread loss must be joined before exact read-only retry;
/// an early retry is inert and the one-second backoff does not recapture newer
/// lower state. The SQLite source and original allocator stay untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_actual_worker_loss_retries_exact_capture_after_join_and_backoff() {
    let fixture = Fixture::new(true);
    let (root, service) = cold(&fixture).await;
    let disk = fixture.disk();
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_checkpoint(&service).unwrap();
    fixture.probe.started.notified().await;
    let CustodyWorkerStatus::SnapshotRetryAt(at) = root.observe().await.unwrap() else {
        panic!("actual loss must join before retry")
    };
    assert!(!root.retry_checkpoint(Instant::now()).unwrap());
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    assert!(root.retry_checkpoint(at).unwrap());
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotAwaitingAdoption
    );
    root.adopt_checkpoint()
        .unwrap()
        .0
        .validate_storage_snapshot()
        .unwrap();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.disk(), disk);
}

/// Equal owner labels on two actual stores cannot qualify foreign lower state.
/// The original remains checkpointable; foreign refusal changes no disk or lane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_foreign_root_refuses_equal_owner_labels_without_mutation() {
    let fixture = Fixture::new(true);
    let (root, service) = cold(&fixture).await;
    let foreign_fixture = Fixture::new(true);
    let foreign = foreign_fixture.root();
    assert_eq!(foreign.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let disk = foreign_fixture.disk();
    assert!(foreign.start_checkpoint(&service).is_err());
    assert_eq!(foreign.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(foreign_fixture.disk(), disk);
    root.start_checkpoint(&service).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotAwaitingAdoption
    );
    root.adopt_checkpoint().unwrap();
}

/// A completed read followed by physical directory replacement cannot publish
/// descriptive checkpoint binding for the replacement. Accepted lower evidence
/// stays retained and cold, and exact original retry remains owned by the root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_postread_store_replacement_fails_closed_without_cache_mutation() {
    let fixture = Fixture::new(true);
    let (root, service) = cold(&fixture).await;
    let before = service.storage_usage().unwrap();
    let retained = fixture.path.with_extension("checkpoint-retained");
    *fixture.probe.relocate_after_commit.lock().unwrap() =
        Some((fixture.path.clone(), retained.clone()));
    root.start_checkpoint(&service).unwrap();
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotRetryAt(_)
    ));
    assert!(root.adopt_checkpoint().is_err());
    let after = service.storage_usage().unwrap().unwrap();
    let before = before.unwrap();
    assert_eq!(after.pending_messages, before.pending_messages);
    assert_eq!(after.cache, before.cache);
    assert!(
        after.metadata_bytes > before.metadata_bytes,
        "failed actual verification still owns its original metadata reservation"
    );
    std::fs::remove_dir_all(&fixture.path).unwrap();
    std::fs::rename(retained, &fixture.path).unwrap();
}

/// A committed marker cannot qualify missing or corrupt original chunks. Failed
/// read-only verification preserves the exact cold ledger and metadata lease;
/// no invented empty envelope or generic fallback can produce a checkpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_corrupt_original_refuses_publication_and_retains_cold_ledger() {
    let fixture = Fixture::new(true);
    let (root, service) = cold(&fixture).await;
    let before = service.storage_usage().unwrap().unwrap();
    let sql = rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap();
    assert!(
        sql.execute(
            "UPDATE records SET value=zeroblob(length(value)) WHERE key LIKE 'a1:%:chunk:%'",
            []
        )
        .unwrap()
            > 0
    );
    root.start_checkpoint(&service).unwrap();
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotRetryAt(_)
    ));
    assert!(root.adopt_checkpoint().is_err());
    let after = service.storage_usage().unwrap().unwrap();
    assert_eq!(after.pending_messages, before.pending_messages);
    assert_eq!(after.cache, before.cache);
    assert!(after.metadata_bytes > before.metadata_bytes);
}

/// A real committed SQLite original may keep an expired receipt after a genuine
/// duplicate lookup without terminal retirement. Qualification and actual atomic
/// snapshot publication must preserve its TTL basis/status after the cache drops
/// its body; neither capture nor publication mutates the original disk ledger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_expired_duplicate_receipt_qualifies_cold_original_and_publishes() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, sender, target, _) = fixture_service();
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    let mut original = envelope(&sender, &target, "expired-checkpoint");
    original.ttl_ms = Some(5);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            original.clone(),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(&mut service, None).unwrap();
    let duplicate = service
        .storage_qualified_duplicate(
            &sender.agent_id,
            &original,
            root.storage_scope(),
            MessageScope::Session,
            106,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        duplicate.status,
        mez_agent::messaging::DeliveryStatus::Expired
    );
    drop(original);
    service
        .release_stored_payload(ticket.sequence(), root.storage_scope())
        .unwrap();
    let before = service.storage_usage().unwrap().unwrap();
    let disk = fixture.disk();
    root.start_checkpoint(&service).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotAwaitingAdoption
    );
    let (snapshot, lease) = root.adopt_checkpoint().unwrap();
    snapshot.validate_storage_snapshot().unwrap();
    let source = &snapshot.storage_snapshot.as_ref().unwrap().sources[0];
    assert_eq!(source.delivery.status, "expired");
    assert_eq!(source.accepted_at_ms, 100);
    assert_eq!(source.ttl_ms, Some(5));
    let runtime = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let (session, context) = runtime
        .host_checkpoint_snapshot_with_message_state(snapshot)
        .unwrap();
    let snapshots =
        crate::storage::snapshot::SnapshotRepository::new(fixture.path.join("expired-snapshots"));
    let saved = snapshots
        .create_from_session_with_context_async(
            "expired-cold-mmp",
            None,
            &session,
            context.as_creation_context(),
        )
        .await
        .unwrap();
    assert!(!saved.restorable);
    let state = snapshots
        .inspect_payload_async("expired-cold-mmp")
        .await
        .unwrap()
        .message_state
        .unwrap();
    assert_eq!(
        state.storage_snapshot.unwrap().sources[0].delivery.status,
        "expired"
    );
    drop(lease);
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
}
