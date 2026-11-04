//! Real original-store recovery workers, cancellation, loss and retirement.

use super::*;
use mez_agent::messaging::{MessageNativeIncarnation, MessageServiceSnapshot};

/// Starts and settles actual storage acceptance/checkpoint workers, then retires
/// their root and waits for the actual kernel fence to become available. Native
/// facts come from an actual original registration, never fabricated snapshot data.
async fn checkpointed_original(fixture: &Fixture) -> MessageServiceSnapshot {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, sender, target, _) = fixture_service();
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    let registration = service
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    service
        .bind_native_conversation(
            &registration,
            "original-conversation",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "original"),
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
    root.start_checkpoint(&service).unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotAwaitingAdoption
    );
    let (snapshot, lease) = root.adopt_checkpoint().unwrap();
    drop(lease);
    drop(service);
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    snapshot
}

/// Keeps destination policy explicit instead of accepting checkpoint limits.
fn request(checkpoint: MessageServiceSnapshot) -> CustodyRecoveryRequest {
    CustodyRecoveryRequest {
        checkpoint: Some(checkpoint),
        limits: Default::default(),
        retention_messages: 16,
        retention_bytes: 4 * 1024 * 1024,
    }
}

/// Uses the same production constructor and worker, with the existing fixture's
/// actual blocking/panic gate so fixture Drop also prevents assertion hangs.
fn gated_root(fixture: &Fixture, checkpoint: MessageServiceSnapshot) -> MmpCustodyRoot {
    MmpCustodyRoot::construct_recovery(
        Destination {
            clock_source: Default::default(),
            root: fixture.path.clone(),
            owner: "original-session".into(),
            limits: Default::default(),
        },
        Handle::current(),
        request(checkpoint),
        Some(fixture.probe.clone()),
    )
}

/// A real reopened original reconstructs cold pending state through the owned
/// worker. Saved native facts remain available to startup while input stays
/// charged, then release before the service starts ordinary root-owned hydration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_recovery_keeps_input_charged_through_staging_then_hydrates_original() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let input_charge = serde_json::to_vec(&snapshot).unwrap().len() * 8;
    let before = fixture.disk();
    let root = MmpCustodyRoot::recover(
        fixture.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    let joined_usage = {
        let state = root.0.state.lock().unwrap();
        let State::Recovered {
            result: Ok(written),
            ..
        } = &*state
        else {
            panic!("actual positive join required")
        };
        written.service.storage_usage().unwrap().unwrap()
    };
    assert_eq!(joined_usage.pending_messages, 1);
    assert_eq!(
        joined_usage.cache,
        mez_agent::messaging::MessagePayloadUsage::default()
    );
    assert!(joined_usage.metadata_bytes >= input_charge);
    assert!(root.retry_recovery(Instant::now()).is_err());
    let mut staged = root.adopt_recovery().unwrap();
    assert_eq!(
        staged
            .checkpoint()
            .storage_snapshot
            .as_ref()
            .unwrap()
            .native
            .len(),
        1
    );
    assert_eq!(
        staged.service_mut().storage_usage().unwrap().unwrap(),
        joined_usage
    );
    assert!(staged.service_mut().discover_agents().is_empty());
    assert!(root.adopt_recovery().is_err());
    let mut service = staged.into_service();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes + input_charge,
        joined_usage.metadata_bytes
    );
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(fixture.disk(), before);
    assert!(root.start_payload_load(&mut service, 1).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::PayloadLoadAwaitingAdoption
    );
    assert!(root.publish_payload_load(&mut service).unwrap());
    assert_eq!(
        service
            .storage_usage()
            .unwrap()
            .unwrap()
            .cache
            .live_payload_bytes,
        "original\0payload".len() * 20_000
    );
    assert_eq!(fixture.disk(), before);
}

/// Observer cancellation cannot remove the real worker, reissue its input,
/// release the kernel owner or publish a partial service while I/O is gated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_recovery_observer_preserves_actual_worker_and_original_input() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let before = fixture.disk();
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    *fixture.probe.gate.0.lock().unwrap() = false;
    let root = gated_root(&fixture, snapshot);
    fixture.probe.started.notified().await;
    let observer = {
        let root = root.clone();
        tokio::spawn(async move { root.observe().await })
    };
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert!(root.adopt_recovery().is_err());
    assert!(root.retry_recovery(Instant::now()).is_err());
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts + 1);
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    let service = root.adopt_recovery().unwrap().into_service();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    assert_eq!(fixture.disk(), before);
}

