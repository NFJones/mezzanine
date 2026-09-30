//! Provider fork-history worker ownership and phase-ordering regressions.
//!
//! A held archive lock must delay only the captured read. Spawn application
//! and later issue writes remain actor-owned and fenced by separate leases.

use super::super::*;

/// A MAAP fork completion must leave the actor responsive while its archive
/// read waits, then allocate a child before admitting the later issue phase.
/// Replaying the old worker outcome must not allocate another child or issue.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_maap_fork_history_lock_preserves_phase_order() {
    let root = std::env::temp_dir().join(format!(
        "mez-maap-fork-lock-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let mut service = test_service();
    service.set_config_root(root.clone());
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    store
        .append(&mez_agent::TranscriptEntry {
            conversation_id: conversation.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::TranscriptRole::User,
            turn_id: "prior-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "prior parent row".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "fork and record an issue")
        .unwrap();
    let task = service.pending_agent_provider_tasks()[0].clone();
    let turn = service
        .agent_turn_ledger()
        .turn(&task.turn_id)
        .unwrap()
        .clone();
    let execution = fork_issue_execution(&task, &turn);
    let started = StdArc::new(tokio::sync::Notify::new());
    service.set_fork_history_started_for_tests(started.clone());
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            store
                .root()
                .join(".conversation-locks")
                .join(format!("{conversation}.lock")),
        )
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .record_claimed_agent_provider_task_for_tests(task.turn_id.clone(), 1)
            .await
            .unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
            turn_id: task.turn_id.clone(),
            claim_generation: 1,
            execution: Box::new(execution),
        }));
        let report =
            tokio::time::timeout(Duration::from_secs(2), handle.submit_runtime_events(batch))
                .await
                .expect("MAAP completion must not read the locked archive on actor")
                .unwrap();
        assert_eq!(report.applied, 1);
        let first_lease = handle
            .drain_timer_side_effects(64)
            .await
            .unwrap()
            .into_iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::ProviderPersistence =>
                {
                    Some(key)
                }
                _ => None,
            })
            .expect("fork read needs its own lease");
        let work = handle
            .drain_persistence_side_effects(64)
            .await
            .unwrap()
            .into_iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::SettleAgentProviderPersistence { work } => Some(work),
                _ => None,
            })
            .expect("completion must queue a fork-only phase");
        assert!(work.fork_read.is_some());
        let worker = tokio::task::spawn_blocking(move || {
            crate::runtime::execute_agent_provider_persistence_work(*work)
        });
        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), handle.lifecycle_state())
                .await
                .unwrap()
                .unwrap(),
            RuntimeLifecycleState::Running
        );
        assert!(
            !worker.is_finished(),
            "fork reader must wait for the held archive lock"
        );
        assert!(
            !root.join("issues.sqlite").exists(),
            "issue writes cannot precede spawn settlement"
        );
        drop(lock);
        let outcome = tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(outcome.memory_results.is_empty());
        assert!(outcome.issue_results.is_empty());
        assert!(
            outcome
                .fork_snapshot
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .any(|row| row.content == "prior parent row")
        );
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(
            AgentProviderEvent::PersistenceSettled {
                outcome: Box::new(outcome.clone()),
            },
        ));
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        let timers = handle.drain_timer_side_effects(64).await.unwrap();
        assert!(timers.iter().any(
            |effect| matches!(effect, RuntimeSideEffect::CancelTimer { key } if key == &first_lease)
        ));
        let second_lease = timers
            .into_iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::ProviderPersistence =>
                {
                    Some(key)
                }
                _ => None,
            })
            .expect("later issue phase needs a fresh lease");
        assert_ne!(second_lease.generation, first_lease.generation);
        let mut late = RuntimeEventBatch::new();
        late.push(RuntimeEvent::AgentProvider(
            AgentProviderEvent::PersistenceSettled {
                outcome: Box::new(outcome),
            },
        ));
        assert_eq!(handle.submit_runtime_events(late).await.unwrap().applied, 0);
        assert!(
            handle
                .drain_timer_side_effects(64)
                .await
                .unwrap()
                .is_empty(),
            "stale outcomes must not renew or cancel any settlement lease"
        );
        let work = handle
            .drain_persistence_side_effects(64)
            .await
            .unwrap()
            .into_iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::SettleAgentProviderPersistence { work } => Some(work),
                _ => None,
            })
            .expect("spawn must resume into the issue phase");
        assert!(work.fork_read.is_none());
        assert_eq!(
            work.execution.action_results[0].status,
            mez_agent::ActionStatus::Running
        );
        let spawn: serde_json::Value = serde_json::from_str(
            work.execution.action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(spawn["delivery_status"], "accepted");
        assert!(spawn["child_agent_id"].as_str().is_some());
        let outcome = tokio::task::spawn_blocking(move || {
            crate::runtime::execute_agent_provider_persistence_work(*work)
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(outcome.issue_results.len(), 1);
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(
            AgentProviderEvent::PersistenceSettled {
                outcome: Box::new(outcome),
            },
        ));
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(
        exit.service
            .agent_provider_persistence_progress_turn_ids()
            .next()
            .is_none()
    );
    let connection = rusqlite::Connection::open(root.join("issues.sqlite")).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    exit.service.terminate_all_pane_processes().unwrap();
    drop(connection);
    let _ = std::fs::remove_dir_all(root);
}

/// Terminal provider bookkeeping must wait on the worker, not the actor. The
/// turn remains owned until checked history returns; only then may transcript
/// rows be admitted and terminal state settled. Duplicate outcomes are inert.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_terminal_bookkeeping_lock_preserves_ownership() {
    run_terminal_bookkeeping_lock(false).await;
}

