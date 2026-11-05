//! Actual horizon jobs on the shared terminal root/actor ownership lane.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
use mez_agent::messaging::MESSAGE_TERMINAL_HANDOFF_MS;

/// Assertion unwind must release only this actual generated worker gate.
struct Gate(Arc<Probe>);
impl Drop for Gate {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Selects an actual original terminal operation at the supplied trusted test
/// observation; the root still performs the real original-fenced physical write.
async fn observe(root: &MmpCustodyRoot, service: &mut MessageService, at: u64) {
    let ticket = service
        .prepare_storage_expiry(root.storage_scope(), at, usize::MAX)
        .unwrap()
        .unwrap();
    root.start_terminal(service, &ticket).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(service).unwrap(), 1);
}
fn releases(fixture: &Fixture) -> i64 {
    fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'r1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// A real worker lost after r1 commit cannot erase the original outcome or
/// release metadata escrow. Exact observed-join retry while stopping reconciles
/// once without advancing generation, injecting input or fabricating any ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn horizon_root_postcommit_loss_reconciles_original_release_during_stop() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(true);
    let (root, mut service, sender, target) = super::expiry::scene(&fixture, probe.clone()).await;
    observe(&root, &mut service, 16).await;
    let ticket = service
        .prepare_storage_horizon_release(root.storage_scope(), 600016, usize::MAX)
        .unwrap()
        .unwrap();
    probe.panic_after_commit.store(true, Ordering::SeqCst);
    root.start_terminal(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual r1 worker loss must join");
    };
    assert_eq!(releases(&fixture), 1);
    let disk = fixture.disk();
    assert_eq!(
        service
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        service
            .payload_observer()
            .unwrap()
            .snapshot()
            .unwrap()
            .reserved_payload_bytes,
        0
    );
    root.request_stop();
    assert!(root.retry_terminal(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    assert_eq!(fixture.disk(), disk);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        0
    );
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 3);
}

/// Historical clocks are supplied explicitly at the genuine lower/root fixture
/// boundary. Their real t1 commit establishes a future live horizon. With no
/// incoming actor traffic that horizon wakes a real r1 worker; shutdown waits
/// for actual adoption and releases only terminal custody, never recipient ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn horizon_actor_idle_deadline_wakes_and_shutdown_drains_release() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(true);
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
    let first = crate::runtime::current_unix_millis() - MESSAGE_TERMINAL_HANDOFF_MS + 1000;
    super::expiry::accept(
        &root,
        service.message_service_mut(),
        &sender,
        &target,
        first - 6,
        5,
    )
    .await;
    observe(&root, service.message_service_mut(), first).await;
    probe.started.notified().await;
    *probe.gate.0.lock().unwrap() = false;
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root.clone()).unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert!(crate::runtime::current_unix_millis() < first + MESSAGE_TERMINAL_HANDOFF_MS);
    let runner = tokio::spawn(actor.run());
    tokio::time::timeout(Duration::from_secs(5), probe.started.notified())
        .await
        .unwrap();
    assert_eq!(releases(&fixture), 0);
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
    assert_eq!(releases(&fixture), 1);
    assert_eq!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        0
    );
    assert!(
        exited
            .service
            .message_service()
            .delivery_outcomes_for(&sender.agent_id, 10)
            .unwrap()
            .is_empty()
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
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 2);
}
