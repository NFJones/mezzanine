//! Runtime-authored bridge sources cross the real actor/root receive boundary.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle};

pub(super) fn bridge(
    actor: &AsyncRuntimeSessionActor,
    turn: &mez_agent::AgentTurnRecord,
    name: &str,
) -> Envelope {
    let id = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let target = actor
        .service
        .message_service()
        .registered_identity(&id)
        .unwrap();
    let mut envelope = super::native_actor::source(actor, turn, target);
    envelope.id = name.into();
    envelope.message_type = "task_status".into();
    envelope.payload = format!(
        r#"{{"task_id":"{}","state":"running","progress_percent":0,"summary":"bridge source test"}}"#,
        turn.turn_id
    );
    envelope.ttl_ms = None;
    envelope
}

pub(super) async fn cleanup(
    fixture: &Fixture,
    handle: AsyncRuntimeSessionHandle,
    mut actor: AsyncRuntimeSessionActor,
) {
    actor.service.terminate_all_pane_processes().unwrap();
    drop((actor, handle));
    wait_until_unlocked(&fixture.path).await;
    std::fs::remove_dir_all(&fixture.path).unwrap();
    assert!(!fixture.path.exists());
}

pub(super) async fn shutdown(actor: &mut AsyncRuntimeSessionActor) {
    let mut closing = actor.begin_message_custody_shutdown_for_tests();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            actor.poll_message_custody_for_tests();
            match closing.try_recv() {
                Ok(result) => {
                    result.unwrap();
                    break;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    actor.observe_message_custody_for_tests().await.unwrap();
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("actual shutdown reply lost: {error}"),
            }
        }
    })
    .await
    .unwrap();
}

/// Refusing a later or oversized source must not overwrite an earlier original,
/// consume a lower sequence, issue I/O, or turn staging into accepted success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_source_capacity_is_bounded_and_preserves_original() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, turn) = super::native_receive::scene(&fixture).await;
    let now = crate::runtime::current_unix_millis();
    let sequence = actor.service.message_service().next_message_sequence();
    let writes = fixture.probe.attempts.load(Ordering::SeqCst);
    let mut oversized = bridge(&actor, &turn, "oversized-bridge");
    oversized
        .extension_fields
        .push(("large-metadata".into(), "x".repeat(300_000)));
    assert!(
        actor
            .service
            .accept_runtime_bridge_message(&turn, oversized, now)
            .is_err()
    );
    let original = bridge(&actor, &turn, "original-bridge");
    assert!(
        !actor
            .service
            .accept_runtime_bridge_message(&turn, original, now)
            .unwrap()
    );
    let later = bridge(&actor, &turn, "later-bridge");
    assert!(
        actor
            .service
            .accept_runtime_bridge_message(&turn, later, now)
            .is_err()
    );
    assert_eq!(
        actor.service.message_service().next_message_sequence(),
        sequence
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), writes);
    assert!(actor.service.has_pending_stored_message_actions());
    assert!(!actor.service.message_service().has_unpublished_acceptance());
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    assert_eq!(
        actor.service.message_service().next_message_sequence(),
        sequence + 1
    );
    let only_original = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'a1:%:commit'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    shutdown(&mut actor).await;
    cleanup(&fixture, handle, actor).await;
    assert_eq!(only_original, 1);
}

/// Never-issued semantics cancel on shutdown; the root receives no admission
/// write and a later callback cannot turn the cancelled source into acceptance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_unissued_shutdown_does_not_accept() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, turn) = super::native_receive::scene(&fixture).await;
    let envelope = bridge(&actor, &turn, "shutdown-bridge");
    let before = fixture.disk();
    assert!(
        !actor
            .service
            .accept_runtime_bridge_message(&turn, envelope, crate::runtime::current_unix_millis())
            .unwrap()
    );
    shutdown(&mut actor).await;
    let pending = actor.service.has_pending_stored_message_actions();
    let after = fixture.disk();
    cleanup(&fixture, handle, actor).await;
    assert!(!pending);
    assert_eq!(before, after);
}

