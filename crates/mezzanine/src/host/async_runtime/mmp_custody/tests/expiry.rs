//! Actual expiry workers, physical ownership, original retries and actor wakes.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};

/// Ensures a failed assertion cannot strand a real blocking expiry worker.
struct Gate(Arc<Probe>);
impl Drop for Gate {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// One real stored raw source with an independent expiry-only physical gate.
pub(super) async fn scene(
    fixture: &Fixture,
    probe: Arc<Probe>,
) -> (
    MmpCustodyRoot,
    MessageService,
    SenderIdentity,
    SenderIdentity,
) {
    let mut root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    Arc::get_mut(&mut root.0).unwrap().terminal_probe = Some(probe);
    let (mut service, sender, target, _) = fixture_service();
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    service
        .require_native_continuity(root.storage_scope())
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    accept(&root, &mut service, &sender, &target, 10, 5).await;
    (root, service, sender, target)
}

/// Persists through the actual root, with the original caller acceptance clock.
pub(super) async fn accept(
    root: &MmpCustodyRoot,
    service: &mut MessageService,
    sender: &SenderIdentity,
    target: &SenderIdentity,
    at: u64,
    ttl: u64,
) {
    let mut original = envelope(sender, target, "root-expiry-original");
    original.ttl_ms = Some(ttl);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            original,
            MessageScope::Session,
            at,
            4096,
            None,
        )
        .unwrap();
    root.start(service, &ticket, None).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(service, None).unwrap();
}
fn reserve(
    root: &MmpCustodyRoot,
    service: &mut MessageService,
) -> mez_agent::messaging::MessageTerminalTicket {
    service
        .prepare_storage_expiry(root.storage_scope(), 16, 8 * 1024 * 1024)
        .unwrap()
        .unwrap()
}
fn terminals(fixture: &Fixture) -> i64 {
    fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 't1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// Actual observer cancellation and panic after the real t1 commit cannot lose
/// the original operation. Retry after observed join reconciles the same bytes
/// while stopping, with no extra generation and no terminal before adoption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_root_lost_positive_keeps_original_through_stop_and_exact_join() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(false);
    let _gate = Gate(probe.clone());
    let (root, mut service, sender, target) = scene(&fixture, probe.clone()).await;
    let observer = service.payload_observer().unwrap();
    let ticket = reserve(&root, &mut service);
    probe.panic_after_commit.store(true, Ordering::SeqCst);
    root.start_terminal(&mut service, &ticket).unwrap();
    probe.started.notified().await;
    let waiting = {
        let root = root.clone();
        tokio::spawn(async move { root.observe().await })
    };
    tokio::task::yield_now().await;
    waiting.abort();
    let _ = waiting.await;
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert!(service.cancel_storage_terminal(&ticket).is_err());
    assert!(
        service
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    root.request_stop();
    probe.release();
    let CustodyWorkerStatus::TerminalRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual worker loss must join");
    };
    assert_eq!(terminals(&fixture), 1);
    let disk = fixture.disk();
    assert!(!root.retry_terminal(&mut service, Instant::now()).unwrap());
    assert!(root.retry_terminal(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    let (mut foreign, _, _, _) = fixture_service();
    assert!(root.adopt_terminal(&mut foreign).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    assert_eq!(fixture.disk(), disk);
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert_eq!(
        service.delivery_outcomes_for(&sender.agent_id, 10).unwrap()[0].observed_at_ms,
        16
    );
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// Last-root retirement retains both the real kernel fence and lower metadata
/// escrow until its actual blocked worker joins. It grants no live adoption;
/// the committed observation remains available to subsequent original recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_last_root_drop_drains_worker_before_releasing_physical_owner() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(false);
    let _gate = Gate(probe.clone());
    let (root, mut service, sender, _) = scene(&fixture, probe.clone()).await;
    let observer = service.payload_observer().unwrap();
    let ticket = reserve(&root, &mut service);
    root.start_terminal(&mut service, &ticket).unwrap();
    probe.started.notified().await;
    drop(root);
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    probe.release();
    wait_until_unlocked(&fixture.path).await;
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert_eq!(terminals(&fixture), 1);
    assert!(
        service
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
}

/// A real post-commit root relocation fails the final position check while the
/// opaque positive stays owned. Restoring the same actual path allows only the
/// position read to retry, without issuing a second expiry write or nonce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_position_failure_retains_positive_and_refreshes_without_rewrite() {
    let fixture = Fixture::new(true);
    let retained = Fixture::new(true);
    let probe = Probe::new(true);
    let (root, mut service, sender, _) = scene(&fixture, probe.clone()).await;
    *probe.relocate_after_commit.lock().unwrap() =
        Some((fixture.path.clone(), retained.path.clone()));
    let ticket = reserve(&root, &mut service);
    root.start_terminal(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual path replacement must refuse position");
    };
    assert!(root.adopt_terminal(&mut service).is_err());
    assert!(
        service
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(terminals(&retained), 1);
    let disk = retained.disk();
    std::fs::remove_dir(&fixture.path).unwrap();
    std::fs::rename(&retained.path, &fixture.path).unwrap();
    assert!(root.retry_terminal(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.disk(), disk);
}

/// A genuine SQLite write error returns unknown input rather than worker loss.
/// The same root connection/plan/decoder reservation retry after actual join;
/// removing the failing trigger cannot reset the first source or observation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_unknown_store_result_retries_original_after_real_join() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(true);
    let (root, mut service, sender, _) = scene(&fixture, probe.clone()).await;
    let connection = fixture.connection();
    connection.execute_batch("CREATE TRIGGER refuse_expiry BEFORE INSERT ON records WHEN NEW.key LIKE 't1:%' BEGIN SELECT RAISE(FAIL,'fixture expiry refusal'); END").unwrap();
    let ticket = reserve(&root, &mut service);
    root.start_terminal(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = root.observe().await.unwrap() else {
        panic!("physical failure remains unknown");
    };
    assert!(root.failure().unwrap().is_some());
    assert_eq!(terminals(&fixture), 0);
    connection
        .execute_batch("DROP TRIGGER refuse_expiry")
        .unwrap();
    assert!(root.retry_terminal(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    assert_eq!(
        service.delivery_outcomes_for(&sender.agent_id, 10).unwrap()[0].observed_at_ms,
        16
    );
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 2);
}

/// With no incoming actor traffic, a finite original TTL must wake the actual
/// producer. Its blocked physical worker permits unrelated lifecycle requests;
/// shutdown waits for the real commit/adoption without fabricating recipient ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_actor_idle_deadline_wakes_and_shutdown_drains_original() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(false);
    let _gate = Gate(probe.clone());
    let mut root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    Arc::get_mut(&mut root.0).unwrap().terminal_probe = Some(probe.clone());
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", vec![]);
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", vec![]);
    service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    root.configure_payload_spill(service.message_service_mut(), Default::default())
        .unwrap();
    service
        .message_service_mut()
        .require_native_continuity(root.storage_scope())
        .unwrap();
    let accepted_at = crate::runtime::current_unix_millis();
    accept(
        &root,
        service.message_service_mut(),
        &sender,
        &target,
        accepted_at,
        1000,
    )
    .await;
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root.clone()).unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert!(crate::runtime::current_unix_millis() <= accepted_at + 1000);
    let runner = tokio::spawn(actor.run());
    tokio::time::timeout(Duration::from_secs(5), probe.started.notified())
        .await
        .unwrap();
    assert!(crate::runtime::current_unix_millis() > accepted_at + 1000);
    assert_eq!(terminals(&fixture), 0);
    assert!(handle.lifecycle_state().await.is_ok());
    let stopping = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.shutdown().await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!stopping.is_finished());
    probe.release();
    stopping.await.unwrap().unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(terminals(&fixture), 1);
    let outcomes = exited
        .service
        .message_service()
        .delivery_outcomes_for(&sender.agent_id, 10)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].reason,
        mez_agent::messaging::DeliveryTerminalReason::Expired
    );
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
}
