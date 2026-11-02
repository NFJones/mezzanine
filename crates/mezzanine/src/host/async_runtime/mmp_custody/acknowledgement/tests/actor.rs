//! Real actor dispatch across original physical ACK gates and shutdown.
//!
//! These tests transfer a genuinely persisted/offered lower ledger into a real
//! runtime actor. Only internal trusted setup registers the root; no fixture
//! claims that ordinary product ingress or native restore is durable already.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
};
use mez_agent::messaging::FanoutBatch;

/// Builds actual persisted sources, releases only the never-issued setup ACK,
/// then transfers exclusive root scheduling to an actor before its first claim.
async fn actor_fixture(
    fixture: &Fixture,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    FanoutBatch,
    MmpCustodyRoot,
) {
    let root = fixture.root();
    let (mut messaging, _, target, ticket) = prepared(&root).await;
    messaging.cancel_acknowledgement(&ticket).unwrap();
    let batch = messaging
        .fanout_ready_for(&target.agent_id, 20, 10)
        .unwrap()
        .unwrap();
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    *service.message_service_mut() = messaging;
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root.clone()).unwrap();
    (handle, actor, batch, root)
}

/// A cancelled peer observer cannot release an issued original ACK. While its
/// actual worker is gated, unrelated actor requests remain responsive, a second
/// ACK issues nothing, and shutdown fences ingress without exiting. Only actual
/// join plus canonical adoption returns the original service with its cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_retains_cancelled_ack_through_gated_graceful_shutdown() {
    let fixture = Fixture::new(false);
    let (handle, actor, batch, _) = actor_fixture(&fixture).await;
    let recipient = batch.recipient.clone();
    let original = fixture.disk();
    let runner = tokio::spawn(actor.run());
    let sender = handle.clone();
    let duplicate = batch.clone();
    let caller = tokio::spawn(async move { sender.acknowledge_message_fanout(batch).await });
    fixture.probe.started.notified().await;
    assert_eq!(fixture.disk(), original);
    assert!(handle.lifecycle_state().await.is_ok());
    assert!(handle.acknowledge_message_fanout(duplicate).await.is_err());
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());

    let shutdown_handle = handle.clone();
    let shutdown = tokio::spawn(async move { shutdown_handle.shutdown().await });
    while !handle.sender.closed.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    assert!(!shutdown.is_finished());
    assert!(!runner.is_finished());
    assert!(handle.lifecycle_state().await.is_err());
    assert_eq!(fixture.disk(), original);
    fixture.probe.release();
    shutdown.await.unwrap().unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        1,
    );
    assert_eq!(fixture.disk(), (original.0 + 1, original.1 + 1));
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
}

/// The original ACK reply cannot become successful before physical commit. The
/// actor publishes the exact cursor after release and a later duplicate does no
/// extra I/O. A rejected second registration cannot replace the original root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_publishes_ack_only_after_real_commit_and_duplicate_is_inert() {
    let fixture = Fixture::new(false);
    let (handle, mut actor, batch, root) = actor_fixture(&fixture).await;
    assert!(actor.register_message_custody(root).is_err());
    let original = fixture.disk();
    let runner = tokio::spawn(actor.run());
    let sender = handle.clone();
    let original_batch = batch.clone();
    let caller =
        tokio::spawn(async move { sender.acknowledge_message_fanout(original_batch).await });
    fixture.probe.started.notified().await;
    assert!(!caller.is_finished());
    assert_eq!(fixture.disk(), original);
    fixture.probe.release();
    let committed = caller.await.unwrap().unwrap();
    assert_eq!(committed.recipient, batch.recipient);
    assert_eq!(committed.last_sequence, 1);
    // Idle re-observation precedes dispatch of the next duplicate request.
    assert_eq!(
        handle.acknowledge_message_fanout(batch).await.unwrap(),
        committed
    );
    assert_eq!(fixture.disk(), (original.0 + 1, original.1 + 1));
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    handle.shutdown().await.unwrap();
    assert_eq!(
        runner
            .await
            .unwrap()
            .service
            .message_service()
            .subscription(&committed.recipient)
            .unwrap()
            .last_sequence,
        1,
    );
}