/// Replacing the private incarnation at the SAME public CID/pane must not
/// rebind a retained report. No lower reservation or physical write is issued.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_same_label_incarnation_replacement_refuses_source() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, turn) = super::native_receive::scene(&fixture).await;
    let envelope = bridge(&actor, &turn, "stale-bridge");
    let sequence = actor.service.message_service().next_message_sequence();
    assert!(
        !actor
            .service
            .accept_runtime_bridge_message(&turn, envelope, crate::runtime::current_unix_millis())
            .unwrap()
    );
    let mut shell = actor
        .service
        .agent_shell_store()
        .get(&turn.pane_id)
        .unwrap()
        .clone();
    shell.native_message_incarnation = Default::default();
    actor
        .service
        .agent_shell_store_mut()
        .restore_session(&turn.pane_id, shell)
        .unwrap();
    assert!(
        actor
            .service
            .prepare_stored_bridge_message(crate::runtime::current_unix_millis())
            .is_err()
    );
    let after = actor.service.message_service().next_message_sequence();
    let pending = actor.service.has_pending_stored_bridge_message();
    cleanup(&fixture, handle, actor).await;
    assert_eq!(sequence, after);
    assert!(!pending);
}

/// A real committed write with a lost worker result stays charged until exact
/// shutdown reconciliation; it must not publish or write a replacement source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_worker_loss_reconciles_original_on_shutdown() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, turn) = super::native_receive::scene(&fixture).await;
    let envelope = bridge(&actor, &turn, "lost-bridge");
    let sequence = actor.service.message_service().next_message_sequence();
    assert!(
        !actor
            .service
            .accept_runtime_bridge_message(&turn, envelope, crate::runtime::current_unix_millis())
            .unwrap()
    );
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    actor.poll_message_custody_for_tests();
    assert!(matches!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RetryAt(_)
    ));
    assert!(actor.service.message_service().has_unpublished_acceptance());
    let committed = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'a1:%:commit'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    shutdown(&mut actor).await;
    let pending = actor.service.message_service().has_unpublished_acceptance();
    let next = actor.service.message_service().next_message_sequence();
    let settled = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'a1:%:commit'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    cleanup(&fixture, handle, actor).await;
    assert_eq!((committed, settled), (1, 1));
    assert_eq!(next, sequence + 1);
    assert!(!pending);
}

/// A real persistent spawn emits started status while its new registration is
/// still waiting for the owned native-continuity write. That unaccepted source
/// must survive until acceptance, canonical input and exact ACK finish, without
/// turning reference delivery into parent inference authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_real_spawn_status_survives_registration_and_storage() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, _, parent) = super::native_receive::scene(&fixture).await;
    actor
        .service
        .remove_pending_agent_provider_task(&parent.turn_id);
    let spawn = mez_agent::AgentAction {
        id: "durable-bridge-child".into(),
        payload: mez_agent::AgentActionPayload::SpawnAgent {
            role: "default".into(),
            placement: "new-window".into(),
            cooperation_mode: "explore-only".into(),
            read_scopes: None,
            write_scopes: None,
            session_mode: None,
            size: None,
            reasoning_effort: None,
            lifetime: mez_agent::SubagentLifetime::Persistent,
            objective: Some("Review assigned work".into()),
            task_prompt: "Reply only when review is complete".into(),
        },
    };
    let result = actor
        .service
        .execute_spawn_action_for_turn(&parent, &spawn)
        .unwrap();
    let children = actor.service.agent_turn_ledger().turns().len() - 1;
    let mut references = 0;
    for _ in 0..32 {
        actor.poll_message_custody_for_tests();
        actor.observe_message_custody_for_tests().await.unwrap();
        references = actor.service.agent_turn_contexts()[&parent.turn_id]
            .chronology()
            .iter()
            .filter(|event| {
                event.block().source == mez_agent::ContextSourceKind::PeerMessage
                    && event.block().content.contains("subagent task started")
            })
            .count();
        if references == 1
            && !actor
                .service
                .native_input_pending_for_conversation(&parent.conversation_id)
        {
            break;
        }
    }
    let child = actor
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id != parent.turn_id)
        .unwrap()
        .clone();
    actor
        .service
        .agent_turn_ledger_mut()
        .finish_turn(&child.turn_id, mez_agent::AgentTurnState::Completed)
        .unwrap();
    actor
        .service
        .emit_subagent_task_result_for_state(&child, mez_agent::AgentTurnState::Completed)
        .unwrap();
    assert!(actor.service.has_pending_stored_bridge_message());
    let mut terminal_references = 0;
    for _ in 0..32 {
        actor.poll_message_custody_for_tests();
        actor.observe_message_custody_for_tests().await.unwrap();
        terminal_references = actor.service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .filter(|block| {
                block.source == mez_agent::ContextSourceKind::PeerMessage
                    && block.content.contains("subagent task completed")
            })
            .count();
        if terminal_references == 1
            && !actor
                .service
                .native_input_pending_for_conversation(&parent.conversation_id)
        {
            break;
        }
    }
    let sequence = actor.service.message_service().next_message_sequence();
    actor
        .service
        .emit_subagent_task_result_for_state(&child, mez_agent::AgentTurnState::Completed)
        .unwrap();
    assert_eq!(
        actor.service.message_service().next_message_sequence(),
        sequence
    );
    assert!(!actor.service.has_pending_stored_bridge_message());
    let offered = actor
        .service
        .agent_provider_task_is_pending(&parent.turn_id);
    actor.service.terminate_all_pane_processes().unwrap();
    drop((actor, handle));
    wait_until_unlocked(&fixture.path).await;
    std::fs::remove_dir_all(&fixture.path).unwrap();
    assert!(!fixture.path.exists());
    assert_eq!(result.status, mez_agent::ActionStatus::Succeeded);
    assert_eq!(children, 1);
    assert_eq!(terminal_references, 1);
    assert_eq!(
        references, 1,
        "actual started status was lost before durable acceptance"
    );
    assert!(
        !offered,
        "durable bridge reference cannot offer parent inference"
    );
}

