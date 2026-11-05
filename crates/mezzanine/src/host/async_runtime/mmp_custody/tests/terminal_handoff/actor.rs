//! Live sender facts driven by genuine action acceptance and actor/root custody.
//!
//! A configured offline provider plans the real send under its issued claim;
//! actual runtime persistence and the actor publish acceptance. Expiry controls
//! set a trusted finite TTL before acceptance and use the original terminal
//! port at a future observation. Raw-retirement controls keep the actual normal
//! TTL-free source and drive its genuine end and terminal through the actor.
//! The tests then drive the actual actor handoff, without fabricating action
//! ownership, context event IDs, canonical proofs, or release bits.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle};
use crate::runtime::{PersistenceEvent, RuntimeSideEffect};
use mez_agent::{ActionStatus, AgentTurnRecord};

/// A genuine accepted action and canonical terminal observation, still pending
/// its sender handoff. The fixture's native process is explicitly retired by
/// each owner after assertions, as in the native action acceptance tests.
async fn accepted_scene(
    fixture: &Fixture,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    AgentTurnRecord,
    SenderIdentity,
) {
    accepted_scene_with_terminal(fixture, false).await
}

/// Uses the same genuine action owner for either strict expiry or an actual
/// raw recipient end; retirement keeps the action's ordinary TTL-free source.
async fn accepted_scene_with_terminal(
    fixture: &Fixture,
    retired: bool,
) -> (
    AsyncRuntimeSessionHandle,
    AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    AgentTurnRecord,
    SenderIdentity,
) {
    let (handle, mut actor, root, turn, target, _) =
        super::super::native_actions::actor_fixture_with_transcript(
            fixture,
            mez_agent::ApprovalPolicy::AutoAllow,
            true,
        )
        .await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    if !retired {
        actor.service.set_native_action_ttl_for_tests(60_000);
    }
    let sender = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let dispatch = actor
        .service
        .claim_configured_agent_provider_task(&sender, &turn.turn_id)
        .unwrap()
        .expect("genuine configured provider claim");
    let (_, _, execution) = super::super::native_actions::plan(dispatch, &target, false).await;
    let transition = actor
        .service
        .apply_agent_provider_completed_transition(&sender, &turn.turn_id, execution)
        .await
        .unwrap();
    let mut effects = transition.side_effects;
    while let Some(effect) = effects.pop() {
        if let RuntimeSideEffect::SettleAgentProviderPersistence { work } = effect {
            let outcome = tokio::task::spawn_blocking(move || {
                crate::runtime::execute_agent_provider_persistence_work(*work)
            })
            .await
            .unwrap()
            .unwrap();
            effects.extend(
                actor
                    .service
                    .apply_agent_provider_persistence_settled_transition(outcome)
                    .await
                    .unwrap()
                    .side_effects,
            );
        }
    }
    loop {
        actor.poll_message_custody_for_tests();
        let status = actor.observe_message_custody_for_tests().await.unwrap();
        match status {
            CustodyWorkerStatus::NativeContinuityAwaitingAdoption
            | CustodyWorkerStatus::RegistrationEndAwaitingAdoption => continue,
            CustodyWorkerStatus::AwaitingAdoption => break,
            other => {
                actor.service.terminate_all_pane_processes().unwrap();
                panic!("unexpected genuine action admission: {other:?}");
            }
        }
    }
    assert_eq!(
        actor
            .service
            .agent_turn_executions()
            .get(&turn.turn_id)
            .unwrap()
            .action_results[0]
            .status,
        ActionStatus::Succeeded
    );
    drain_ordinary(&mut actor).await;
    if retired {
        assert!(
            actor
                .service
                .message_service_mut()
                .retire_agent_identity(&target.agent_id)
        );
        let mut ended = false;
        for _ in 0..16 {
            actor.poll_message_custody_for_tests();
            match actor.observe_message_custody_for_tests().await.unwrap() {
                CustodyWorkerStatus::RegistrationEndAwaitingAdoption => ended = true,
                CustodyWorkerStatus::TerminalAwaitingAdoption if ended => {
                    return (handle, actor, root, turn, target);
                }
                CustodyWorkerStatus::Idle => {}
                other => {
                    actor.service.terminate_all_pane_processes().unwrap();
                    panic!("unexpected genuine raw retirement: {other:?}");
                }
            }
        }
        actor.service.terminate_all_pane_processes().unwrap();
        panic!("actual raw end did not reach terminal adoption");
    }
    let ticket = actor
        .service
        .message_service_mut()
        .prepare_storage_expiry(root.storage_scope(), u64::MAX - 1, usize::MAX)
        .unwrap()
        .expect("real original TTL source");
    root.start_terminal(actor.service.message_service_mut(), &ticket)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    root.adopt_terminal(actor.service.message_service_mut())
        .unwrap();
    drop(ticket);
    (handle, actor, root, turn, target)
}