/// A worker can actually commit and then panic before its callback. The actor
/// retains its source/plan, waits for the real failed join and backoff, and
/// reconciles the same ACK instead of emitting a second record or replaying input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_reconciles_actual_postcommit_worker_loss_without_extra_record() {
    let fixture = Fixture::new(true);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let (handle, actor, batch, _) = actor_fixture(&fixture).await;
    let original = fixture.disk();
    let runner = tokio::spawn(actor.run());
    let cursor = handle.acknowledge_message_fanout(batch).await.unwrap();
    assert_eq!(cursor.last_sequence, 1);
    assert_eq!(fixture.disk(), (original.0 + 1, original.1 + 1));
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&cursor.recipient)
            .unwrap()
            .last_sequence,
        1
    );
}

/// Actual SQLite failure cannot publish ACK or abandon its original when the
/// caller and shutdown observers disappear. Shutdown retains the actor/root and
/// completes the same record once storage becomes writable, without another
/// socket/native application execution or an extra occurrence allocation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_drains_original_ack_after_storage_failure_and_shutdown_observer_loss() {
    let fixture = Fixture::new(true);
    let (handle, actor, batch, root) = actor_fixture(&fixture).await;
    let recipient = batch.recipient.clone();
    let original = fixture.disk();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_ack BEFORE INSERT ON records WHEN NEW.key LIKE 'k1:%' \
         BEGIN SELECT RAISE(ABORT,'injected actual actor ACK failure'); END;",
        )
        .unwrap();
    let runner = tokio::spawn(actor.run());
    let sender = handle.clone();
    let caller = tokio::spawn(async move { sender.acknowledge_message_fanout(batch).await });
    loop {
        if matches!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AcknowledgementRetryAt(_)
        ) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(fixture.disk(), original);
    assert!(!caller.is_finished());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let shutdown_handle = handle.clone();
    let shutdown = tokio::spawn(async move { shutdown_handle.shutdown().await });
    while !handle.sender.closed.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    assert!(!runner.is_finished());
    fixture
        .connection()
        .execute_batch("DROP TRIGGER fail_ack;")
        .unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(fixture.disk(), (original.0 + 1, original.1 + 1));
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
}

/// An offered memory-only occurrence has no immutable source in the store, so
/// the real actor must retain the failed ACK instead of falling back to memory
/// success. Restore of the original source is deliberately not fabricated here;
/// after proving refusal, this fixture aborts the actor (not graceful shutdown).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_never_reports_durable_ack_success_for_memory_only_source() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let messaging = service.message_service_mut();
    let sender = messaging.register_agent(None, None, "sender", vec![]);
    let target = messaging.register_agent(None, None, "target", vec![]);
    messaging.subscribe(&target.agent_id).unwrap();
    messaging
        .accept_at_with_scope(
            &sender.agent_id,
            envelope(&sender, &target, "not-persisted"),
            MessageScope::Session,
            10,
        )
        .unwrap();
    let batch = messaging
        .fanout_ready_for(&target.agent_id, 20, 10)
        .unwrap()
        .unwrap();
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root.clone()).unwrap();
    let runner = tokio::spawn(actor.run());
    let caller = tokio::spawn(async move { handle.acknowledge_message_fanout(batch).await });
    loop {
        if matches!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AcknowledgementRetryAt(_)
        ) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(!caller.is_finished());
    assert_eq!(fixture.disk(), (0, 0));
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    runner.abort();
    assert!(runner.await.unwrap_err().is_cancelled());
    // No actual worker is live here: the original physical failure was joined.
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementRetryAt(_)
    ));
}
