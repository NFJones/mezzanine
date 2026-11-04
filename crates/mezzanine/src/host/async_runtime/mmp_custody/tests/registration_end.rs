//! Actual actor/root end retention, loss, replacement and graceful shutdown.

use super::*;
use mez_core::ids::AgentId;

/// Actual guarded native journal through the actor, without executing providers.
async fn scene(
    fixture: &Fixture,
) -> (
    crate::host::async_runtime::AsyncRuntimeSessionHandle,
    crate::host::async_runtime::AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    mez_agent::AgentTurnRecord,
    SenderIdentity,
) {
    let (handle, mut actor, root, turn, target) = native_actor::actor_fixture(fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    fixture.probe.started.notified().await;
    (handle, actor, root, turn, target)
}

/// Same bounded source decoder policy used by actual original recovery.
fn limits() -> mez_agent::messaging::MessageAcceptanceDecodeLimits {
    mez_agent::messaging::MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// Reads only descriptive evidence; none of it can recreate a live registration.
fn store(fixture: &Fixture) -> MmpCustodyStore {
    MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default()).unwrap()
}

/// A real accepted original survives explicit sender retirement and the actor's
/// o1 drain. Actual runtime identity registration for the still-live shell gets
/// a fresh namespace, whose n1 follows the original end. Mail is not settled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_end_actor_preserves_original_before_replacement_native_publication() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, target) = scene(&fixture).await;
    let id = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let original = native_actor::source(&actor, &turn, &target);
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .start_tracked_native_message(
            &turn,
            original,
            MessageScope::Session,
            crate::runtime::current_unix_millis(),
            reply,
        )
        .unwrap();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    assert!(response.await.unwrap().unwrap().accepted);
    let before = store(&fixture).inventory_custody(limits()).unwrap();
    assert_eq!(before.occurrences.len(), 1);
    assert!(
        actor
            .service
            .message_service_mut()
            .retire_agent_identity(&id)
    );
    let restore_error = actor
        .service
        .ensure_runtime_message_transfers_settled_for_restore()
        .err()
        .unwrap();
    assert!(
        restore_error
            .to_string()
            .contains("original MMP storage work must settle before restore"),
        "{restore_error}"
    );
    actor
        .service
        .ensure_runtime_message_identity(
            &turn.agent_id,
            None,
            "agent",
            &[],
            crate::runtime::current_unix_millis(),
        )
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert!(
        !actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    let ended = store(&fixture).inventory_custody(limits()).unwrap();
    assert_eq!(ended.registration_end_keys.len(), 1);
    assert_eq!(ended.occurrences[0].digest, before.occurrences[0].digest);
    assert!(
        ended.expiry_keys.is_empty()
            && ended.acknowledgement_keys.is_empty()
            && ended.release_keys.is_empty()
            && ended.consumption_keys.is_empty()
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    let after = store(&fixture).inventory_custody(limits()).unwrap();
    assert_eq!(after.native_lifetime_keys.len(), 2);
    assert_eq!(after.registration_end_keys.len(), 1);
    assert_eq!(
        actor
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
}

/// A native worker can finish after the original allocation was removed. Its
/// immutable publication survives and the separately retained end intent is the
/// next actor operation, before any successor native observation is journaled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_end_actor_retains_removal_during_original_native_worker() {
    let fixture = Fixture::new(false);
    let (_, mut actor, _, turn, _) = native_actor::actor_fixture(&fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    actor.poll_message_custody_for_tests();
    fixture.probe.started.notified().await;
    let id = AgentId::opaque(turn.agent_id).unwrap();
    assert!(
        actor
            .service
            .message_service_mut()
            .retire_agent_identity(&id)
    );
    fixture.probe.release();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(
        !actor
            .service
            .message_service()
            .native_continuity_is_recorded(&id)
    );
    assert!(
        actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert_eq!(fixture.disk(), (2, 3));
    assert_eq!(
        store(&fixture)
            .inventory_custody(limits())
            .unwrap()
            .registration_end_keys
            .len(),
        1
    );
}

/// Actual post-commit worker panic retains the original input and first plan.
/// The root reconciles o1 after real backoff and join, without a second commit
/// or early release of the lower intent/scratch charge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_end_actor_reconciles_lost_positive_after_actual_join() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, _) = scene(&fixture).await;
    let id = AgentId::opaque(turn.agent_id).unwrap();
    actor
        .service
        .message_service_mut()
        .retire_agent_identity(&id);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    actor.poll_message_custody_for_tests();
    assert!(matches!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndRetryAt(_)
    ));
    assert_eq!(fixture.disk(), (2, 3));
    assert!(
        actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    assert!(
        actor
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes
            >= 2 * 1024 * 1024
    );
    assert!(!actor.message_custody_wake_is_ready_for_tests());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert_eq!(fixture.disk(), (2, 3));
    assert!(
        !actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
}

/// Metadata capacity refusal has no physical worker and cannot spin the actor.
/// The actual ended allocation remains retained until external capture owners
/// release their charges; the next due attempt then journals the same end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_end_actor_capacity_refusal_backs_off_without_losing_intent() {
    let fixture = Fixture::new(true);
    let (_, mut actor, root, turn, _) = scene(&fixture).await;
    let id = AgentId::opaque(turn.agent_id).unwrap();
    let held = (0..3)
        .map(|_| {
            actor
                .service
                .message_service()
                .prepare_native_continuity(root.storage_scope(), &id)
                .unwrap()
        })
        .collect::<Vec<_>>();
    actor
        .service
        .message_service_mut()
        .retire_agent_identity(&id);
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    actor.poll_message_custody_for_tests();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
    assert!(
        actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    assert!(!actor.message_custody_wake_is_ready_for_tests());
    drop(held);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert!(
        !actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
}

/// Stop must retain the actor/root until an actually blocked end worker joins
/// and its original lower intent adopts. The public shutdown reply cannot win
/// this gate or discard the original end just because the registration is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_end_actor_shutdown_drains_blocked_original_end() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, turn, _) = scene(&fixture).await;
    *fixture.probe.gate.0.lock().unwrap() = false;
    let id = AgentId::opaque(turn.agent_id).unwrap();
    actor
        .service
        .message_service_mut()
        .retire_agent_identity(&id);
    actor.poll_message_custody_for_tests();
    fixture.probe.started.notified().await;
    let runner = tokio::spawn(actor.run());
    let shutdown_handle = handle.clone();
    let shutdown = tokio::spawn(async move { shutdown_handle.shutdown().await });
    while !handle.sender.closed.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    assert!(!shutdown.is_finished());
    assert!(!runner.is_finished());
    fixture.probe.release();
    shutdown.await.unwrap().unwrap();
    let exited = runner.await.unwrap();
    assert!(
        !exited
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    assert_eq!(fixture.disk(), (2, 3));
}

/// A foreign canonical service cannot steal the original joined end result.
/// The root retains its real positive until the actual actor/service adopts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_end_root_refuses_foreign_adopter_without_losing_positive() {
    let fixture = Fixture::new(true);
    let (_, mut actor, root, turn, _) = scene(&fixture).await;
    let id = AgentId::opaque(turn.agent_id).unwrap();
    actor
        .service
        .message_service_mut()
        .retire_agent_identity(&id);
    actor.poll_message_custody_for_tests();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert!(
        root.adopt_registration_end(&mut MessageService::default())
            .is_err()
    );
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert!(
        actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert!(
        !actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
}