/// Settles only already queued ordinary transcript work through its real port
/// and completion event; no frontier or persistence readiness is assigned.
async fn drain_ordinary(actor: &mut AsyncRuntimeSessionActor) {
    for _ in 0..16 {
        let wake = actor.service.stored_message_continuation_needs_wake();
        actor
            .service
            .recover_stored_message_continuations()
            .unwrap();
        let resumed = wake && !actor.service.stored_message_continuation_needs_wake();
        let candidates = actor.service.claim_bookkeeping_candidates();
        let mut progressed = resumed || !candidates.is_empty();
        for work in candidates {
            let read = work.clone();
            let history = tokio::task::spawn_blocking(move || read.execute())
                .await
                .unwrap();
            actor
                .service
                .complete_bookkeeping_candidate(work, history)
                .unwrap();
        }
        for effect in actor
            .service
            .drain_transcript_persistence_transition()
            .side_effects
        {
            if let RuntimeSideEffect::PersistTranscriptEntries {
                store,
                entries,
                path,
            } = effect
            {
                progressed = true;
                let original = entries.clone();
                let bytes = tokio::task::spawn_blocking(move || store.append_many(&entries))
                    .await
                    .unwrap()
                    .unwrap();
                actor
                    .service
                    .apply_persistence_transition(PersistenceEvent::TranscriptCompleted {
                        conversation_id: original[0].conversation_id.clone(),
                        first_sequence: original[0].sequence,
                        entries: original,
                        path,
                        bytes,
                    })
                    .unwrap();
            }
        }
        if !progressed {
            return;
        }
    }
    panic!("ordinary action bookkeeping did not settle");
}

fn journal_count(fixture: &Fixture, prefix: &str) -> i64 {
    fixture
        .connection()
        .query_row(
            "SELECT count(*) FROM records WHERE key LIKE ?1",
            [format!("{prefix}:%")],
            |row| row.get(0),
        )
        .unwrap()
}
fn facts(actor: &AsyncRuntimeSessionActor, turn: &AgentTurnRecord) -> usize {
    actor
        .service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .blocks()
        .iter()
        .filter(|b| {
            b.source == mez_agent::ContextSourceKind::RuntimeHint
                && b.content
                    .starts_with("Accepted MMP recipient delivery ended")
        })
        .count()
}
fn pending(actor: &AsyncRuntimeSessionActor) -> usize {
    actor
        .service
        .message_service()
        .storage_usage()
        .unwrap()
        .unwrap()
        .pending_messages
}

/// Guarded timer cannot half-append memory or directly consume. Qualified
/// absence preflights one actual event; canonical commit leaves transport
/// pending, and only the separate actual S1 adoption releases it. Provider
/// offers, accepted action result and recipient cursor do not change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_persists_before_consumption_without_provider_wake() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, target) = accepted_scene(&fixture).await;
    let before = actor.service.pending_agent_provider_tasks();
    actor
        .service
        .apply_peer_message_delivery_timer(crate::runtime::current_unix_millis(), 1)
        .unwrap();
    assert_eq!(facts(&actor, &turn), 0);
    assert_eq!(pending(&actor), 1);
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(facts(&actor, &turn), 1);
    assert!(
        actor
            .service
            .canonical_message_pending_for_conversation(&turn.conversation_id)
    );
    assert!(
        actor
            .service
            .ensure_runtime_message_transfers_settled_for_restore()
            .is_err()
    );
    assert!(
        actor.service.pending_agent_provider_tasks().is_empty(),
        "existing offer waits behind the canonical fact"
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(journal_count(&fixture, "h1"), 1);
    assert_eq!(journal_count(&fixture, "s1"), 0);
    assert_eq!(pending(&actor), 1);
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(pending(&actor), 0);
    assert!(!actor.service.has_pending_terminal_handoff());
    assert_eq!(facts(&actor, &turn), 1);
    assert_eq!(actor.service.pending_agent_provider_tasks(), before);
    assert_eq!(
        actor
            .service
            .message_service()
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    let rows = actor
        .service
        .agent_transcript_store()
        .unwrap()
        .inspect(&turn.conversation_id)
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(
                TranscriptContextEvent::from_transcript_content(&r.content),
                Some(TranscriptContextEvent::TerminalHandoff { .. })
            ))
            .count(),
        1
    );
    actor.service.terminate_all_pane_processes().unwrap();
}

