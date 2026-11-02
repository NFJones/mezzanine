//! Actual raw actor sends across physical storage gates, faults and lost observers.
//! A real hello owns its fresh raw identity; pending sources are persisted through
//! the production root adapter, and replies are inspected as actual framed MMP.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
};
use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
use mez_agent::messaging::MessageConnection;
use tokio::sync::watch;

/// Builds a live actor with a real private store and one subscribed raw target.
async fn actor_fixture(
    fixture: &Fixture,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    SenderIdentity,
) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
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
    (handle, actor, root, target)
}

/// Actual hello registers the sender through ordinary audited identity dispatch.
/// Its lease receiver remains independent of later per-frame reply cancellation.
pub(super) async fn hello(
    handle: &AsyncRuntimeSessionHandle,
) -> (
    MessageConnection,
    watch::Sender<Option<mez_core::ids::AgentId>>,
    watch::Receiver<Option<mez_core::ids::AgentId>>,
) {
    let (owner, identity) = watch::channel(None);
    let welcome = handle
        .handle_message_input_with_owner(
            encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"original-raw"}"#),
            2 * 1024 * 1024,
            MessageConnection::default(),
            10,
            Some(owner.clone()),
        )
        .await
        .unwrap();
    assert_eq!(body(&welcome.output)["type"], "welcome");
    assert_eq!(*identity.borrow(), welcome.connection.agent_id);
    (welcome.connection, owner, identity)
}

/// A valid original wire payload larger than a single 256KiB store transaction.
fn send(connection: &MessageConnection, target: &SenderIdentity, id: &str) -> Vec<u8> {
    encode_mmp_body(
        &serde_json::json!({
            "protocol":"mmp/1","type":"send","id":id,"time":"original-wire-time",
            "scope":"session","sender":{"agent_id":connection.agent_id.as_ref().unwrap().as_str()},
            "recipient":{"agent_id":target.agent_id.as_str()},"ttl_ms":null,"correlation_id":null,
            "content_type":"text/plain; charset=utf-8","payload":"original".repeat(50_000)
        })
        .to_string(),
    )
}

/// Decodes only actual framed actor replies, never a fabricated acceptance label.
pub(super) fn body(frame: &[u8]) -> serde_json::Value {
    serde_json::from_str(&decode_mmp_frame(frame, 2 * 1024 * 1024).unwrap().0).unwrap()
}