/// Storage owns a real blocked worker while actor queries still progress. An
/// actual postcommit loss recovers on the original timer; a permanent marker
/// refusal stays unpublished and resolves only after qualified shutdown abort.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_active_recovery_and_refusal_preserve_control() {
    for refuse in [false, true] {
        let fixture = Fixture::new(true);
        let (handle, mut actor, _, turn) = super::native_receive::scene(&fixture).await;
        let target = actor.service.message_service_mut().register_agent(
            None,
            None,
            "bridge-observer",
            vec![],
        );
        actor
            .service
            .message_service_mut()
            .subscribe(&target.agent_id)
            .unwrap();
        let mut envelope = bridge(&actor, &turn, "active-original-bridge");
        envelope.recipient = Recipient::Agent(target.agent_id.clone());
        assert!(
            !actor
                .service
                .accept_runtime_bridge_message(
                    &turn,
                    envelope,
                    crate::runtime::current_unix_millis()
                )
                .unwrap()
        );
        if refuse {
            fixture.connection().execute_batch("CREATE TRIGGER fail_bridge_marker BEFORE INSERT ON records WHEN NEW.key LIKE 'a1:%:commit' BEGIN SELECT RAISE(ABORT,'bridge marker refusal'); END;").unwrap();
        } else {
            fixture
                .probe
                .panic_after_commit
                .store(true, Ordering::SeqCst);
        }
        let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
        *fixture.probe.gate.0.lock().unwrap() = false;
        actor.poll_message_custody_for_tests();
        let runner = tokio::spawn(actor.run());
        tokio::time::timeout(Duration::from_secs(5), async {
            while fixture.probe.attempts.load(Ordering::SeqCst) == attempts {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            handle.pending_agent_provider_tasks(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!runner.is_finished());
        assert!(
            handle
                .message_fanout_ready_for(
                    target.agent_id.clone(),
                    crate::runtime::current_unix_millis(),
                    10
                )
                .await
                .unwrap()
                .is_none()
        );
        fixture.probe.release();
        if !refuse {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(batch) = handle
                        .message_fanout_ready_for(
                            target.agent_id.clone(),
                            crate::runtime::current_unix_millis(),
                            10,
                        )
                        .await
                        .unwrap()
                        && batch
                            .batch
                            .batch
                            .messages
                            .iter()
                            .any(|message| message.envelope.id == "active-original-bridge")
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        handle.shutdown().await.unwrap();
        let mut actor = runner.await.unwrap();
        assert!(!actor.service.message_service().has_unpublished_acceptance());
        let commits = fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'a1:%:commit'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        actor.service.terminate_all_pane_processes().unwrap();
        drop((actor, handle));
        wait_until_unlocked(&fixture.path).await;
        std::fs::remove_dir_all(&fixture.path).unwrap();
        assert!(!fixture.path.exists());
        assert_eq!(commits, i64::from(!refuse));
    }
}
