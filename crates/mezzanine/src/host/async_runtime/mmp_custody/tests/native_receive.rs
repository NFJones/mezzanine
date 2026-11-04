//! Native receive at the actual actor/root/application boundary.
//!
//! Genuine user-turn construction supplies canonical context and native tokens.
//! The ordinary actor pump and its real retained root completions drive each
//! stage. Provider offers are inspected without dispatching network work; no
//! transcript receipt, native token or readiness status is fabricated.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle};
use mez_agent::transcript::{TranscriptContextEvent, TranscriptRole};
use mez_core::ids::AgentId;

/// Actual native actor with guarded spill/registration settled before admission.
async fn scene(
    fixture: &Fixture,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    AgentId,
    mez_agent::AgentTurnRecord,
) {
    let (handle, mut actor, root, turn, _) = super::native_actor::actor_fixture(fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let recipient = AgentId::opaque(turn.agent_id.clone()).unwrap();
    (handle, actor, recipient, turn)
}

/// Accepts a genuine same-conversation native self-message via the established
/// tracked native ingress. Its original request, lower source and actual disk
/// acceptance settle through the actor before any input probe can begin.
async fn accept(
    actor: &mut AsyncRuntimeSessionActor,
    recipient: &AgentId,
    turn: &mez_agent::AgentTurnRecord,
    name: &str,
) {
    accept_payload(
        actor,
        recipient,
        turn,
        name,
        "actual canonical native receive input".into(),
    )
    .await;
}

/// Accepts a real caller-selected bounded body for cache pressure controls.
async fn accept_payload(
    actor: &mut AsyncRuntimeSessionActor,
    recipient: &AgentId,
    turn: &mez_agent::AgentTurnRecord,
    name: &str,
    payload: String,
) {
    let target = actor
        .service
        .message_service()
        .registered_identity(recipient)
        .unwrap()
        .clone();
    let mut envelope = super::native_actor::source(actor, turn, &target);
    envelope.id = name.into();
    envelope.payload = payload;
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .start_tracked_native_message(
            turn,
            envelope,
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
}

/// Counts actual canonical peer blocks in the original application's context.
fn peer_blocks(actor: &AsyncRuntimeSessionActor, turn: &mez_agent::AgentTurnRecord) -> usize {
    actor
        .service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .blocks()
        .iter()
        .filter(|b| {
            b.source == mez_agent::ContextSourceKind::PeerMessage
                && b.content.contains("actual canonical native receive input")
        })
        .count()
}

/// Real root absence precedes memory input, the real canonical append precedes
/// native ACK, and provider continuation stays unavailable across both pending
/// stages. Exact ACK admits only the original turn's single provider offer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_receive_persists_before_ack_and_releases_original_provider_once() {
    let fixture = Fixture::new(true);
    let (_, mut actor, recipient, turn) = scene(&fixture).await;
    accept(&mut actor, &recipient, &turn, "actor-native-original").await;
    assert_eq!(peer_blocks(&actor, &turn), 0);
    actor
        .service
        .deliver_pending_runtime_agent_messages(crate::runtime::current_unix_millis())
        .unwrap();
    assert_eq!(
        peer_blocks(&actor, &turn),
        0,
        "ordinary sweep cannot bypass root dedup"
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert_eq!(peer_blocks(&actor, &turn), 1);
    assert!(
        actor
            .service
            .native_input_pending_for_conversation(&turn.conversation_id)
    );
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        actor
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        0
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert!(actor.service.pending_native_input_ack().is_some());
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    let rows = actor
        .service
        .agent_transcript_store()
        .unwrap()
        .inspect(&turn.conversation_id)
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r.role == TranscriptRole::System
                && matches!(
                    TranscriptContextEvent::from_transcript_content(&r.content),
                    Some(TranscriptContextEvent::NativePeerMessage { .. })
                ))
            .count(),
        1
    );
    let user = rows
        .iter()
        .position(|row| {
            matches!(
                TranscriptContextEvent::from_transcript_content(&row.content),
                Some(TranscriptContextEvent::UserEvent { .. })
            )
        })
        .expect("the actual user request must already be canonical before ACK");
    let peer = rows
        .iter()
        .position(|row| {
            matches!(
                TranscriptContextEvent::from_transcript_content(&row.content),
                Some(TranscriptContextEvent::NativePeerMessage { .. })
            )
        })
        .unwrap();
    assert!(
        user < peer,
        "native input cannot precede its original user request"
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    assert_eq!(
        actor
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        1
    );
    assert!(
        !actor
            .service
            .native_input_pending_for_conversation(&turn.conversation_id)
    );
    assert_eq!(
        actor
            .service
            .pending_agent_provider_tasks()
            .iter()
            .filter(|task| task.turn_id == turn.turn_id)
            .count(),
        1
    );
    assert_eq!(peer_blocks(&actor, &turn), 1);
}

/// An actual post-canonical journal refusal retains the original application
/// receipt and blocks provider/ACK publication. The actor retries its original
/// worker plan after real join/backoff, rather than injecting the input again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_receive_journal_refusal_preserves_one_input_and_provider_gate() {
    let fixture = Fixture::new(true);
    let (_, mut actor, recipient, turn) = scene(&fixture).await;
    accept(&mut actor, &recipient, &turn, "actor-native-retry").await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    fixture.connection().execute_batch("CREATE TRIGGER stop_native_commit BEFORE INSERT ON records WHEN NEW.key LIKE 'c1:%' BEGIN SELECT RAISE(FAIL,'injected native journal refusal'); END;").unwrap();
    actor.poll_message_custody_for_tests();
    let CustodyWorkerStatus::NativeInputRetryAt(_) =
        actor.observe_message_custody_for_tests().await.unwrap()
    else {
        panic!("actual journal refusal must join");
    };
    assert_eq!(peer_blocks(&actor, &turn), 1);
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    fixture
        .connection()
        .execute_batch("DROP TRIGGER stop_native_commit")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    assert_eq!(peer_blocks(&actor, &turn), 1);
    assert_eq!(
        actor
            .service
            .pending_agent_provider_tasks()
            .iter()
            .filter(|task| task.turn_id == turn.turn_id)
            .count(),
        1
    );
}