/// Actual worker panic after reconstruction discards its unpublished candidate,
/// while the original input, position and kernel fence survive until joined
/// retry. Early retry cannot bypass monotonic scheduling or replace a positive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_worker_loss_retries_same_original_after_actual_join_and_backoff() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let before = fixture.disk();
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let root = gated_root(&fixture, snapshot);
    fixture.probe.started.notified().await;
    let CustodyWorkerStatus::RecoveryRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual lost worker must join")
    };
    assert!(root.failure().unwrap().is_some());
    assert!(!root.retry_recovery(Instant::now()).unwrap());
    assert!(root.adopt_recovery().is_err());
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert!(root.retry_recovery(due).unwrap());
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts + 2);
    assert!(root.retry_recovery(due).is_err());
    let service = root.adopt_recovery().unwrap().into_service();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    assert_eq!(
        service.storage_usage().unwrap().unwrap().cache,
        mez_agent::messaging::MessagePayloadUsage::default()
    );
    assert_eq!(fixture.disk(), before);
}

/// Last-root loss transfers the actual blocked worker and input to retirement;
/// a competing root stays excluded until the worker finishes and is drained.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn last_recovery_root_drop_drains_actual_worker_before_releasing_kernel_fence() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let before = fixture.disk();
    *fixture.probe.gate.0.lock().unwrap() = false;
    let root = gated_root(&fixture, snapshot);
    fixture.probe.started.notified().await;
    drop(root);
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    fixture.probe.release();
    wait_until_unlocked(&fixture.path).await;
    assert_eq!(fixture.disk(), before);
}

/// Missing destinations must remain missing: recovery never creates a fresh
/// private allocation with equal public owner text and then calls it restored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_original_recovery_never_creates_a_replacement_directory() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let missing = fixture.path.join("missing-original");
    let before = fixture.disk();
    let root = MmpCustodyRoot::recover(
        missing.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryRetryAt(_)
    ));
    assert!(!missing.exists());
    assert!(root.adopt_recovery().is_err());
    assert_eq!(fixture.disk(), before);
}

/// An existing directory/lock with a temporarily absent original database must
/// not receive an empty SQLite replacement. The restored original can retry
/// under the same retained fence when no earlier store position was observed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_original_database_is_preserved_for_retry_without_recreation() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let database = fixture.path.join("custody.sqlite");
    let retained = fixture.path.join("custody.retained");
    let before = fixture.disk();
    std::fs::rename(&database, &retained).unwrap();
    let root = MmpCustodyRoot::recover(
        fixture.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    let CustodyWorkerStatus::RecoveryRetryAt(due) = root.observe().await.unwrap() else {
        panic!("missing original must fail")
    };
    assert!(!database.exists());
    std::fs::rename(retained, database).unwrap();
    assert!(root.retry_recovery(due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    assert_eq!(
        root.adopt_recovery()
            .unwrap()
            .into_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert_eq!(fixture.disk(), before);
}

/// Once the first worker saw a position, retry may not silently adopt a later
/// store generation. A direct low-level fixture mutation models a writer that
/// bypasses the cooperative fence; reject before another reconstruction read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_recovery_worker_cannot_refresh_its_frozen_store_position() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let root = gated_root(&fixture, snapshot);
    fixture.probe.started.notified().await;
    let CustodyWorkerStatus::RecoveryRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual loss required")
    };
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    let generation = fixture.disk().0 as u64;
    let mut store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    store
        .commit(
            generation,
            &[crate::storage::mmp_custody::CustodyWrite {
                key: "fixture-later-row",
                previous: None,
                value: Some(b"later"),
            }],
        )
        .unwrap();
    assert!(root.retry_recovery(due).unwrap());
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryRetryAt(_)
    ));
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
    assert!(root.adopt_recovery().is_err());
}