/// The original context/projection already exists when Stop arrives. Graceful
/// shutdown must drain canonical handoff and S1 through the original root,
/// without treating the ended turn as permission to discard accepted custody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_keeps_preflighted_owner_through_shutdown() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, turn, _) = accepted_scene(&fixture).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(facts(&actor, &turn), 1);
    let runner = tokio::spawn(actor.run());
    tokio::time::timeout(Duration::from_secs(10), handle.shutdown())
        .await
        .unwrap()
        .unwrap();
    let mut exited = runner.await.unwrap();
    assert!(!exited.service.has_pending_terminal_handoff());
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
    assert_eq!(journal_count(&fixture, "h1"), 1);
    assert_eq!(journal_count(&fixture, "s1"), 1);
    exited.service.terminate_all_pane_processes().unwrap();
}

/// Historical h1 is probed before an already-aged horizon. A fresh runtime
/// with no accepted-action metadata may settle its original slot, but cannot
/// recreate a sender task, append a second row or offer provider work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_historical_positive_precedes_horizon_without_task() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    scene.start();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    let proof = scene
        .root
        .adopt_terminal_handoff(&scene.service)
        .unwrap()
        .unwrap();
    drop(proof);
    let Scene {
        root,
        service,
        source,
        transcript,
        ..
    } = scene;
    drop(source);
    let before = transcript.inspect("native-sender").unwrap();
    let mut runtime = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    *runtime.message_service_mut() = service;
    runtime.set_agent_transcript_store(transcript.clone());
    let tasks = runtime.agent_turn_ledger().turns().len();
    let (handle, mut actor) = AsyncRuntimeSessionActor::new(runtime, Default::default()).unwrap();
    actor.register_message_custody(root).unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(actor.service.has_pending_terminal_handoff());
    assert_eq!(pending(&actor), 1);
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(pending(&actor), 0);
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(journal_count(&fixture, "r1"), 0);
    assert_eq!(transcript.inspect("native-sender").unwrap(), before);
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), tasks);
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    drop(handle);
}

/// Absence without a real accepted-action owner grants no live application
/// authority. The old lower source can age out through actual R1; neither an
/// available native sender nor a matching archive path creates a runtime fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_absence_without_action_keeps_context_empty() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture, Probe::new(true)).await;
    let Scene {
        root,
        service,
        source,
        transcript,
        ..
    } = scene;
    drop(source);
    let before = transcript.inspect("native-sender").unwrap();
    let mut runtime = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    *runtime.message_service_mut() = service;
    runtime.set_agent_transcript_store(transcript.clone());
    let (_, mut actor) = AsyncRuntimeSessionActor::new(runtime, Default::default()).unwrap();
    actor.register_message_custody(root).unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(!actor.service.has_pending_terminal_handoff());
    assert_eq!(journal_count(&fixture, "h1"), 0);
    assert_eq!(journal_count(&fixture, "s1"), 0);
    assert_eq!(transcript.inspect("native-sender").unwrap(), before);
}

/// Conflicting actual context evidence fails before installing a candidate. The
/// real accepted owner and qualified root absence do not turn a reused label
/// into a durable occurrence or permission to consume the lower outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_preflight_failure_does_not_half_append_context() {
    let fixture = Fixture::new(true);
    let (_, mut actor, root, turn, _) = accepted_scene(&fixture).await;
    let sender = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let outcome = actor
        .service
        .message_service()
        .delivery_outcomes_for(&sender, 1)
        .unwrap()
        .remove(0);
    let source = actor
        .service
        .message_service()
        .capture_terminal_handoff_lookup(outcome.sequence, 0)
        .unwrap();
    let target = actor.service.agent_transcript_store().unwrap().clone();
    root.start_terminal_handoff(
        actor.service.message_service_mut(),
        source.clone(),
        target,
        None,
    )
    .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(
        root.adopt_terminal_handoff(actor.service.message_service())
            .unwrap()
            .is_none()
    );
    let (label, _) = mez_agent::transcript::terminal_outcome_reference(&outcome).unwrap();
    actor
        .service
        .agent_turn_contexts_mut()
        .get_mut(&turn.turn_id)
        .unwrap()
        .append_runtime_fact_event(label, "conflicting local evidence")
        .unwrap();
    let before = actor
        .service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .chronology()
        .len();
    assert!(actor.service.prepare_terminal_handoff(&source).is_err());
    assert_eq!(
        actor
            .service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .chronology()
            .len(),
        before
    );
    assert_eq!(facts(&actor, &turn), 0);
    assert!(!actor.service.has_pending_terminal_handoff());
    assert_eq!(pending(&actor), 1);
    assert_eq!(journal_count(&fixture, "h1"), 0);
    actor.service.terminate_all_pane_processes().unwrap();
}

