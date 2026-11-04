//! Live generic restore cannot replace original durable custody or end raw peers.
//!
//! These controls use genuine native/hello registrations, original root writes,
//! the real snapshot repository/control dispatcher and a physical O1 worker gate.
//! Snapshot data supplies layout/display state only, never commit authority.

use super::*;
use crate::control::{ControlConnectionState, decode_control_frame, encode_control_body};
use crate::storage::snapshot::{SessionSnapshotPayload, SnapshotRepository};

/// A stale generic snapshot must not detach the actual storage scope, discard
/// later accepted originals, or invalidate a still-live raw registration. The
/// original root can continue operating on that same service after validation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guarded_generic_snapshot_keeps_original_ledger_and_live_registrations() {
    let fixture = Fixture::new(true);
    let (_handle, mut actor, _root, turn, target) = native_actor::actor_fixture(&fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    actor.poll_message_custody_for_tests();
    actor.observe_message_custody_for_tests().await.unwrap();
    let mut stale = SessionSnapshotPayload::from_session(actor.service.session());
    stale.message_state = Some(actor.service.message_service().snapshot_state().unwrap());
    let raw = actor
        .service
        .message_service()
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    let native = actor
        .service
        .message_service()
        .capture_sender_registration(
            &mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
        )
        .unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    let original = native_actor::source(&actor, &turn, &target);
    actor
        .start_tracked_native_message(&turn, original, MessageScope::Session, 100, reply)
        .unwrap();
    actor.observe_message_custody_for_tests().await.unwrap();
    let accepted = response.await.unwrap().unwrap();
    assert!(accepted.accepted);
    let watermark = actor.service.message_service().next_message_sequence();
    let mut invalid = stale.clone();
    invalid.message_state.as_mut().unwrap().schema_version = 99;
    assert!(
        actor
            .service
            .restore_message_state_for_restored_snapshot(&invalid)
            .is_err()
    );
    assert_eq!(
        actor.service.message_service().next_message_sequence(),
        watermark
    );
    actor
        .service
        .restore_message_state_for_restored_snapshot(&stale)
        .unwrap();
    assert!(actor.service.message_service().payload_observer().is_some());
    assert_eq!(
        actor.service.message_service().next_message_sequence(),
        watermark
    );
    for registration in [&raw, &native] {
        assert!(
            actor
                .service
                .message_service()
                .sender_registration_is_current(registration)
        );
    }
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
    // Actual old-root end and terminal operations still recognize the scope.
    actor
        .service
        .message_service_mut()
        .retire_agent_identity(&target.agent_id);
    for _ in 0..32 {
        actor.poll_message_custody_for_tests();
        actor.observe_message_custody_for_tests().await.unwrap();
        if !actor
            .service
            .message_service()
            .delivery_outcomes_for(
                &mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
                10,
            )
            .unwrap()
            .is_empty()
        {
            break;
        }
    }
    assert_eq!(
        actor
            .service
            .message_service()
            .delivery_outcomes_for(&mez_core::ids::AgentId::opaque(turn.agent_id).unwrap(), 10,)
            .unwrap()
            .len(),
        1
    );
}