/// Runs the production ordered transcript writer without starting a provider.
/// Stop/namespace tests need its real claim and completion events before native
/// canonical work can acquire the archive prefix. The caller joins it before
/// closing actor request admission.
fn writer(
    handle: &AsyncRuntimeSessionHandle,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    tokio::task::JoinHandle<()>,
) {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let handle = handle.clone();
    let task = tokio::spawn(async move {
        let report = crate::host::async_runtime::run_async_persistence_side_effect_service(
            &handle,
            crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                max_polls: 100_000,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            move |_, _| stopped.load(std::sync::atomic::Ordering::Acquire),
        )
        .await
        .unwrap();
        assert_eq!(report.failed, 0);
    });
    (stop, task)
}

/// Real Stop retires the original task before native selection. The actor then
/// creates one idle peer-triggered context and withholds its provider offer until
/// the same canonical/ACK path completes; no stopped task is restarted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_idle_receive_admits_one_new_turn_after_durable_ack() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, recipient, original) = scene(&fixture).await;
    accept(&mut actor, &recipient, &original, "actor-native-idle").await;
    let primary = actor.service.session().clients()[0].id.clone();
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    let runner = tokio::spawn(actor.run());
    let (stop, persistence) = writer(&handle);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .iter()
                .any(|t| t.turn_id != original.turn_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Release);
    persistence.await.unwrap();
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    let received = exited
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|t| t.trigger == mez_agent::AgentTurnTrigger::LocalMessage)
        .collect::<Vec<_>>();
    assert_eq!(received.len(), 1);
    assert_ne!(received[0].turn_id, original.turn_id);
    assert_eq!(peer_blocks_service(&exited.service, received[0]), 1);
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        1
    );
}

