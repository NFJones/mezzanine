//! Real native registration journal workers and original-store observations.

use super::*;
use mez_agent::messaging::{MessageAcceptanceDecodeLimits, MessageNativeIncarnation};

/// Builds actual native tokens under the root's actual empty-store policy.
async fn scene(
    fixture: &Fixture,
) -> (
    MmpCustodyRoot,
    MessageService,
    SenderIdentity,
    SenderIdentity,
    MessageNativeIncarnation,
) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, sender, target, _) = fixture_service();
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    let epoch = MessageNativeIncarnation::default();
    let registration = service
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    service
        .bind_native_conversation(&registration, "native-parent", &epoch)
        .unwrap();
    (root, service, sender, target, epoch)
}

/// Standard bounded inventory policy; native metadata never decodes a body.
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// Actual committed facts survive reopening/inventory, but neither the observed
/// data nor a positive status marks live state before exact actor adoption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_registration_journal_survives_store_reopen_without_becoming_authority() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, target, epoch) = scene(&fixture).await;
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    assert!(root.retry_native_continuity(Instant::now()).is_err());
    let stored =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let fact = stored
        .read_native_continuity(target.agent_id.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(
        fact.snapshot.native.shell_incarnation,
        epoch.snapshot_identity()
    );
    assert_eq!(fact.snapshot.identity.agent_id, target.agent_id.as_str());
    let inventory = stored.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.native_continuity_keys.len(), 1);
    assert!(inventory.occurrences.is_empty() && inventory.acknowledgement_keys.is_empty());
    assert!(root.adopt_native_continuity(&mut service).unwrap());
    assert!(service.native_continuity_is_recorded(&target.agent_id));
    assert!(root.adopt_native_continuity(&mut service).is_err());
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().cache,
        mez_agent::messaging::MessagePayloadUsage::default()
    );
}

/// The physical commit can outlive its observer and actual worker result. Retry
/// must reconcile the original batch without incrementing generation again or
/// marking the binding before the actual positive join is adopted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_journal_lost_positive_reconciles_exact_original_after_join() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, target, _) = scene(&fixture).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    let CustodyWorkerStatus::NativeContinuityRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual lost result must join")
    };
    let committed = fixture.disk();
    assert_eq!(committed, (1, 1));
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    assert!(!root.retry_native_continuity(Instant::now()).unwrap());
    assert!(root.retry_native_continuity(due).unwrap());
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(root.adopt_native_continuity(&mut service).unwrap());
    assert_eq!(fixture.disk(), committed);
}

/// A replacement binding may share every public label and sequence floor with
/// the original. Its new opaque shell cannot inherit an old journal completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_native_journal_completion_leaves_replacement_unconfirmed() {
    let fixture = Fixture::new(false);
    let (root, mut service, _, target, _) = scene(&fixture).await;
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    let registration = service
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    let replacement = MessageNativeIncarnation::default();
    service
        .bind_native_conversation(&registration, "native-parent", &replacement)
        .unwrap();
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(!root.adopt_native_continuity(&mut service).unwrap());
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(root.adopt_native_continuity(&mut service).unwrap());
    let stored =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    assert_eq!(
        stored
            .read_native_continuity(target.agent_id.as_str())
            .unwrap()
            .unwrap()
            .snapshot
            .native
            .shell_incarnation,
        replacement.snapshot_identity()
    );
}

/// A foreign lower runtime cannot consume the original positive result even if
/// its public identity/CID text matches. Correct adoption remains available.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_native_journal_adopter_cannot_steal_the_original_result() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, target, _) = scene(&fixture).await;
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    let mut foreign = MessageService::default();
    assert!(root.adopt_native_continuity(&mut foreign).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(root.adopt_native_continuity(&mut service).unwrap());
}