/// A completed result still owns input, cold metadata and the original kernel
/// fence. Last-root retirement must dispose of that whole candidate before a
/// new cooperating owner can open the store, without inventing an adoption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joined_recovery_retirement_releases_candidate_without_publishing_it() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let before = fixture.disk();
    let root = MmpCustodyRoot::recover(
        fixture.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    assert_eq!(fixture.disk(), before);
}

/// Equal owner text, copied allocation UUID and complete source bytes do not
/// establish original physical-store continuity. A real worker may inspect the
/// copy, but must not publish it as original custody or modify either store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_worker_refuses_a_copied_store_with_equal_public_identity() {
    let original = Fixture::new(true);
    let snapshot = checkpointed_original(&original).await;
    let copied = Fixture::new(true);
    let fence = RootOwnership::acquire(&copied.path).unwrap();
    std::fs::copy(
        original.path.join("custody.sqlite"),
        copied.path.join("custody.sqlite"),
    )
    .unwrap();
    drop(fence);
    let before = copied.disk();
    let root = MmpCustodyRoot::recover(
        copied.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryRetryAt(_)
    ));
    assert!(root.adopt_recovery().is_err());
    assert!(
        root.failure()
            .unwrap()
            .unwrap()
            .to_string()
            .contains("original allocation changed")
    );
    assert_eq!(copied.disk(), before);
    assert_eq!(original.disk(), before);
}

/// Existing-only opening must not initialize a blank file as a replacement for
/// original custody. Restore the retained original manually only after proving
/// that the failed worker left the blank file empty and its schema untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_never_initializes_an_existing_blank_database() {
    use std::os::unix::fs::OpenOptionsExt;
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let database = fixture.path.join("custody.sqlite");
    let retained = fixture.path.join("custody.retained");
    let before = fixture.disk();
    std::fs::rename(&database, &retained).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&database)
        .unwrap();
    let root = MmpCustodyRoot::recover(
        fixture.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    let CustodyWorkerStatus::RecoveryRetryAt(due) = root.observe().await.unwrap() else {
        panic!("blank replacement must fail")
    };
    assert_eq!(std::fs::metadata(&database).unwrap().len(), 0);
    std::fs::remove_file(&database).unwrap();
    std::fs::rename(retained, database).unwrap();
    assert!(root.retry_recovery(due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    assert_eq!(
        root.adopt_recovery()
            .unwrap()
            .into_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert_eq!(fixture.disk(), before);
}

/// Cancellable session staging must itself retain the actual recovered root.
/// Dropping the caller's handle cannot release its kernel fence while the
/// candidate still holds original native facts and unpublished canonical state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_recovery_retains_actual_root_until_candidate_retirement() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let before = fixture.disk();
    let root = MmpCustodyRoot::recover(
        fixture.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    let witness = Arc::downgrade(&root.0);
    let staged = root.adopt_recovery().unwrap();
    drop(root);
    assert!(
        witness.upgrade().is_some(),
        "staging must retain the actual root owner"
    );
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert_eq!(
        staged
            .checkpoint()
            .storage_snapshot
            .as_ref()
            .unwrap()
            .sources
            .len(),
        1
    );
    drop(staged);
    wait_until_unlocked(&fixture.path).await;
    assert_eq!(fixture.disk(), before);
}

/// The original cooperating owner-lock allocation is required too. Recovery
/// must not create a new lock after the original pathname disappears, even
/// though its directory and all source bytes still exist and have equal labels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_original_owner_lock_is_not_recreated_by_recovery() {
    let fixture = Fixture::new(true);
    let snapshot = checkpointed_original(&fixture).await;
    let lock = fixture.path.join("owner.lock");
    let retained = fixture.path.join("owner.retained");
    let before = fixture.disk();
    std::fs::rename(&lock, &retained).unwrap();
    let root = MmpCustodyRoot::recover(
        fixture.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        request(snapshot),
    );
    let CustodyWorkerStatus::RecoveryRetryAt(due) = root.observe().await.unwrap() else {
        panic!("missing original lock must fail")
    };
    assert!(!lock.exists());
    std::fs::rename(retained, lock).unwrap();
    assert!(root.retry_recovery(due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    assert_eq!(
        root.adopt_recovery()
            .unwrap()
            .into_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert_eq!(fixture.disk(), before);
}