/// A first-plan publication fault occurs after the real context owner exists.
/// The actor retains its exact projection, gates bookkeeping, and retries that
/// same root through Stop; no second context occurrence or canonical row appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_issued_failure_retains_original_through_stop() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, turn, _) = accepted_scene(&fixture).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    actor.fail_custody_receipt_publication_for_tests(CustodyReceiptPublicationFault::BeforeStage);
    actor.poll_message_custody_for_tests();
    assert!(matches!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffRetryAt(_)
    ));
    assert_eq!(facts(&actor, &turn), 1);
    assert_eq!(pending(&actor), 1);
    assert_eq!(journal_count(&fixture, "h1"), 0);
    assert!(actor.service.claim_bookkeeping_candidates().is_empty());
    let runner = tokio::spawn(actor.run());
    tokio::time::timeout(Duration::from_secs(10), handle.shutdown())
        .await
        .unwrap()
        .unwrap();
    let mut exited = runner.await.unwrap();
    assert_eq!(journal_count(&fixture, "u1"), 0);
    assert_eq!(journal_count(&fixture, "h1"), 1);
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert!(!exited.service.has_pending_terminal_handoff());
    exited.service.terminate_all_pane_processes().unwrap();
}

/// A real queued ordinary row owns its archive position before the sender fact.
/// The lookup may prove absence, but preflight waits without changing memory.
/// Actual ordinary completion and bounded retry then admit the same original.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_waits_for_ordinary_producer_before_context_preflight() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, _) = accepted_scene(&fixture).await;
    let store = actor.service.agent_transcript_store().unwrap().clone();
    let high_water = store
        .inspect(&turn.conversation_id)
        .unwrap()
        .last()
        .unwrap()
        .sequence;
    let row = TranscriptEntry {
        conversation_id: turn.conversation_id.clone(),
        sequence: high_water + 1,
        created_at_unix_seconds: turn.started_at_unix_seconds,
        role: TranscriptRole::System,
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        content: "original earlier ordinary producer".into(),
    };
    actor
        .service
        .queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
            path: store.transcript_path(&turn.conversation_id).unwrap(),
            store: store.clone(),
            entries: vec![row.clone()],
        });
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(facts(&actor, &turn), 0);
    assert!(!actor.service.has_pending_terminal_handoff());
    drain_ordinary(&mut actor).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(facts(&actor, &turn), 1);
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    let rows = store.inspect(&turn.conversation_id).unwrap();
    assert!(
        rows.iter().position(|r| r == &row).unwrap()
            < rows
                .iter()
                .position(|r| matches!(
                    TranscriptContextEvent::from_transcript_content(&r.content),
                    Some(TranscriptContextEvent::TerminalHandoff { .. })
                ))
                .unwrap()
    );
    actor.service.terminate_all_pane_processes().unwrap();
}

/// Post-canonical private-frontier failure must retain the actual positive,
/// without consuming or replaying canonical storage. Repairing only application
/// state lets local backoff settle S1 against that same original H1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_keeps_positive_across_application_failure() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, _) = accepted_scene(&fixture).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    let original = actor
        .service
        .agent_shell_store()
        .get(&turn.pane_id)
        .unwrap()
        .clone();
    let mut broken = original.clone();
    broken.transcript_entries = u64::MAX;
    actor
        .service
        .agent_shell_store_mut()
        .restore_session(&turn.pane_id, broken)
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(journal_count(&fixture, "h1"), 1);
    assert_eq!(journal_count(&fixture, "s1"), 0);
    assert!(actor.service.has_pending_terminal_handoff());
    assert!(
        actor
            .service
            .pending_terminal_handoff_consumption()
            .is_none()
    );
    assert_eq!(pending(&actor), 1);
    for _ in 0..4 {
        actor.poll_message_custody_for_tests();
    }
    assert_eq!(journal_count(&fixture, "h1"), 1);
    actor
        .service
        .agent_shell_store_mut()
        .restore_session(&turn.pane_id, original)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(journal_count(&fixture, "h1"), 1);
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(pending(&actor), 0);
    actor.service.terminate_all_pane_processes().unwrap();
}

