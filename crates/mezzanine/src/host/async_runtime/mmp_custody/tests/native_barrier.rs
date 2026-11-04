//! Actual actor admission gates through the original native journal worker.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle};
use mez_agent::AgentTurnRecord;
use mez_core::ids::AgentId;
use tokio::sync::oneshot;

/// Enables the actual product barrier before any actor message is reserved.
async fn actor(
    fixture: &Fixture,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    AgentTurnRecord,
    SenderIdentity,
) {
    let (handle, mut actor, root, turn, target) = native_actor::actor_fixture(fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    (handle, actor, root, turn, target)
}

/// Direct trusted ingress returns an honest busy result while the journal owns
/// the lane. No request generation, sequence or full-body side queue is created.
fn start_barrier(
    actor: &mut AsyncRuntimeSessionActor,
    turn: &AgentTurnRecord,
    target: &SenderIdentity,
) {
    let original = native_actor::source(actor, turn, target);
    let (reply, _) = oneshot::channel();
    assert!(
        actor
            .start_tracked_native_message(turn, original, MessageScope::Session, 20, reply)
            .is_err()
    );
    assert_eq!(actor.service.message_service().next_message_sequence(), 1);
    assert!(!actor.service.message_service().has_unpublished_acceptance());
    assert_eq!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
}

/// Observes actual joined/adopted state while ordinary actor requests remain
/// schedulable. The entry count distinguishes a replacement's second write from
/// the brief idle interval after adoption of an older captured native tuple.
async fn await_idle(
    handle: &AsyncRuntimeSessionHandle,
    root: &MmpCustodyRoot,
    fixture: &Fixture,
    writes: usize,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            handle.lifecycle_state().await.unwrap();
            if root.observe().await.unwrap() == CustodyWorkerStatus::Idle
                && fixture.probe.attempts.load(Ordering::SeqCst) >= writes
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// The native metadata commit precedes any source reservation, and a busy
/// observer cannot make the actor abandon the actual journal or block unrelated
/// lifecycle requests. Exact adoption opens the lower gate without accepting mail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_barrier_records_before_any_source_or_request_generation() {
    let fixture = Fixture::new(false);
    let (handle, mut actor, root, turn, target) = actor(&fixture).await;
    start_barrier(&mut actor, &turn, &target);
    fixture.probe.started.notified().await;
    assert_eq!(fixture.disk(), (0, 0));
    let runner = tokio::spawn(actor.run());
    assert!(handle.lifecycle_state().await.is_ok());
    fixture.probe.release();
    await_idle(&handle, &root, &fixture, 1).await;
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    let id = AgentId::opaque(turn.agent_id).unwrap();
    assert!(
        exited
            .service
            .message_service()
            .native_continuity_is_recorded(&id)
    );
    assert!(
        exited
            .service
            .message_service()
            .pending_native_continuity()
            .is_none()
    );
    assert_eq!(exited.service.message_service().next_message_sequence(), 1);
    assert_eq!(
        exited.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    assert_eq!(fixture.disk(), (1, 2));
}

/// A new real shell epoch observed during old metadata I/O cannot inherit the
/// old positive. The actor records the replacement before opening admission,
/// preserving the current observation and both lifetime tuples without creating
/// a message sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_barrier_reobserves_replacement_after_late_positive() {
    let fixture = Fixture::new(false);
    let (handle, mut actor, root, turn, target) = actor(&fixture).await;
    start_barrier(&mut actor, &turn, &target);
    fixture.probe.started.notified().await;
    actor
        .service
        .finish_agent_turn("%1", &turn.turn_id, mez_agent::AgentTurnState::Interrupted)
        .unwrap();
    actor
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement-native", 0)
        .unwrap();
    let replacement = actor
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_message_incarnation
        .snapshot_identity();
    let runner = tokio::spawn(actor.run());
    fixture.probe.release();
    await_idle(&handle, &root, &fixture, 2).await;
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    let id = AgentId::opaque(turn.agent_id).unwrap();
    assert!(
        exited
            .service
            .message_service()
            .native_continuity_is_recorded(&id)
    );
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let fact = store.read_native_continuity(id.as_str()).unwrap().unwrap();
    assert_eq!(fact.snapshot.native.shell_incarnation, replacement);
    assert_eq!(
        fact.snapshot.native.owner.conversation_id(),
        "replacement-native"
    );
    assert_eq!(exited.service.message_service().next_message_sequence(), 1);
    assert_eq!(fixture.disk(), (2, 3));
}

/// Real postcommit worker loss retains the original journal input through actual
/// join and backoff. It cannot open source admission or allocate a native request
/// before exact original reconciliation, and it never writes another generation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_barrier_reconciles_original_journal_after_worker_loss() {
    let fixture = Fixture::new(true);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let (handle, mut actor, root, turn, target) = actor(&fixture).await;
    start_barrier(&mut actor, &turn, &target);
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityRetryAt(_)
    ));
    assert!(
        !actor
            .service
            .message_service()
            .native_continuity_is_recorded(&AgentId::opaque(turn.agent_id.clone()).unwrap())
    );
    let committed = fixture.disk();
    let runner = tokio::spawn(actor.run());
    await_idle(&handle, &root, &fixture, 2).await;
    assert_eq!(fixture.disk(), committed);
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert!(
        exited
            .service
            .message_service()
            .pending_native_continuity()
            .is_none()
    );
    assert_eq!(
        exited.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
}

/// Shutdown owns the actual native journal worker even when no message body was
/// reserved and its direct caller was rejected as busy. It cannot retire the
/// root or return the service while the physical metadata operation is blocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_shutdown_drains_native_journal_before_returning_service() {
    let fixture = Fixture::new(false);
    let (handle, mut actor, _, turn, target) = actor(&fixture).await;
    start_barrier(&mut actor, &turn, &target);
    fixture.probe.started.notified().await;
    let runner = tokio::spawn(actor.run());
    let shutdown_handle = handle.clone();
    let shutdown = tokio::spawn(async move { shutdown_handle.shutdown().await });
    while !handle.sender.closed.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    assert!(!runner.is_finished());
    assert!(!shutdown.is_finished());
    fixture.probe.release();
    shutdown.await.unwrap().unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(exited.service.message_service().next_message_sequence(), 1);
    assert_eq!(fixture.disk(), (1, 2));
}