/// A source already selected into the old context survives real Stop and a new
/// conversation binding. The root persists only its original projection, then
/// settles the old slot without advancing the successor cursor or waking it.
/// Interrupted bookkeeping must not archive an extra unsealed peer projection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_late_commit_survives_context_retirement_without_replacement_input() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, recipient, original) = scene(&fixture).await;
    accept(&mut actor, &recipient, &original, "actor-native-retired").await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert_eq!(peer_blocks(&actor, &original), 1);
    let primary = actor.service.session().clients()[0].id.clone();
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor
        .service
        .agent_shell_store_mut()
        .bind_conversation(&original.pane_id, "replacement-native-context", 0)
        .unwrap();
    actor.service.refresh_native_message_bindings().unwrap();
    let runner = tokio::spawn(actor.run());
    let (stop, persistence) = writer(&handle);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            handle.lifecycle_state().await.unwrap();
            if fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Release);
    persistence.await.unwrap();
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        exited
            .service
            .agent_shell_store()
            .get(&original.pane_id)
            .unwrap()
            .session_id,
        "replacement-native-context"
    );
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(exited.service.pending_agent_provider_tasks().is_empty());
    let rows = exited
        .service
        .agent_transcript_store()
        .unwrap()
        .inspect(&original.conversation_id)
        .unwrap();
    let peers = rows
        .iter()
        .filter(
            |r| match TranscriptContextEvent::from_transcript_content(&r.content) {
                Some(TranscriptContextEvent::NativePeerMessage { .. }) => true,
                Some(TranscriptContextEvent::PromptBoundary {
                    source: mez_agent::ContextSourceKind::PeerMessage,
                    ..
                }) => true,
                _ => false,
            },
        )
        .count();
    assert_eq!(
        peers, 1,
        "root and interrupted bookkeeping must share one canonical peer owner"
    );
}

/// Inspecting the original service's current canonical context is observational;
/// it grants no delivery or scheduler authority to this test.
fn peer_blocks_service(
    service: &crate::runtime::RuntimeSessionService,
    turn: &mez_agent::AgentTurnRecord,
) -> usize {
    service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .blocks()
        .iter()
        .filter(|b| {
            b.source == mez_agent::ContextSourceKind::PeerMessage
                && b.content.contains("actual canonical native receive input")
        })
        .count()
}

/// Closing admission after real absence has selected input must finish the
/// original canonical append and exact ACK. Shutdown does not release the
/// previously held provider offer or start a replacement turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_shutdown_drains_selected_input_before_write_issuance() {
    let fixture = Fixture::new(true);
    let (_, mut actor, recipient, turn) = scene(&fixture).await;
    accept(&mut actor, &recipient, &turn, "actor-native-shutdown").await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert_eq!(peer_blocks(&actor, &turn), 1);
    let reply = actor.begin_message_custody_shutdown_for_tests();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    actor.poll_message_custody_for_tests();
    reply.await.unwrap().unwrap();
    assert!(!actor.service.has_pending_native_input());
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 1);
    assert_eq!(
        actor
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'c1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

/// Admits two genuine native recipient originals before scheduling custody is
/// transferred to the actor. Their combined bodies exceed the real cache, so
/// the first is cold and the second resident without manufacturing either state.
async fn accepted_pair(
    fixture: &Fixture,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    AgentId,
    mez_agent::AgentTurnRecord,
) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, turn, sender) = super::native_actor::prepared_service(fixture);
    root.configure_payload_spill(service.message_service_mut(), Default::default())
        .unwrap();
    service
        .message_service_mut()
        .require_native_continuity(root.storage_scope())
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    let recipient = AgentId::opaque(turn.agent_id.clone()).unwrap();
    root.start_native_continuity(service.message_service(), &recipient)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert!(
        root.adopt_native_continuity(service.message_service_mut())
            .unwrap()
    );
    for name in ["native-pair-first", "native-pair-second"] {
        let envelope = Envelope {
            protocol: "mmp/1",
            id: name.into(),
            message_type: "send".into(),
            time: "original-cache-control".into(),
            sender: sender.clone(),
            recipient: Recipient::Agent(recipient.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain".into(),
            payload: "x".repeat(600 * 1024),
            extension_fields: vec![],
        };
        let ticket = service
            .message_service_mut()
            .prepare_acceptance(
                &sender.agent_id,
                envelope,
                MessageScope::Session,
                crate::runtime::current_unix_millis(),
                4096,
                None,
            )
            .unwrap();
        root.start(service.message_service_mut(), &ticket, None)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AwaitingAdoption
        );
        root.adopt(service.message_service_mut(), None).unwrap();
    }
    let (handle, mut actor) = AsyncRuntimeSessionActor::new(
        service,
        crate::host::async_runtime::AsyncRuntimeActorConfig::default(),
    )
    .unwrap();
    actor.register_message_custody(root).unwrap();
    (handle, actor, recipient, turn)
}