/// A parked preflight source must not occupy the physical lane or positive slot.
/// The bounded scan can settle another original historical H1 while an earlier
/// ordinary producer remains queued, preserving both originals independently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_deferred_source_does_not_starve_historical_positive() {
    let fixture = Fixture::new(true);
    let scene = scene_with_second_source(&fixture, true).await;
    let Scene {
        root,
        service,
        source,
        transcript,
        ..
    } = scene;
    drop(source);
    let mut runtime = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    *runtime.message_service_mut() = service;
    runtime.set_agent_transcript_store(transcript.clone());
    let row = TranscriptEntry {
        conversation_id: "native-sender".into(),
        sequence: 3,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "earlier-ordinary-producer".into(),
        agent_id: "original-sender".into(),
        pane_id: "original-pane".into(),
        content: "still queued".into(),
    };
    runtime.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        path: transcript.transcript_path("native-sender").unwrap(),
        store: transcript,
        entries: vec![row],
    });
    let (_, mut actor) = AsyncRuntimeSessionActor::new(runtime, Default::default()).unwrap();
    actor.register_message_custody(root).unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(!actor.service.has_pending_terminal_handoff());
    assert_eq!(pending(&actor), 2);
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(actor.service.has_pending_terminal_handoff());
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(pending(&actor), 1);
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(journal_count(&fixture, "r1"), 0);
}

/// Creates a second actual terminal source, optionally finishing its genuine
/// H1 before runtime installation. Both actor pacing controls use identical
/// accepted originals and physical I/O, with no reconstructed task authority.
async fn scene_with_second_source(fixture: &Fixture, canonical: bool) -> Scene {
    let mut scene = Scene::new(fixture, Probe::new(true)).await;
    let target = scene
        .service
        .register_agent(None, None, "second-target", vec![]);
    let mut original = envelope(&scene.sender, &target, "second-historical-terminal");
    original.ttl_ms = Some(5);
    let ticket = scene
        .service
        .prepare_acceptance(
            &scene.sender.agent_id,
            original,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    scene.root.start(&mut scene.service, &ticket, None).unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    scene.root.adopt(&mut scene.service, None).unwrap();
    drop(ticket);
    let ticket = scene
        .service
        .prepare_storage_expiry(scene.root.storage_scope(), 16, usize::MAX)
        .unwrap()
        .unwrap();
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    scene.root.adopt_terminal(&mut scene.service).unwrap();
    drop(ticket);
    if !canonical {
        return scene;
    }
    let outcome = scene
        .service
        .delivery_outcomes_for(&scene.sender.agent_id, 10)
        .unwrap()
        .into_iter()
        .find(|o| o.message_id == "second-historical-terminal")
        .unwrap();
    let source = scene
        .service
        .capture_terminal_handoff(&scene.sender.agent_id, &outcome)
        .unwrap();
    let (label, content) = mez_agent::transcript::terminal_outcome_reference(&outcome).unwrap();
    let row = TranscriptEntry {
        conversation_id: "native-sender".into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "actual-second-original-context".into(),
        agent_id: scene.sender.agent_id.to_string(),
        pane_id: "original-pane".into(),
        content,
    };
    let projection = Arc::new(
        crate::storage::mmp_custody::terminal_handoff::TerminalHandoffProjection::new(
            row,
            12,
            label,
            vec![],
        )
        .unwrap(),
    );
    scene
        .root
        .start_terminal_handoff(
            &mut scene.service,
            source.clone(),
            scene.transcript.clone(),
            Some(projection),
        )
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    drop(
        scene
            .root
            .adopt_terminal_handoff(&scene.service)
            .unwrap()
            .unwrap(),
    );
    drop(source);
    scene
}

/// Two deferred absences share one retry owner. Repeated ordinary actor polls
/// must exhaust their bounded pass and wait for its deadline, rather than keep
/// replacing the parked source and reopening immediate full scans forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_multiple_deferrals_do_not_spin_root_lookups() {
    let fixture = Fixture::new(true);
    let Scene {
        root,
        service,
        source,
        transcript,
        ..
    } = scene_with_second_source(&fixture, false).await;
    drop(source);
    let mut runtime = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    *runtime.message_service_mut() = service;
    runtime.set_agent_transcript_store(transcript.clone());
    let row = TranscriptEntry {
        conversation_id: "native-sender".into(),
        sequence: 2,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "earlier-ordinary-producer".into(),
        agent_id: "original-sender".into(),
        pane_id: "original-pane".into(),
        content: "still queued".into(),
    };
    runtime.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        path: transcript.transcript_path("native-sender").unwrap(),
        store: transcript,
        entries: vec![row],
    });
    let (_, mut actor) = AsyncRuntimeSessionActor::new(runtime, Default::default()).unwrap();
    actor.register_message_custody(root).unwrap();
    for _ in 0..2 {
        actor.poll_message_custody_for_tests();
        assert_eq!(
            actor.observe_message_custody_for_tests().await.unwrap(),
            CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
        );
    }
    for _ in 0..4 {
        actor.poll_message_custody_for_tests();
        assert_eq!(
            actor.observe_message_custody_for_tests().await.unwrap(),
            CustodyWorkerStatus::Idle
        );
    }
    assert!(!actor.service.has_pending_terminal_handoff());
    assert_eq!(pending(&actor), 2);
    assert_eq!(journal_count(&fixture, "h1"), 0);
    assert_eq!(journal_count(&fixture, "s1"), 0);
}

