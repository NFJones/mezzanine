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

/// A full store must still permit the original recipient ACK to use its
/// reserved metadata headroom while a later original is waiting for capacity.
/// The raw caller and worker remain owned until orderly shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_store_must_not_block_ack_behind_unaccepted_raw_send() {
    exercise_capacity_ack(false, false, false, false).await;
}

/// A real worker lost after the guarded ACK commits must reconcile that same
/// ACK and retain the original send across the actual join and retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_store_ack_lost_reply_keeps_original_send_and_reconciles() {
    exercise_capacity_ack(true, false, false, false).await;
}

/// An authenticated raw recipient ACK uses the same guarded lane and retains
/// its original connection while the separate send remains pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_store_raw_ack_can_pass_an_unaccepted_send() {
    exercise_capacity_ack(false, true, false, false).await;
}

/// A raw ACK whose actual worker is lost after commit reconciles the same
/// original before either the socket response or suspended send is published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_store_raw_ack_lost_reply_keeps_original_send() {
    exercise_capacity_ack(true, true, false, false).await;
}

/// Repeated transient post-commit position-read failures must be retried without
/// discarding either the real ACK commit or the suspended original send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_store_ack_recovers_transient_anchor_failure() {
    exercise_capacity_ack(false, false, true, false).await;
}

/// Losing the actual position-refresh worker must reconcile the exact ACK and
/// retain the original send without writing a second ACK record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_store_ack_recovers_lost_anchor_worker() {
    exercise_capacity_ack(false, false, false, true).await;
}

/// Exercises the real actor, storage and original wire input with an optional
/// physical ACK-worker loss after its committed transaction.
async fn exercise_capacity_ack(
    lose_ack: bool,
    raw_ack: bool,
    fail_anchor: bool,
    lose_refresh: bool,
) {
    let fixture = Fixture::new(true);
    let ack_probe = super::super::acknowledgement::tests::Probe::new(true);
    if lose_ack {
        ack_probe.lose_next_commit();
    }
    if fail_anchor {
        ack_probe.fail_anchors(2);
    }
    if lose_refresh {
        ack_probe.fail_anchors(1);
        ack_probe.lose_next_anchor_refresh();
    }
    let root = MmpCustodyRoot::construct(
        Destination {
            clock_source: Default::default(),
            root: fixture.path.clone(),
            owner: "original-session".into(),
            limits: CustodyStoreLimits {
                records: 4,
                ..CustodyStoreLimits::default()
            },
        },
        Handle::current(),
        Some(fixture.probe.clone()),
        Some(ack_probe.clone()),
    );
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
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _identity) = hello(&handle).await;
    let raw_recipient = if raw_ack {
        Some(hello(&handle).await)
    } else {
        None
    };
    let target_id = raw_recipient
        .as_ref()
        .and_then(|(connection, _, _)| connection.agent_id.clone())
        .unwrap_or_else(|| target.agent_id.clone());
    let small = |id: &str| {
        encode_mmp_body(&serde_json::json!({
        "protocol":"mmp/1", "type":"send", "id":id, "time":"original",
        "scope":"session", "sender":{"agent_id":connection.agent_id.as_ref().unwrap().as_str()},
        "recipient":{"agent_id":target_id.as_str()}, "ttl_ms":null, "correlation_id":null,
        "content_type":"text/plain; charset=utf-8", "payload":"small"
    }).to_string())
    };
    let first = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = handle
                .handle_message_input_with_owner(
                    small("first"),
                    4096,
                    connection.clone(),
                    20,
                    Some(owner.clone()),
                )
                .await
                .unwrap();
            if body(&reply.output)["type"] == "ack" {
                break reply;
            }
            assert_eq!(
                body(&reply.output)["error"]["code"],
                "capacity",
                "unexpected first-send response: {}",
                body(&reply.output)
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(body(&first.output)["type"], "ack");
    let batch = handle
        .message_fanout_ready_for(target_id.clone(), 30, 10)
        .await
        .unwrap()
        .unwrap()
        .batch;
    let second = small("second");
    let sender = handle.clone();
    let mut caller = tokio::spawn(async move {
        sender
            .handle_message_input_with_owner(second, 4096, connection, 31, Some(owner))
            .await
    });
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
    assert_eq!(fixture.disk().1, 3);
    {
        let mut state = root.0.state.lock().unwrap();
        let State::Joined { retry_at, .. } = &mut *state else {
            panic!("capacity failure was not retained")
        };
        *retry_at = Instant::now() + Duration::from_secs(2);
    }
    let recipient = batch.recipient.clone();
    let ack = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some((connection, owner, _lease)) = raw_recipient.as_ref() {
                let frame = encode_mmp_body(r#"{"protocol":"mmp/1","type":"ack","sequence":1}"#);
                let response = handle
                    .handle_message_input_with_owner(
                        frame,
                        4096,
                        connection.clone(),
                        32,
                        Some(owner.clone()),
                    )
                    .await
                    .unwrap();
                if body(&response.output)["type"] == "ack" {
                    assert_eq!(body(&response.output)["last_sequence"], 1);
                    break;
                }
            } else if handle
                .acknowledge_message_fanout(batch.clone())
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if ack.is_ok() && fail_anchor {
        assert_eq!(
            ack_probe.physical_attempts(),
            1,
            "position retries must not rewrite the committed ACK"
        );
        assert_eq!(fixture.disk().1, 4);
    }
    if ack.is_ok() && lose_refresh {
        assert_eq!(
            ack_probe.physical_attempts(),
            2,
            "lost refresh must reconcile the same ACK once"
        );
        assert_eq!(fixture.disk().1, 4);
    }
    if ack.is_err() && (fail_anchor || lose_refresh) {
        runner.abort();
        caller.abort();
        let _ = runner.await;
        let _ = caller.await;
        panic!("a transient position-read failure stranded the committed ACK");
    }
    if ack.is_ok() {
        // The production store policy is immutable. This test-only change
        // models capacity becoming available without issuing another store
        // transaction, and verifies the retained original is not regenerated.
        provide_record_capacity(&root, 7).await;
        let accepted = tokio::time::timeout(Duration::from_secs(10), &mut caller)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(body(&accepted.output)["type"], "ack");
        assert_eq!(body(&accepted.output)["message_id"], "second");
        assert_eq!(fixture.disk().1, 7);
    }
    let shutdown = handle.shutdown().await;
    if ack.is_err() {
        let _ = caller.await;
    }
    let mut exit = runner.await.unwrap();
    shutdown.unwrap();
    let messages = exit.service.message_service_mut();
    if ack.is_ok() {
        let snapshot = messages.snapshot_state().unwrap();
        assert!(
            snapshot
                .retained_messages
                .iter()
                .chain(snapshot.custody_messages.as_deref().unwrap_or(&[]))
                .any(|row| row.sequence == 2 && row.envelope.id == "second")
        );
    }
    if ack.is_err() {
        let ticket = messages
            .prepare_acknowledgement(&recipient, 1, 512 * 1024)
            .unwrap()
            .unwrap();
        root.start_acknowledgement(messages, &ticket).unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AcknowledgementAwaitingAdoption
        );
        assert_eq!(
            root.adopt_acknowledgement(messages).unwrap().last_sequence,
            1
        );
    }
    if ack.is_err() {
        assert_eq!(
            fixture.disk().1,
            4,
            "control ACK must actually fit in the reserved row"
        );
    }
    assert!(
        ack.is_ok(),
        "accepted first message ACK was blocked despite spare store row: {ack:?}"
    );
}

