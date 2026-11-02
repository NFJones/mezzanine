//! Actual actor/root settlement of original tracked native request metadata.
//! Genuine native prompt/conversation setup is process-free; no provider runs and
//! no action permission or native result-publication proof is fabricated here.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
};
use mez_agent::AgentTurnRecord;
use tokio::sync::oneshot;

/// Creates a real native conversation/turn and subscribed external peer, then
/// registers the root before its first tracked claim. No provider task is run.
pub(super) async fn actor_fixture(
    fixture: &Fixture,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    AgentTurnRecord,
    SenderIdentity,
) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.replace_config_layers(vec![ConfigLayer{name:"tracked-native-storage".into(),path:None,
        format:ConfigFormat::Toml,scope:ConfigScope::Primary,trusted:true,
        text:"[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"native\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\n[permissions]\napproval_policy=\"ask\"\nsandbox=\"policy-only\"\n".into()}]).unwrap();
    service
        .session_mut_for_tests()
        .set_pane_live_state("%1", false)
        .unwrap();
    let primary = service
        .attach_primary(
            "parent",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "coordinate one stored native request")
        .unwrap();
    let turn = service.agent_turn_ledger().turns().last().unwrap().clone();
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", vec![]);
    service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root.clone()).unwrap();
    (handle, actor, root, turn, target)
}

/// Original native semantic body; caller/current discovery is trusted setup.
fn source(
    actor: &AsyncRuntimeSessionActor,
    turn: &AgentTurnRecord,
    target: &SenderIdentity,
) -> Envelope {
    let sender = actor
        .service
        .message_service()
        .registered_identity(&mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap())
        .unwrap()
        .clone();
    Envelope {
        protocol: "mmp/1",
        id: format!("{}:native-storage", turn.turn_id),
        message_type: "send".into(),
        time: "original-native-time".into(),
        sender,
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: Some(turn.turn_id.clone()),
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "native original payload".repeat(20_000),
        extension_fields: vec![],
    }
}

/// No accepted native request or delivery can appear before its actual physical
/// commit. The original caller receipt and original payload publish together;
/// mutable telemetry after reservation cannot make adoption reselect the sender.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_actor_adopts_original_runtime_metadata_only_after_actual_commit() {
    let fixture = Fixture::new(false);
    let (handle, mut actor, _, turn, target) = actor_fixture(&fixture).await;
    let envelope = source(&actor, &turn, &target);
    let (reply, mut response) = oneshot::channel();
    actor
        .start_tracked_native_message(&turn, envelope, MessageScope::Session, 20, reply)
        .unwrap();
    fixture.probe.started.notified().await;
    assert!(matches!(
        response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(actor.service.tracked_message_request_usage().0.0, 0);
    assert!(actor.service.tracked_message_request_usage().0.1 > 0);
    actor
        .service
        .message_service_mut()
        .update_agent_objective(
            &mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            Some("changed after selected original"),
            21,
        )
        .unwrap();
    assert!(
        actor
            .service
            .retire_runtime_message_transfers_for_restore()
            .is_err()
    );
    let runner = tokio::spawn(actor.run());
    assert!(handle.lifecycle_state().await.is_ok());
    assert!(
        handle
            .message_fanout_ready_for(target.agent_id.clone(), 30, 10)
            .await
            .unwrap()
            .is_none()
    );
    fixture.probe.release();
    let delivery = response.await.unwrap().unwrap();
    assert!(delivery.accepted);
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(exited.service.tracked_message_request_usage().0.0, 1);
    let batch = exited
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, 30, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].envelope.time, "original-native-time");
    assert_ne!(
        batch.messages[0].envelope.sender.objective.as_deref(),
        Some("changed after selected original")
    );
}

/// Losing the native receipt observer cannot abandon an issued request. Graceful
/// shutdown retains actual worker and all paired runtime/lower owners until join
/// and metadata adoption, preserving exactly one canonical occurrence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_actor_keeps_original_after_observer_loss_through_graceful_shutdown() {
    let fixture = Fixture::new(false);
    let (handle, mut actor, _, turn, target) = actor_fixture(&fixture).await;
    let envelope = source(&actor, &turn, &target);
    let (reply, response) = oneshot::channel();
    actor
        .start_tracked_native_message(&turn, envelope, MessageScope::Session, 20, reply)
        .unwrap();
    fixture.probe.started.notified().await;
    drop(response);
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
    assert_eq!(exited.service.tracked_message_request_usage().0.0, 1);
    assert_eq!(
        exited
            .service
            .message_service()
            .receive_subscribed(&target.agent_id, 30, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        !exited
            .service
            .message_service()
            .has_unpublished_acceptance()
    );
}

/// An actual postcommit worker panic reconciles the same tracked occurrence and
/// native metadata after real join/backoff, without allocating another request
/// generation or reinserting canonical mail from a duplicate completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_actor_reconciles_actual_postcommit_worker_loss_once() {
    let fixture = Fixture::new(true);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let (handle, mut actor, root, turn, target) = actor_fixture(&fixture).await;
    let envelope = source(&actor, &turn, &target);
    let (reply, response) = oneshot::channel();
    actor
        .start_tracked_native_message(&turn, envelope, MessageScope::Session, 20, reply)
        .unwrap();
    let runner = tokio::spawn(actor.run());
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RetryAt(_)
    ));
    let committed = fixture.disk();
    assert!(response.await.unwrap().unwrap().accepted);
    assert_eq!(fixture.disk(), committed);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(exited.service.tracked_message_request_usage().0.0, 1);
    assert_eq!(
        exited
            .service
            .message_service()
            .receive_subscribed(&target.agent_id, 30, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
}

/// Shutdown of an actually failed native stage qualifies noncommit through the
/// original durable abort. Only then may both lower/request and upper native
/// reservations release; no accepted receipt or provider effect is fabricated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_actor_qualified_abort_releases_all_original_metadata() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, root, turn, target) = actor_fixture(&fixture).await;
    let sql = rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_native_marker BEFORE INSERT ON records WHEN NEW.key LIKE 'a1:%:commit' BEGIN SELECT RAISE(ABORT,'native fixture marker failure'); END;").unwrap();
    let envelope = source(&actor, &turn, &target);
    let (reply, response) = oneshot::channel();
    actor
        .start_tracked_native_message(&turn, envelope, MessageScope::Session, 20, reply)
        .unwrap();
    let runner = tokio::spawn(actor.run());
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RetryAt(_)
    ));
    let shutdown_handle = handle.clone();
    let shutdown = tokio::spawn(async move { shutdown_handle.shutdown().await });
    assert!(response.await.unwrap().is_err());
    shutdown.await.unwrap().unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        exited.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    assert!(
        !exited
            .service
            .message_service()
            .has_unpublished_acceptance()
    );
    assert!(
        exited
            .service
            .message_service()
            .receive_subscribed(&target.agent_id, 30, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(fixture.disk().1, 1);
}