/// Two real accepted originals exceed the one-MiB cache. A genuine external
/// allocation pins the second while the first is cold. Capacity refusal must
/// keep actor requests responsive without issuing repeated probe/read workers;
/// releasing that allocation lets the retained original retry and both ACKs
/// finish without a resend or provider dispatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_cold_capacity_waits_without_spin_and_retries_after_pin_release() {
    let fixture = Fixture::new(true);
    let (handle, actor, recipient, _) = accepted_pair(&fixture).await;
    let first = actor
        .service
        .message_service()
        .next_native_input_lookup(None)
        .unwrap()
        .unwrap();
    assert!(first.resident_envelope().is_err());
    let second = actor
        .service
        .message_service()
        .next_native_input_lookup(Some((first.sequence(), first.binding_index())))
        .unwrap()
        .unwrap();
    let pin = second.resident_envelope().unwrap();
    drop((first, second));
    let started = fixture.probe.attempts.load(Ordering::SeqCst);
    let runner = tokio::spawn(actor.run());
    for _ in 0..10 {
        tokio::time::timeout(Duration::from_secs(1), handle.lifecycle_state())
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        fixture.probe.attempts.load(Ordering::SeqCst) - started <= 2,
        "cache refusal must park before issuing another physical job"
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(pin);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            handle.lifecycle_state().await.unwrap();
            if fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        2
    );
    assert!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .cache
            .live_payload_bytes
            <= 1024 * 1024
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'c1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}

/// A saved display projection with the same public occurrence cannot consume
/// the newly selected canonical owner. Actual post-context receive replaces its
/// saved turn labels and retains the original root work through canonical ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_saved_display_collision_cannot_discard_actual_canonical_work() {
    let fixture = Fixture::new(true);
    let (_, mut actor, recipient, turn) = scene(&fixture).await;
    accept(&mut actor, &recipient, &turn, "native-display-collision").await;
    let source = actor
        .service
        .message_service()
        .next_native_input_lookup(None)
        .unwrap()
        .unwrap();
    let envelope = source.resident_envelope().unwrap();
    let identity = crate::runtime::RuntimeSessionService::received_peer_message_presentation_id(
        &recipient,
        source.sequence(),
        &envelope,
    );
    actor
        .service
        .restore_unsettled_received_peer_message_presentations(&[
            crate::storage::snapshot::SnapshotUnsettledPeerPresentation {
                identity,
                occurrence: Default::default(),
                recipient_agent_id: recipient.to_string(),
                pane_id: turn.pane_id.clone(),
                conversation_id: turn.conversation_id.clone(),
                turn_id: "saved-display-is-not-a-turn-owner".into(),
                sequence: source.sequence(),
                peer_label: "saved-label".into(),
                direct_parent: false,
                content_type: "text/plain".into(),
                payload: "display-only".into(),
                presentation_eligible: false,
                live_rendered: false,
            },
        ])
        .unwrap();
    drop((source, envelope));
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert_eq!(peer_blocks(&actor, &turn), 1);
    assert!(actor.service.has_pending_native_input());
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    assert!(!actor.service.has_pending_native_input());
    assert_eq!(
        actor
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        1
    );
}