/// Real retained snapshot captures exhaust metadata for B's S1 admission while A's parked
/// lookup deadline is already expired. Only B's retained application/admission
/// timer may wake the actor during that ownership interval; A resumes after B
/// actually consumes, then its qualified absence can age out normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_positive_backoff_suppresses_older_parked_lookup_wake() {
    let fixture = Fixture::new(true);
    let Scene {
        root,
        service,
        source,
        transcript,
        ..
    } = scene_with_second_source(&fixture, true).await;
    let first_sequence = source.sequence();
    drop(source);
    let mut runtime = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    *runtime.message_service_mut() = service;
    runtime.set_agent_transcript_store(transcript.clone());
    let row = TranscriptEntry {
        conversation_id: "native-sender".into(),
        sequence: 3,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "earlier-ordinary-producer".into(),
        agent_id: "original-sender".into(),
        pane_id: "original-pane".into(),
        content: "still queued".into(),
    };
    runtime.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        path: transcript.transcript_path("native-sender").unwrap(),
        store: transcript,
        entries: vec![row],
    });
    let (handle, mut actor) = AsyncRuntimeSessionActor::new(runtime, Default::default()).unwrap();
    let scope = root.storage_scope().clone();
    actor.register_message_custody(root).unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(
        !actor.service.has_pending_terminal_handoff(),
        "A owns only parked lookup custody"
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert!(
        actor
            .service
            .pending_terminal_handoff_consumption()
            .is_some(),
        "B owns actual applied H1 proof"
    );
    let held_source = actor
        .service
        .message_service()
        .capture_terminal_handoff_lookup(first_sequence, 0)
        .unwrap();
    let io = actor
        .service
        .message_service_mut()
        .reserve_terminal_handoff_io(&held_source)
        .unwrap();
    // S1 streams source validation and no longer needs a second cache body.
    // Hold actual immutable snapshot captures until their shared metadata cap
    // refuses another one, establishing genuine independent resource pressure.
    let mut captures = Vec::new();
    let mut refused = false;
    for _ in 0..256 {
        match actor
            .service
            .message_service()
            .prepare_storage_snapshot(&scope)
        {
            Ok(request) => captures.push(request),
            Err(error) => {
                assert_eq!(
                    error.kind(),
                    mez_agent::messaging::MessageErrorKind::Capacity
                );
                refused = true;
                break;
            }
        }
    }
    assert!(refused && !captures.is_empty());
    assert!(
        actor
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes
            <= 8 * 1024 * 1024
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert!(actor.service.has_pending_terminal_handoff());
    assert!(
        actor
            .service
            .pending_terminal_handoff_consumption()
            .is_none(),
        "actual metadata refusal installed B's future admission retry"
    );
    assert_eq!(journal_count(&fixture, "s1"), 0);
    assert!(
        !actor.message_custody_wake_is_ready_for_tests(),
        "A's expired lookup deadline must not spin while B owns application/consumption backoff"
    );
    for _ in 0..4 {
        actor.poll_message_custody_for_tests();
    }
    assert!(!actor.message_custody_wake_is_ready_for_tests());
    assert_eq!(journal_count(&fixture, "s1"), 0);
    drop(captures);
    drop(io);
    drop(held_source);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(pending(&actor), 1);
    assert!(!actor.service.has_pending_terminal_handoff());
    drain_ordinary(&mut actor).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption,
        "A's original parked lookup resumes after B consumption"
    );
    // This bare recovered runtime has not admitted the queued conversation's
    // existing archive. Drive the real actor startup admission and its actual
    // persistence service, rather than fabricate completion of that dependency.
    let runner = tokio::spawn(actor.run());
    let (stop, persistence) = super::super::native_receive::writer(&handle);
    tokio::time::timeout(Duration::from_secs(10), async {
        while journal_count(&fixture, "r1") == 0 {
            handle.lifecycle_state().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual archive admission/write must release A's later horizon work");
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
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
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(journal_count(&fixture, "r1"), 1);
    assert!(exited.service.pending_agent_provider_tasks().is_empty());
}

/// Suppressing another source's lookup clock must preserve this actual work's
/// own pre-issuance retry. A real held decoder quantum refuses canonical start;
/// its due wake later admits the same projection, then actual H1/S1 settle once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_sender_handoff_own_preissue_backoff_keeps_its_deadline_wakeable() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, _) = accepted_scene(&fixture).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    assert_eq!(facts(&actor, &turn), 1);
    let sender = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let outcome = actor
        .service
        .message_service()
        .delivery_outcomes_for(&sender, 1)
        .unwrap()
        .remove(0);
    let held_source = actor
        .service
        .message_service()
        .capture_terminal_handoff_lookup(outcome.sequence, 0)
        .unwrap();
    let io = actor
        .service
        .message_service_mut()
        .reserve_terminal_handoff_io(&held_source)
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert!(actor.service.pending_terminal_handoff_work().is_some());
    assert_eq!(journal_count(&fixture, "h1"), 0);
    assert!(!actor.message_custody_wake_is_ready_for_tests());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        actor.message_custody_wake_is_ready_for_tests(),
        "the current work's own due preissue retry must remain wakeable"
    );
    drop(io);
    drop(held_source);
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(journal_count(&fixture, "h1"), 1);
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(pending(&actor), 0);
    assert_eq!(facts(&actor, &turn), 1);
    actor.service.terminate_all_pane_processes().unwrap();
}

/// A genuine TTL-free model action to an actual raw registration gets one
/// neutral sender fact after that endpoint ends. Real H1 and separate S1 retain
/// recipient_retired, release only original custody, and preserve provider/action
/// ownership without a fabricated recipient ACK or new task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_raw_retirement_handoff_consumes_actual_ttl_free_action_once() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, target) = accepted_scene_with_terminal(&fixture, true).await;
    let before = actor.service.pending_agent_provider_tasks();
    assert_eq!(facts(&actor, &turn), 0);
    for expected in [
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption,
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption,
        CustodyWorkerStatus::TerminalAwaitingAdoption,
    ] {
        actor.poll_message_custody_for_tests();
        assert_eq!(
            actor.observe_message_custody_for_tests().await.unwrap(),
            expected
        );
    }
    assert_eq!(facts(&actor, &turn), 1);
    assert_eq!(pending(&actor), 0);
    assert_eq!(actor.service.pending_agent_provider_tasks(), before);
    assert!(
        actor
            .service
            .message_service()
            .registered_identity(&target.agent_id)
            .is_none()
    );
    assert_eq!(journal_count(&fixture, "h1"), 1);
    assert_eq!(journal_count(&fixture, "s1"), 1);
    assert_eq!(journal_count(&fixture, "k1"), 0);
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let limits = mez_agent::messaging::MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    };
    let history = store.fold_custody_history(limits).unwrap();
    assert_eq!(
        history.terminal_observations[0].reason,
        mez_agent::messaging::DeliveryTerminalReason::RecipientRetired
    );
    assert!(history.terminal_observations[0].sender_consumed);
    let (source, _, _) = store
        .read_accepted_source(history.terminal_observations[0].sequence, limits)
        .unwrap();
    assert_eq!(source.envelope().ttl_ms, None);
    actor.service.terminate_all_pane_processes().unwrap();
}