/// Decodes only actual framed actor replies, never a fabricated acceptance label.
pub(super) fn body(frame: &[u8]) -> serde_json::Value {
    serde_json::from_str(&decode_mmp_frame(frame, 2 * 1024 * 1024).unwrap().0).unwrap()
}

/// The durable disk ledger must accept more unread payload than its resident
/// cache can hold, without acknowledging either recipient or raising the
/// memory limit. This drives actual spill through the raw actor and real root;
/// disk availability alone is insufficient while lower custody pins every
/// payload. Cache accounting includes live allocations and read reservations;
/// trusted restart delivery remains a separate integration requirement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_actor_spills_unread_payload_beyond_resident_cache() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, target) = actor_fixture(&fixture).await;
    actor
        .enable_message_payload_spill(mez_agent::messaging::MessageStorageLimits::default())
        .unwrap();
    let payload_usage = actor.service.message_service().payload_observer().unwrap();
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _lease) = hello(&handle).await;
    let replies = send_unread_pair(&handle, &connection, &owner, &target, &payload_usage).await;
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        replies[0]["type"], "ack",
        "first source must fit: {}",
        replies[0]
    );
    assert_eq!(
        replies[1]["type"], "ack",
        "disk-backed custody must spill rather than retain every payload: {}",
        replies[1]
    );
    let usage = exited
        .service
        .message_service()
        .storage_usage()
        .unwrap()
        .unwrap();
    assert_eq!(usage.pending_messages, 2);
    assert_eq!(usage.pending_payload_bytes, 2 * 768 * 1024);
    assert_eq!(usage.reserved_messages, 0);
    assert_eq!(usage.cache.reserved_allocations, 0);
    assert!(usage.cache.live_payload_bytes <= 1024 * 1024);
    assert!(usage.metadata_bytes <= 8 * 1024 * 1024);
    let commits: i64 = rusqlite::Connection::open(fixture.path.join("custody.sqlite"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE '%:commit'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(commits, 2);
}

/// Sends two actual authenticated wire originals while measuring the shared
/// canonical cache. The connection lease stays with its caller through shutdown.
async fn send_unread_pair(
    handle: &AsyncRuntimeSessionHandle,
    connection: &MessageConnection,
    owner: &watch::Sender<Option<mez_core::ids::AgentId>>,
    target: &SenderIdentity,
    payload_usage: &mez_agent::messaging::MessagePayloadObserver,
) -> Vec<serde_json::Value> {
    let mut replies = Vec::new();
    for id in ["first-spilled", "second-spilled"] {
        let input = encode_mmp_body(&serde_json::json!({
            "protocol":"mmp/1", "type":"send", "id":id, "time":"original",
            "scope":"session", "sender":{"agent_id":connection.agent_id.as_ref().unwrap().as_str()},
            "recipient":{"agent_id":target.agent_id.as_str()}, "ttl_ms":null, "correlation_id":null,
            "content_type":"text/plain; charset=utf-8", "payload":"x".repeat(768 * 1024)
        }).to_string());
        let response = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = handle
                    .handle_message_input_with_owner(
                        input.clone(),
                        2 * 1024 * 1024,
                        connection.clone(),
                        20,
                        Some(owner.clone()),
                    )
                    .await
                    .unwrap();
                let decoded = body(&reply.output);
                if decoded["error"]["message"]
                    != "MMP storage lane busy or unavailable; message not accepted"
                {
                    break decoded;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let cache = payload_usage.snapshot().unwrap();
        assert!(cache.live_payload_bytes + cache.reserved_payload_bytes <= 1024 * 1024);
        assert!(cache.live_allocations + cache.reserved_allocations <= 1000);
        replies.push(response);
    }
    replies
}

/// Actual cold delivery must return each original body and sequence through
/// the public actor handle, persist both recipient ACKs, and release logical
/// pending custody without ever retaining both 768 KiB canonical allocations.
/// This is same-runtime paging; trusted restart binding is a separate gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_actor_reloads_spilled_originals_and_persists_delivery_acks() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, target) = actor_fixture(&fixture).await;
    actor
        .enable_message_payload_spill(mez_agent::messaging::MessageStorageLimits::default())
        .unwrap();
    let usage = actor.service.message_service().payload_observer().unwrap();
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _lease) = hello(&handle).await;
    let replies = send_unread_pair(&handle, &connection, &owner, &target, &usage).await;
    assert!(replies.iter().all(|reply| reply["type"] == "ack"));
    for (index, id) in ["first-spilled", "second-spilled"].into_iter().enumerate() {
        let fanout = tokio::time::timeout(
            Duration::from_secs(10),
            handle.message_fanout_ready_for(target.agent_id.clone(), 30, 64),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(fanout.messages, 1);
        let delivered = &fanout.batch.batch.messages[0];
        assert_eq!(delivered.envelope.id, id);
        assert_eq!(delivered.sequence, index as u64 + 1);
        assert_eq!(delivered.envelope.payload.len(), 768 * 1024);
        assert!(delivered.envelope.payload.bytes().all(|byte| byte == b'x'));
        let measured = usage.snapshot().unwrap();
        assert_eq!(measured.live_allocations, 1);
        assert_eq!(measured.live_payload_bytes, 768 * 1024);
        assert_eq!(measured.reserved_payload_bytes, 0);
        let ack = tokio::time::timeout(
            Duration::from_secs(10),
            handle.acknowledge_message_fanout(fanout.batch),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(ack.last_sequence, index as u64 + 1);
    }
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    let measured = exited
        .service
        .message_service()
        .storage_usage()
        .unwrap()
        .unwrap();
    assert_eq!(measured.pending_messages, 0);
    assert_eq!(measured.pending_payload_bytes, 0);
    assert!(measured.cache.live_payload_bytes <= 1024 * 1024);
    assert_eq!(measured.cache.reserved_payload_bytes, 0);
    let sql = rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap();
    for (pattern, count) in [("a1:%:commit", 2), ("k1:%", 2)] {
        let actual: i64 = sql
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE ?1",
                [pattern],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, count, "physical committed records for {pattern}");
    }
}

/// Host checkpointing must represent actual accepted cold originals by verified
/// store references, without loading bodies, exceeding the canonical cache, or
/// inventing empty envelopes. The saved schema10 data remains nonauthorizing:
/// generic snapshot restoration cannot create a live storage/native owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_actor_host_checkpoint_keeps_cold_originals_as_store_references() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, target) = actor_fixture(&fixture).await;
    actor
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let native = actor
        .service
        .ensure_runtime_message_identity(
            "agent-%1",
            mez_core::ids::PaneId::opaque("%1"),
            "agent",
            &["agent-harness"],
            10,
        )
        .unwrap();
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    let usage = actor.service.message_service().payload_observer().unwrap();
    let shell = actor.service.agent_shell_store().get("%1").unwrap();
    let native_registration = actor
        .service
        .message_service()
        .capture_sender_registration(&native.agent_id)
        .unwrap();
    let native_owner = actor
        .service
        .message_service()
        .native_owner_for_registration(&native_registration, &shell.native_message_incarnation)
        .unwrap()
        .clone();
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _lease) = hello(&handle).await;
    let replies = send_unread_pair(&handle, &connection, &owner, &target, &usage).await;
    assert!(replies.iter().all(|reply| reply["type"] == "ack"));
    let before = usage.snapshot().unwrap();
    let disk_before = fixture.disk();
    let snapshots =
        crate::storage::snapshot::SnapshotRepository::new(fixture.path.join("session-snapshots"));
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        handle.create_host_checkpoint(snapshots.clone(), "cold-mmp".into(), None),
    )
    .await
    .unwrap();
    let after = usage.snapshot().unwrap();
    handle.shutdown().await.unwrap();
    runner.await.unwrap();
    assert!(
        result.is_ok(),
        "accepted cold originals must be checkpointable through the real actor: {result:?}"
    );
    let checkpoint = result.unwrap();
    assert!(!checkpoint.restorable);
    assert!(
        checkpoint
            .limitations
            .iter()
            .any(|line| line.contains("original-store session recovery"))
    );
    let saved = snapshots.inspect_payload_async("cold-mmp").await.unwrap();
    let state = saved.message_state.unwrap();
    assert_eq!(state.schema_version, 10);
    let data = state.storage_snapshot.as_ref().unwrap();
    assert_eq!(data.native.len(), 1);
    assert_eq!(data.native[0].owner, native_owner);
    assert_eq!(data.native[0].agent_id, native.agent_id.as_str());
    assert_eq!(state.registered_agents.len(), 1);
    assert!(state.retained_messages.is_empty());
    assert!(state.custody_messages.as_deref().unwrap_or(&[]).is_empty());
    assert!(state.accepted_messages.is_empty());
    let encoded = serde_json::to_value(&state).unwrap();
    let sources = encoded["storage_snapshot"]["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert!(
        sources
            .iter()
            .all(|source| source["payload_bytes"] == 768 * 1024)
    );
    assert!(serde_json::to_vec(&state).unwrap().len() < 64 * 1024);
    assert!(mez_agent::messaging::MessageService::from_snapshot_state(&state).is_err());
    assert_eq!(after, before);
    assert_eq!(
        fixture.disk(),
        disk_before,
        "checkpoint qualification is read-only"
    );
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
            .unwrap()
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
            .unwrap()
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