/// Observer cancellation cannot free/reissue metadata or release its actual
/// kernel owner while the native writer is blocked inside the real worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_native_journal_observer_preserves_actual_writer_and_fence() {
    let fixture = Fixture::new(false);
    let (root, mut service, _, target, _) = scene(&fixture).await;
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    let observer = {
        let root = root.clone();
        tokio::spawn(async move { root.observe().await })
    };
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert!(root.adopt_native_continuity(&mut service).is_err());
    assert!(root.retry_native_continuity(Instant::now()).is_err());
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(root.adopt_native_continuity(&mut service).unwrap());
}

/// A durable registration floor can cover cancelled, never-accepted sequence
/// gaps. Reserve that high-water atomically so future accepted occurrences start
/// beyond it even though no message row exists for any of those gaps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_journal_reserves_registration_floor_without_accepting_a_message() {
    let fixture = Fixture::new(true);
    let (root, mut service, sender, target, _) = scene(&fixture).await;
    for i in 1..=3 {
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope(&sender, &target, &format!("cancelled-{i}")),
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        assert_eq!(ticket.sequence(), i);
        service.cancel_acceptance(&ticket).unwrap();
    }
    let later = service.register_agent(None, None, "later native", vec![]);
    let registration = service
        .capture_sender_registration(&later.agent_id)
        .unwrap();
    service
        .bind_native_conversation(
            &registration,
            "later-parent",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    root.start_native_continuity(&service, &later.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(root.adopt_native_continuity(&mut service).unwrap());
    let stored =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let inventory = stored.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.sequence_watermark, 3);
    assert!(inventory.occurrences.is_empty());
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &later, "after-floor"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(ticket.sequence(), 4);
    root.start(&mut service, &ticket, None).unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(&mut service, None).unwrap();
}

/// Metadata cannot reserve a floor over an unpublished original acceptance.
/// The caller must settle that original first; refusal leaves the root idle and
/// preserves the exact pending ticket so it can be explicitly released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_journal_refuses_while_an_original_admission_is_reserved() {
    let fixture = Fixture::new(true);
    let (root, mut service, sender, target, _) = scene(&fixture).await;
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "pending"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    assert!(
        root.start_native_continuity(&service, &target.agent_id)
            .is_err()
    );
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(fixture.disk(), (0, 0));
    service.cancel_acceptance(&ticket).unwrap();
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(root.adopt_native_continuity(&mut service).unwrap());
}

/// Last-root retirement must drain the actual blocked native writer, preserving
/// its kernel fence and input lease. The durable observation survives, but an
/// abandoned positive result cannot mark the lower binding as adopted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_journal_last_root_drop_drains_input_without_live_publication() {
    let fixture = Fixture::new(false);
    let (root, service, _, target, _) = scene(&fixture).await;
    let before = service.storage_usage().unwrap().unwrap().metadata_bytes;
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    drop(root);
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    fixture.probe.release();
    wait_until_unlocked(&fixture.path).await;
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        before
    );
    let stored =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    assert!(
        stored
            .read_native_continuity(target.agent_id.as_str())
            .unwrap()
            .is_some()
    );
}

/// A historical equal row cannot substitute for the original last-batch reply
/// after a later generation. The retained plan/preimage must not be refreshed
/// merely because the first native write's result was lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_journal_retry_refuses_unrelated_later_store_generation() {
    let fixture = Fixture::new(true);
    let (root, mut service, _, target, _) = scene(&fixture).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_native_continuity(&service, &target.agent_id)
        .unwrap();
    fixture.probe.started.notified().await;
    let CustodyWorkerStatus::NativeContinuityRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual lost result required")
    };
    let mut store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    store
        .commit(
            fixture.disk().0 as u64,
            &[crate::storage::mmp_custody::CustodyWrite {
                key: "fixture-later",
                previous: None,
                value: Some(b"later"),
            }],
        )
        .unwrap();
    let later = fixture.disk();
    assert!(root.retry_native_continuity(due).unwrap());
    fixture.probe.started.notified().await;
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityRetryAt(_)
    ));
    assert!(root.adopt_native_continuity(&mut service).is_err());
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    assert_eq!(fixture.disk(), later);
}