/// Expiring a bookkeeping lease while its archive stays locked must not send
/// failure containment back through an actor-owned read of the same archive.
/// The failed turn's chronology remains queued and late old outcomes are inert.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_terminal_bookkeeping_timeout_does_not_read_archive() {
    run_terminal_bookkeeping_lock(true).await;
}

/// Exercises normal acceptance and deadline containment against one held lock.
async fn run_terminal_bookkeeping_lock(expire: bool) {
    let root = std::env::temp_dir().join(format!(
        "mez-bookkeeping-lock-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    store
        .append(&mez_agent::TranscriptEntry {
            conversation_id: conversation.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::TranscriptRole::User,
            turn_id: "prior-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "prior history".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "finish with an answer")
        .unwrap();
    let task = service.pending_agent_provider_tasks()[0].clone();
    let turn = service
        .agent_turn_ledger()
        .turn(&task.turn_id)
        .unwrap()
        .clone();
    let mut execution = fork_issue_execution(&task, &turn);
    let action = mez_agent::AgentAction {
        id: "answer".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: "Checked terminal answer.".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    execution.response.action_batch.as_mut().unwrap().actions = vec![action.clone()];
    execution.action_results = vec![mez_agent::ActionResult::succeeded(
        &turn,
        &action,
        vec!["Checked terminal answer.".to_string()],
        None,
    )];
    execution.final_turn = true;
    execution.terminal_state = mez_agent::AgentTurnState::Completed;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            store
                .root()
                .join(".conversation-locks")
                .join(format!("{conversation}.lock")),
        )
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .record_claimed_agent_provider_task_for_tests(task.turn_id.clone(), 1)
            .await
            .unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
            turn_id: task.turn_id.clone(),
            claim_generation: 1,
            execution: Box::new(execution),
        }));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), handle.submit_runtime_events(batch))
                .await
                .expect("terminal completion must not read the locked archive on actor")
                .unwrap()
                .applied,
            1
        );
        assert!(handle.agent_turn_is_running(&task.turn_id).await.unwrap());
        let effects = handle.drain_persistence_side_effects(64).await.unwrap();
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, RuntimeSideEffect::PersistTranscriptEntries { .. }))
        );
        let work = effects
            .into_iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::SettleAgentProviderPersistence { work } => Some(work),
                _ => None,
            })
            .expect("terminal completion must queue checked bookkeeping");
        assert!(work.bookkeeping_read.is_some());
        let (started, worker_started) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            crate::runtime::execute_agent_provider_persistence_work(*work)
        });
        worker_started.await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), handle.lifecycle_state())
                .await
                .unwrap()
                .unwrap(),
            RuntimeLifecycleState::Running
        );
        assert!(!worker.is_finished());
        if expire {
            let key = handle
                .drain_timer_side_effects(64)
                .await
                .unwrap()
                .into_iter()
                .find_map(|effect| match effect {
                    RuntimeSideEffect::ScheduleTimer { key, .. }
                        if key.kind == RuntimeTimerKind::ProviderPersistence =>
                    {
                        Some(key)
                    }
                    _ => None,
                })
                .expect("bookkeeping must have a lease");
            let mut expired = RuntimeEventBatch::new();
            expired.push(RuntimeEvent::Timer(TimerEvent { key, now_ms: 1 }));
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    handle.submit_runtime_events(expired)
                )
                .await
                .expect("timeout containment must not read the locked archive")
                .unwrap()
                .applied,
                1
            );
            assert!(!handle.agent_turn_is_running(&task.turn_id).await.unwrap());
            assert_eq!(
                handle.lifecycle_state().await.unwrap(),
                RuntimeLifecycleState::Running
            );
        }
        drop(lock);
        let outcome = tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(
            AgentProviderEvent::PersistenceSettled {
                outcome: Box::new(outcome.clone()),
            },
        ));
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            usize::from(!expire)
        );
        assert!(!handle.agent_turn_is_running(&task.turn_id).await.unwrap());
        if expire {
            let rows = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let rows = handle
                        .drain_persistence_side_effects(64)
                        .await
                        .unwrap()
                        .into_iter()
                        .filter_map(|effect| match effect {
                            RuntimeSideEffect::PersistTranscriptEntries { entries, .. } => {
                                Some(entries)
                            }
                            _ => None,
                        })
                        .flatten()
                        .collect::<Vec<_>>();
                    if !rows.is_empty() {
                        break rows;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("captured failure chronology must be admitted after lock release");
            assert!(rows.iter().any(|row| {
                row.content
                    .contains("persistence worker result was not delivered")
            }));
            let mut late = RuntimeEventBatch::new();
            late.push(RuntimeEvent::AgentProvider(
                AgentProviderEvent::PersistenceSettled {
                    outcome: Box::new(outcome),
                },
            ));
            assert_eq!(handle.submit_runtime_events(late).await.unwrap().applied, 0);
            handle.shutdown().await.unwrap();
            return;
        }
        let rows = handle
            .drain_persistence_side_effects(64)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|effect| match effect {
                RuntimeSideEffect::PersistTranscriptEntries { entries, .. } => Some(entries),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert!(!rows.is_empty());
        assert_eq!(rows.first().unwrap().sequence, 2);
        assert_eq!(
            rows.iter()
                .filter(|row| row.role == mez_agent::TranscriptRole::Assistant
                    && row
                        .content
                        .lines()
                        .any(|line| line == "Checked terminal answer."))
                .count(),
            1
        );
        let mut late = RuntimeEventBatch::new();
        late.push(RuntimeEvent::AgentProvider(
            AgentProviderEvent::PersistenceSettled {
                outcome: Box::new(outcome),
            },
        ));
        assert_eq!(handle.submit_runtime_events(late).await.unwrap().applied, 0);
        assert!(
            handle
                .drain_persistence_side_effects(64)
                .await
                .unwrap()
                .is_empty()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Interruption chronology remains conversation-owned when `/new` replaces its
/// pane binding during a locked worker read. Neither command may block on the
/// archive, and acceptance must create a receipt for the original conversation.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_interruption_bookkeeping_survives_locked_replacement() {
    let root = std::env::temp_dir().join(format!(
        "mez-interruption-replacement-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let original = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    store
        .append(&mez_agent::TranscriptEntry {
            conversation_id: original.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::TranscriptRole::User,
            turn_id: "prior-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "prior history".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "retain stopped prompt across replacement")
        .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            store
                .root()
                .join(".conversation-locks")
                .join(format!("{original}.lock")),
        )
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let stopped = tokio::time::timeout(
            Duration::from_secs(2),
            handle.execute_agent_shell_command(primary.clone(), "/stop".to_string()),
        )
        .await
        .expect("interruption must not read the archive on actor")
        .unwrap();
        assert!(stopped.contains("state=cancelled"), "{stopped}");
        let replaced = tokio::time::timeout(
            Duration::from_secs(2),
            handle.execute_agent_shell_command(primary.clone(), "/new".to_string()),
        )
        .await
        .expect("replacement must not wait for old archive checking")
        .unwrap();
        assert!(replaced.contains("new=true"), "{replaced}");
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        drop(lock);
        let rows = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let rows = handle
                    .drain_persistence_side_effects(64)
                    .await
                    .unwrap()
                    .into_iter()
                    .filter_map(|effect| match effect {
                        RuntimeSideEffect::PersistTranscriptEntries { entries, .. } => {
                            Some(entries)
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                if !rows.is_empty() {
                    break rows;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original interruption must enter the append lane after lock release");
        assert!(rows.iter().all(|row| row.conversation_id == original));
        assert!(rows.iter().any(|row| {
            row.content
                .contains("retain stopped prompt across replacement")
        }));
        assert!(!store.pending_append_receipts().unwrap().is_empty());
        store.append_many(&rows).unwrap();
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    let replacement = exit.service.agent_shell_store().get("%1").unwrap();
    assert_ne!(replacement.session_id, original);
    assert_eq!(replacement.transcript_entries, 0);
    assert!(store.inspect(&original).unwrap().iter().any(|row| {
        row.content
            .contains("retain stopped prompt across replacement")
    }));
    exit.service.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Builds a provider execution with a fork before a durable issue action.
fn fork_issue_execution(
    task: &crate::runtime::RuntimeAgentProviderTask,
    turn: &mez_agent::AgentTurnRecord,
) -> mez_agent::AgentTurnExecution {
    let actions = vec![
        mez_agent::AgentAction {
            id: "fork".to_string(),
            payload: mez_agent::AgentActionPayload::SpawnAgent {
                role: "explorer".to_string(),
                placement: "new-window".to_string(),
                cooperation_mode: "explore-only".to_string(),
                read_scopes: None,
                write_scopes: None,
                session_mode: Some(mez_agent::SubagentSessionMode::Fork),
                size: None,
                reasoning_effort: None,
                lifetime: mez_agent::SubagentLifetime::Task,
                objective: None,
                task_prompt: "inspect inherited work".to_string(),
            },
        },
        mez_agent::AgentAction {
            id: "issue".to_string(),
            payload: mez_agent::AgentActionPayload::IssueAdd {
                kind: "task".to_string(),
                state: None,
                priority: None,
                title: "after fork".to_string(),
                body: None,
                notes: None,
                depends_on: Vec::new(),
            },
        },
    ];
    mez_agent::AgentTurnExecution {
        request: mez_agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            model_capabilities: Default::default(),
            max_input_tokens: None,
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            thinking_enabled: task.model_profile.thinking_enabled(),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::all_enabled(),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "fork and record an issue".to_string(),
            }]
            .into(),
        },
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "fork and issue".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "inherit history before recording issue".to_string(),
                actions: actions.clone(),
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: actions
            .iter()
            .map(|action| mez_agent::ActionResult::running(turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Running,
    }
}