/// Observes the retained actual join failure; does not retry or mutate root work.
async fn failed(root: &MmpCustodyRoot) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::RetryAt(_)
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// Raw success and delivery must wait for actual storage commit, while unrelated
/// identity/lifecycle requests remain responsive. Busy sends reserve no sequence;
/// a mutable objective update cannot rewrite or reject the selected original.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_actor_reserves_once_and_replies_only_after_original_storage_adoption() {
    let fixture = Fixture::new(false);
    let (handle, actor, _, target) = actor_fixture(&fixture).await;
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _identity) = hello(&handle).await;
    let input = send(&connection, &target, "first");
    let consumed = input.len();
    let sender = handle.clone();
    let caller_connection = connection.clone();
    let caller_owner = owner.clone();
    let caller = tokio::spawn(async move {
        sender
            .handle_message_input_with_owner(
                input,
                2 * 1024 * 1024,
                caller_connection,
                20,
                Some(caller_owner),
            )
            .await
    });
    fixture.probe.started.notified().await;
    assert!(!caller.is_finished());
    assert_eq!(fixture.disk(), (0, 0));
    assert!(handle.lifecycle_state().await.is_ok());
    assert!(
        handle
            .message_fanout_ready_for(target.agent_id.clone(), 20, 10)
            .await
            .unwrap()
            .is_none()
    );
    let busy = handle
        .handle_message_input_with_owner(
            send(&connection, &target, "second"),
            2 * 1024 * 1024,
            connection.clone(),
            21,
            Some(owner.clone()),
        )
        .await
        .unwrap();
    assert_eq!(body(&busy.output)["type"], "error");
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    handle.handle_message_input_with_owner(encode_mmp_body(
        r#"{"protocol":"mmp/1","type":"presence","objective":"changed while original write is blocked"}"#),
        4096, connection.clone(), 22, Some(owner.clone())).await.unwrap();
    fixture.probe.release();
    let accepted = caller.await.unwrap().unwrap();
    assert_eq!(accepted.consumed, consumed);
    assert_eq!(body(&accepted.output)["message_id"], "first");
    assert_eq!(body(&accepted.output)["type"], "ack");
    let selected = handle
        .message_fanout_ready_for(target.agent_id.clone(), 30, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(selected.batch.batch.messages.len(), 1);
    assert_eq!(
        selected.batch.batch.messages[0].envelope.sender.objective,
        None
    );
    let second = handle
        .handle_message_input_with_owner(
            send(&connection, &target, "second"),
            2 * 1024 * 1024,
            connection,
            30,
            Some(owner),
        )
        .await
        .unwrap();
    assert_eq!(body(&second.output)["type"], "ack");
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    let pending = exited
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, 30, 10)
        .unwrap();
    assert_eq!(
        pending
            .messages
            .iter()
            .map(|m| m.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(pending.messages[0].envelope.time, "original-wire-time");
    assert_eq!(
        pending.messages[0].envelope.payload,
        "original".repeat(50_000)
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
}

/// A cancelled send caller and explicit raw lease retirement cannot erase an
/// issued original. Graceful shutdown retains the original until actual commit;
/// adoption preserves its historical sender without reviving raw registration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_actor_cancelled_sender_retains_acceptance_through_shutdown_and_retirement() {
    let fixture = Fixture::new(false);
    let (handle, actor, _, target) = actor_fixture(&fixture).await;
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _identity) = hello(&handle).await;
    let sender_id = connection.agent_id.clone().unwrap();
    let input = send(&connection, &target, "cancelled-observer");
    let sender = handle.clone();
    let caller = tokio::spawn(async move {
        sender
            .handle_message_input_with_owner(input, 2 * 1024 * 1024, connection, 20, Some(owner))
            .await
    });
    fixture.probe.started.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    handle
        .retire_message_connection(sender_id.clone())
        .await
        .unwrap();
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
    assert!(
        exited
            .service
            .message_service()
            .registered_identity(&sender_id)
            .is_none()
    );
    let mail = exited
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, 30, 10)
        .unwrap();
    assert_eq!(mail.messages.len(), 1);
    assert_eq!(mail.messages[0].envelope.id, "cancelled-observer");
    assert_eq!(mail.messages[0].envelope.sender.agent_id, sender_id);
    assert!(
        !exited
            .service
            .message_service()
            .has_unpublished_acceptance()
    );
}

/// Actual commit-before-worker-panic must reconcile the same immutable source.
/// A later exact raw retry returns its qualified live receipt with no new write,
/// record or occurrence, rather than running the original request again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_actor_reconciles_actual_postcommit_worker_loss_and_exact_duplicate() {
    let fixture = Fixture::new(true);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let (handle, actor, root, target) = actor_fixture(&fixture).await;
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _identity) = hello(&handle).await;
    let input = send(&connection, &target, "lost-positive");
    let sender = handle.clone();
    let original = input.clone();
    let original_connection = connection.clone();
    let original_owner = owner.clone();
    let caller = tokio::spawn(async move {
        sender
            .handle_message_input_with_owner(
                original,
                2 * 1024 * 1024,
                original_connection,
                20,
                Some(original_owner),
            )
            .await
    });
    failed(&root).await;
    let committed = fixture.disk();
    assert!(committed.0 > 2);
    assert!(!caller.is_finished());
    assert_eq!(body(&caller.await.unwrap().unwrap().output)["type"], "ack");
    assert_eq!(fixture.disk(), committed);
    let duplicate = handle
        .handle_message_input_with_owner(input, 2 * 1024 * 1024, connection, 30, Some(owner))
        .await
        .unwrap();
    assert_eq!(body(&duplicate.output)["type"], "ack");
    assert_eq!(fixture.disk(), committed);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    handle.shutdown().await.unwrap();
    assert_eq!(
        runner
            .await
            .unwrap()
            .service
            .message_service()
            .snapshot_state()
            .next_sequence,
        2
    );
}

/// A genuine SQL marker failure leaves the original unaccepted. Removing the
/// trigger lets the actor retry its retained source/plan after monotonic backoff;
/// no caller resend or new sequence is needed to produce exactly one acceptance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_actor_storage_recovery_commits_original_without_caller_resend() {
    let fixture = Fixture::new(true);
    let (handle, actor, root, target) = actor_fixture(&fixture).await;
    let sql = rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE 'a1:%:commit' BEGIN SELECT RAISE(ABORT,'fixture raw marker failure'); END;").unwrap();
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _identity) = hello(&handle).await;
    let input = send(&connection, &target, "original-after-recovery");
    let sender = handle.clone();
    let caller = tokio::spawn(async move {
        sender
            .handle_message_input_with_owner(input, 2 * 1024 * 1024, connection, 20, Some(owner))
            .await
    });
    failed(&root).await;
    assert!(!caller.is_finished());
    assert!(
        handle
            .message_fanout_ready_for(target.agent_id.clone(), 30, 10)
            .await
            .unwrap()
            .is_none()
    );
    sql.execute_batch("DROP TRIGGER fail_marker;").unwrap();
    assert_eq!(body(&caller.await.unwrap().unwrap().output)["type"], "ack");
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        exited
            .service
            .message_service()
            .snapshot_state()
            .next_sequence,
        2
    );
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