/// A cold original is skipped while its actual endpoint is offline. The same
/// endpoint becomes available, with one real delivery notification, while the
/// later original's actual root lookup remains owned. Exhausting that pass must
/// preserve a rescan of the earlier cold slot. The running actor receives no
/// further request, timer event or readiness mutation before both durable ACKs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_active_scan_preserves_notification_for_earlier_cold_source() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, recipient, turn) = accepted_pair(&fixture).await;
    actor
        .native_presence_changed_for_tests(
            &recipient,
            mez_agent::messaging::AgentPresenceStatus::Offline,
        )
        .unwrap();
    let first = actor
        .service
        .message_service()
        .next_native_input_lookup(None)
        .unwrap()
        .unwrap();
    assert!(first.resident_envelope().is_err());
    assert!(
        !actor
            .service
            .message_service()
            .native_input_can_receive(&first, crate::runtime::current_unix_millis())
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert!(
        first.resident_envelope().is_err(),
        "an ineligible lookup must not issue a read or capacity retry"
    );
    assert_eq!(peer_blocks(&actor, &turn), 0);
    // Own B's real retained lookup before publishing A's changed availability.
    actor.poll_message_custody_for_tests();
    actor
        .native_presence_changed_for_tests(
            &recipient,
            mez_agent::messaging::AgentPresenceStatus::Available,
        )
        .unwrap();
    assert!(
        actor
            .service
            .message_service()
            .native_input_can_receive(&first, crate::runtime::current_unix_millis())
    );
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert!(first.resident_envelope().is_err());
    drop(first);
    let runner = tokio::spawn(actor.run());
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // This is the first actor request since the single readiness notification.
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert!(
        settled.is_ok(),
        "the earlier cold eligible original was stranded after active-pass notification"
    );
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        2
    );
    assert_eq!(
        exited
            .service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .chronology()
            .iter()
            .filter(|e| e.block().source == mez_agent::ContextSourceKind::PeerMessage)
            .count(),
        2
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'c1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}

/// A valid in-memory event can exceed the narrower canonical user codec. Native
/// receive must refuse at preflight without adding a peer, losing its source,
/// advancing its cursor or exposing a provider continuation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_prefix_preflight_failure_leaves_live_context_and_source_unchanged() {
    let fixture = Fixture::new(true);
    let (_, mut actor, recipient, turn) = scene(&fixture).await;
    actor
        .service
        .agent_turn_contexts_mut()
        .get_mut(&turn.turn_id)
        .unwrap()
        .append_user_event("steering", "accepted memory event\0not canonical")
        .unwrap();
    let before = actor
        .service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .clone();
    let offers_before = actor
        .service
        .pending_agent_provider_tasks()
        .iter()
        .map(|task| task.turn_id.clone())
        .collect::<Vec<_>>();
    accept(&mut actor, &recipient, &turn, "native-prefix-refusal").await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert_eq!(
        *actor
            .service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap(),
        before
    );
    assert_eq!(peer_blocks(&actor, &turn), 0);
    assert!(!actor.service.has_pending_native_input());
    assert_eq!(
        actor
            .service
            .pending_agent_provider_tasks()
            .iter()
            .map(|task| task.turn_id.clone())
            .collect::<Vec<_>>(),
        offers_before,
        "refused input must leave the original provider offer unchanged"
    );
    assert_eq!(
        actor
            .service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        0
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'c1:%' OR key LIKE 'k1:%'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