/// A cancelled administrative observer cannot detach the actual checkpoint read
/// or suppress its requested snapshot write. Unrelated actor metrics remain
/// responsive while the root's blocking worker is gated; the final file contains
/// both original references after release, with no canonical body reload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_actor_host_checkpoint_observer_cancellation_still_persists_original_capture() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, target) = actor_fixture(&fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    let usage = actor.service.message_service().payload_observer().unwrap();
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _lease) = hello(&handle).await;
    let replies = send_unread_pair(&handle, &connection, &owner, &target, &usage).await;
    assert!(replies.iter().all(|reply| reply["type"] == "ack"));
    // Consume both acceptance worker notifications before gating the actual read.
    fixture.probe.started.notified().await;
    *fixture.probe.gate.0.lock().unwrap() = false;
    let snapshots =
        crate::storage::snapshot::SnapshotRepository::new(fixture.path.join("session-snapshots"));
    let observing = handle.clone();
    let destination = snapshots.clone();
    let observer = tokio::spawn(async move {
        observing
            .create_host_checkpoint(destination, "cancelled-cold-mmp".into(), None)
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(10), handle.metrics())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 3);
    fixture.probe.release();
    let saved = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(saved) = snapshots.inspect_payload_async("cancelled-cold-mmp").await {
                break saved;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        saved
            .message_state
            .unwrap()
            .storage_snapshot
            .unwrap()
            .sources
            .len(),
        2
    );
    handle.shutdown().await.unwrap();
    runner.await.unwrap();
}