/// Shutdown of an actually failed partial stage must preserve a committed source
/// or qualify noncommit through original cleanup. This real marker failure has
/// no acceptance, so the actor releases only after durable abort reconciliation,
/// returns a wire rejection and leaves no hidden accepted/issued lower source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_actor_shutdown_reconciles_uncommitted_original_before_rejection() {
    let fixture = Fixture::new(true);
    let (handle, actor, root, target) = actor_fixture(&fixture).await;
    let sql = rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE 'a1:%:commit' BEGIN SELECT RAISE(ABORT,'fixture shutdown marker failure'); END;").unwrap();
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _identity) = hello(&handle).await;
    let input = send(&connection, &target, "uncommitted");
    let sender = handle.clone();
    let caller = tokio::spawn(async move {
        sender
            .handle_message_input_with_owner(input, 2 * 1024 * 1024, connection, 20, Some(owner))
            .await
    });
    failed(&root).await;
    assert!(!caller.is_finished());
    let shutdown_handle = handle.clone();
    let shutdown = tokio::spawn(async move { shutdown_handle.shutdown().await });
    let response = caller.await.unwrap().unwrap();
    assert_eq!(body(&response.output)["type"], "error");
    assert_eq!(
        body(&response.output)["error"]["delivery_status"],
        "rejected"
    );
    shutdown.await.unwrap().unwrap();
    let exited = runner.await.unwrap();
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
    assert_eq!(fixture.disk().1, 1, "only charged abort descriptor remains");
    assert_eq!(
        sql.query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'a1:%:commit'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

/// A genuinely committed source under one root must not gain durable replay
/// authority from a different empty root attached to the same live service.
/// Equal public sender/message data and a qualified cache entry are insufficient;
/// actual original source reconciliation is required before accepting that reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_actor_foreign_root_cannot_upgrade_cached_original_acceptance() {
    let original = Fixture::new(true);
    let original_root = original.root();
    assert_eq!(
        original_root.observe().await.unwrap(),
        CustodyWorkerStatus::Idle
    );
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", vec![]);
    service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "raw", vec![]);
    let connection = MessageConnection {
        agent_id: Some(sender.agent_id),
        delivery_cursor: None,
    };
    let input = send(&connection, &target, "original-root");
    let request = mez_agent::messaging::parse_mmp_storage_request(
        &decode_mmp_frame(&input, 2 * 1024 * 1024).unwrap().0,
        service.message_service(),
        &connection,
    )
    .unwrap()
    .unwrap();
    let mez_agent::messaging::MmpStoragePreparation::Acceptance(ticket) = request
        .prepare(
            service.message_service_mut(),
            original_root.storage_scope(),
            20,
            4096,
            512 * 1024,
        )
        .unwrap()
    else {
        panic!("original must reserve")
    };
    original_root
        .start(service.message_service_mut(), &ticket, None)
        .unwrap();
    assert_eq!(
        original_root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    original_root
        .adopt(service.message_service_mut(), None)
        .unwrap();
    let committed = original.disk();
    let foreign = Fixture::new(true);
    let foreign_root = foreign.root();
    assert_eq!(
        foreign_root.observe().await.unwrap(),
        CustodyWorkerStatus::Idle
    );
    assert_ne!(original_root.storage_scope(), foreign_root.storage_scope());
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(foreign_root).unwrap();
    let runner = tokio::spawn(actor.run());
    let response = handle
        .handle_message_input(input, 2 * 1024 * 1024, connection, 30)
        .await
        .unwrap();
    assert_eq!(body(&response.output)["type"], "error");
    assert_eq!(
        body(&response.output)["error"]["delivery_status"],
        "rejected"
    );
    assert_eq!(foreign.disk(), (0, 0));
    assert_eq!(original.disk(), committed);
    assert_eq!(foreign.probe.attempts.load(Ordering::SeqCst), 0);
    handle.shutdown().await.unwrap();
    runner.await.unwrap();
}