/// Releases a generated blocking worker even if an assertion unwinds.
struct Gate(Arc<Probe>);
impl Drop for Gate {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Actual snapshot/resume waits for the end created by retiring an old native
/// pane. Control remains responsive while physical O1 is blocked. On success
/// the original raw socket can still read its pre-restore message and issue a
/// second original, proving neither ledger replacement nor cold classification.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_snapshot_resume_drains_native_end_and_preserves_raw_originals() {
    let fixture = Fixture::new(true);
    let (handle, actor, _native, _turn) = native_receive::scene(&fixture).await;
    let primary = actor
        .service
        .session()
        .attached_primaries()
        .next()
        .unwrap()
        .id
        .clone();
    let snapshots = SnapshotRepository::new(fixture.path.join("snapshots"));
    let runner = tokio::spawn(actor.run());
    let (raw, owner, _lease) = raw_actor::hello(&handle).await;
    let raw_id = raw.agent_id.as_ref().unwrap().clone();
    let connection = ControlConnectionState::trusted_existing_client(primary.clone());
    let created = handle.handle_control_input_for_connection_with_snapshots(
        encode_control_body(r#"{"jsonrpc":"2.0","id":"create","method":"snapshot/create","params":{"target":{"default":true},"idempotency_key":"restore-original-create"}}"#),
        1024 * 1024, connection, snapshots.clone(),
    ).await.unwrap();
    let (body, _) = decode_control_frame(&created.output, 1024 * 1024).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let snapshot_id = value["result"]["snapshot"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let send = |id: &str| {
        crate::protocol::message::encode_mmp_body(&serde_json::json!({
        "protocol":"mmp/1","type":"send","id":id,"time":"original","scope":"session",
        "sender":{"agent_id":raw_id.as_str()},"recipient":{"agent_id":raw_id.as_str()},
        "ttl_ms":null,"correlation_id":null,"content_type":"text/plain; charset=utf-8","payload":"raw original survives live layout restore"
    }).to_string())
    };
    let accepted = handle
        .handle_message_input_with_owner(
            send("before-live-restore"),
            4096,
            raw,
            100,
            Some(owner.clone()),
        )
        .await
        .unwrap();
    assert_eq!(raw_actor::body(&accepted.output)["type"], "ack");
    fixture.probe.started.notified().await;
    let _gate = Gate(fixture.probe.clone());
    *fixture.probe.gate.0.lock().unwrap() = false;
    let attempted_during_restore = send("during-live-restore-never-accepted");
    let attempted_bytes = attempted_during_restore.len();
    let second_snapshots = snapshots.clone();
    let second_snapshot_id = snapshot_id.clone();
    let restore_handle = handle.clone();
    let resume = tokio::spawn(async move {
        restore_handle.handle_control_input_for_connection_with_snapshots(
            encode_control_body(&serde_json::json!({"jsonrpc":"2.0","id":"resume","method":"snapshot/resume","params":{"snapshot_id":snapshot_id,"idempotency_key":"restore-original-resume"}}).to_string()),
            1024 * 1024, created.connection, snapshots,
        ).await
    });
    tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .unwrap();
    assert!(
        !resume.is_finished(),
        "restore replied before actual end journal joined"
    );
    assert!(handle.lifecycle_state().await.is_ok());
    let paused_raw = handle
        .handle_message_input_with_owner(
            attempted_during_restore,
            4096,
            accepted.connection.clone(),
            150,
            Some(owner.clone()),
        )
        .await
        .unwrap();
    assert_eq!(paused_raw.consumed, attempted_bytes);
    assert_eq!(paused_raw.connection.agent_id.as_ref(), Some(&raw_id));
    assert_eq!(raw_actor::body(&paused_raw.output)["type"], "error");
    let conflicting = handle.handle_control_input_for_connection_with_snapshots(
        encode_control_body(&serde_json::json!({"jsonrpc":"2.0","id":"second-resume","method":"snapshot/resume","params":{"snapshot_id":second_snapshot_id,"idempotency_key":"second-restore-must-not-replace-original"}}).to_string()),
        1024 * 1024, ControlConnectionState::trusted_existing_client(primary), second_snapshots,
    ).await.unwrap();
    let (body, _) = decode_control_frame(&conflicting.output, 1024 * 1024).unwrap();
    assert!(body.contains("already pending"), "{body}");
    assert!(!resume.is_finished());
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT count(*) FROM records WHERE key LIKE 'o1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    fixture.probe.release();
    let resumed = tokio::time::timeout(Duration::from_secs(20), resume)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (body, _) = decode_control_frame(&resumed.output, 1024 * 1024).unwrap();
    assert!(body.contains("\"resumed\":true"), "{body}");
    let batch = handle
        .message_fanout_ready_for(raw_id.clone(), 200, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.batch.batch.messages.len(), 1);
    assert_eq!(
        batch.batch.batch.messages[0].envelope.id,
        "before-live-restore"
    );
    let next = handle
        .handle_message_input_with_owner(
            send("after-live-restore"),
            4096,
            paused_raw.connection,
            201,
            Some(owner),
        )
        .await
        .unwrap();
    assert_eq!(raw_actor::body(&next.output)["type"], "ack");
    handle.shutdown().await.unwrap();
    let mut exit = runner.await.unwrap();
    assert!(exit.service.message_service().payload_observer().is_some());
    assert_eq!(exit.service.message_service().next_message_sequence(), 3);
    assert!(!exit.service.message_restore_pending());
    exit.service.terminate_all_pane_processes().unwrap();
}

/// A real canonical input is already issued before restore is requested. The
/// old registration cannot end while its original root result/application/ACK
/// remains owned. Dropping the restore observer cannot cancel that drain or
/// cause another input, provider continuation, or loss of the original ledger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_restore_retains_issued_canonical_input_after_observer_cancellation() {
    use mez_agent::transcript::{TranscriptContextEvent, TranscriptRole};
    let fixture = Fixture::new(true);
    let (_handle, mut actor, recipient, turn) = native_receive::scene(&fixture).await;
    let primary = actor
        .service
        .session()
        .attached_primaries()
        .next()
        .unwrap()
        .id
        .clone();
    let snapshots = SnapshotRepository::new(fixture.path.join("snapshots"));
    snapshots
        .create_from_session("before-canonical-input", None, actor.service.session())
        .unwrap();
    let original_registration = actor
        .service
        .message_service()
        .capture_sender_registration(&recipient)
        .unwrap();
    native_receive::accept(&mut actor, &recipient, &turn, "canonical-before-restore").await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert!(actor.service.has_pending_native_input());
    // Issue the actual canonical append, retaining its real unobserved handle.
    actor.poll_message_custody_for_tests();
    let mut response = actor
        .stage_live_restore_for_tests(snapshots, "before-canonical-input", primary)
        .await
        .unwrap();
    assert!(actor.service.message_restore_pending());
    assert!(!actor.poll_live_restore_for_tests().await);
    assert!(
        actor
            .service
            .message_service()
            .sender_registration_is_current(&original_registration)
    );
    assert!(
        !actor
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    assert!(matches!(
        response.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(
        actor
            .service
            .claim_configured_agent_provider_task(&recipient, &turn.turn_id)
            .unwrap()
            .is_none()
    );
    drop(response);
    let mut applied = false;
    for _ in 0..64 {
        actor.poll_message_custody_for_tests();
        actor.observe_message_custody_for_tests().await.unwrap();
        if actor.poll_live_restore_for_tests().await {
            applied = true;
            break;
        }
    }
    assert!(
        applied,
        "cancelled observer must not abandon original restore drain"
    );
    assert!(!actor.service.message_restore_pending());
    assert!(!actor.service.has_pending_native_input());
    assert!(
        !actor
            .service
            .message_service()
            .sender_registration_is_current(&original_registration)
    );
    assert!(actor.service.message_service().payload_observer().is_some());
    assert_eq!(actor.service.message_service().next_message_sequence(), 2);
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    let rows = actor
        .service
        .agent_transcript_store()
        .unwrap()
        .inspect(&turn.conversation_id)
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.role == TranscriptRole::System
                && matches!(
                    TranscriptContextEvent::from_transcript_content(&row.content),
                    Some(TranscriptContextEvent::NativePeerMessage { .. })
                ))
            .count(),
        1
    );
    for prefix in ["k1", "o1"] {
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT count(*) FROM records WHERE key LIKE ?1",
                    [format!("{prefix}:%")],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
    actor.service.terminate_all_pane_processes().unwrap();
}
