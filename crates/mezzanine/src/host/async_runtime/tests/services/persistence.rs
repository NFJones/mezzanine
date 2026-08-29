//! Async-runtime tests owned by persistence behavior.

use super::super::*;
use crate::storage::transcript::AgentPresentationEntry;

/// Verifies that persistence side effects are owned by a concrete Tokio worker
/// instead of the actor. The worker writes the bytes and reports completion back
/// through typed event ingress so later audit, transcript, snapshot, and config
/// migrations can share the same boundary.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_writes_bytes_and_reports_completion() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-complete-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("audit.jsonl");
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                target: PersistenceTarget::AuditLog,
                path: path.clone(),
                bytes: b"{\"event\":\"worker\"}\n".to_vec(),
                mode: PersistenceWriteMode::Append,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.bytes_written, 19);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"event\":\"worker\"}\n"
        );
        let metrics = handle.metrics().await.unwrap();
        assert_eq!(
            metrics
                .phase_latency(AsyncRuntimeLatencyPhase::PersistenceOperation)
                .observations,
            1
        );
        assert_eq!(
            metrics
                .phase_latency(AsyncRuntimeLatencyPhase::PersistenceBatch)
                .observations,
            1
        );
        #[cfg(unix)]
        {
            assert_eq!(unix_mode(&root), 0o700);
            assert_eq!(unix_mode(&path), 0o600);
        }
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"audit_log""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies adjacent audit appends with one destination and retention policy
/// share a single durability and retention batch without losing per-record
/// completion accounting or chronological JSONL order.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_batches_compatible_audit_appends() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-audit-batch-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("audit.jsonl");
    let retention = crate::security::audit::AuditRetentionPolicy {
        max_age_days: None,
        max_records: Some(2),
        max_bytes: None,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        let effects = (1..=3)
            .map(|event_id| RuntimeSideEffect::PersistAuditLog {
                path: path.clone(),
                bytes: format!("{{\"event_id\":{event_id}}}\n").into_bytes(),
                retention: retention.clone(),
            })
            .collect();
        assert_eq!(handle.queue_runtime_side_effects(effects).await.unwrap(), 3);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();

        assert_eq!(report.drained, 3);
        assert_eq!(report.audit_batches, 1);
        assert_eq!(report.completed, 3);
        assert_eq!(report.failed, 0);
        assert_eq!(report.submitted_events, 3);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"event_id\":2}\n{\"event_id\":3}\n"
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies retention scheduling survives separate drain polls and only scans
/// initially and after the configured record threshold is crossed.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_schedules_retention_across_drains() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-audit-schedule-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("audit.jsonl");
    let retention = crate::security::audit::AuditRetentionPolicy {
        max_age_days: None,
        max_records: Some(2),
        max_bytes: None,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        let effects = (1..=3)
            .map(|event_id| RuntimeSideEffect::PersistAuditLog {
                path: path.clone(),
                bytes: format!("{{\"event_id\":{event_id}}}\n").into_bytes(),
                retention: retention.clone(),
            })
            .collect();
        assert_eq!(handle.queue_runtime_side_effects(effects).await.unwrap(), 3);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 1,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 4,
        )
        .await
        .unwrap();

        assert_eq!(report.drained, 3);
        assert_eq!(report.audit_batches, 3);
        assert_eq!(report.audit_retention_runs, 2);
        assert_eq!(report.completed, 3);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"event_id\":2}\n{\"event_id\":3}\n"
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies durable provider usage is appended by the persistence worker and
/// reported through typed actor ingress instead of touching SQLite in actor
/// settlement.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_appends_token_usage() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-token-usage-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = crate::storage::token_usage::TokenUsageStore::new(root.join("token-usage.sqlite"));
    store.initialize(100).unwrap();
    let event = crate::storage::token_usage::TokenUsageEvent {
        id: "provider-settlement-usage".to_string(),
        observed_at_unix_seconds: 100,
        model: mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 7,
            output_tokens: 3,
            ..Default::default()
        },
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTokenUsage {
                store: store.clone(),
                event,
            }])
            .await
            .unwrap();
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();

        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        let totals = store.aggregate_windows(100, &[7]).unwrap();
        let key = mez_agent::ModelTokenUsageKey::new("openai", "gpt-test");
        assert_eq!(totals[&7][&key].input_tokens, 7);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""target":"token_usage""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies SQLite writer contention remains isolated to the blocking
/// persistence worker while the serialized actor continues serving lifecycle
/// heartbeats. Releasing the lock then allows the queued accounting append to
/// complete without losing the durable event.
#[tokio::test(flavor = "current_thread")]
async fn async_token_usage_lock_contention_does_not_block_actor_heartbeats() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-token-usage-lock-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = crate::storage::token_usage::TokenUsageStore::new(root.join("token-usage.sqlite"));
    store.initialize(100).unwrap();
    let lock = rusqlite::Connection::open(store.path()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let event = crate::storage::token_usage::TokenUsageEvent {
        id: "provider-settlement-lock".to_string(),
        observed_at_unix_seconds: 100,
        model: mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 11,
            ..Default::default()
        },
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTokenUsage {
                store: store.clone(),
                event,
            }])
            .await
            .unwrap();
        let worker = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        );
        let heartbeat = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let lifecycle =
                tokio::time::timeout(Duration::from_millis(50), handle.lifecycle_state())
                    .await
                    .expect("actor heartbeat must not wait for the SQLite writer lock")
                    .unwrap();
            lock.execute_batch("COMMIT").unwrap();
            lifecycle
        };

        let (report, lifecycle) = tokio::join!(worker, heartbeat);
        let report = report.unwrap();
        assert_eq!(lifecycle, RuntimeLifecycleState::Running);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        let totals = store.aggregate_windows(100, &[7]).unwrap();
        let key = mez_agent::ModelTokenUsageKey::new("openai", "gpt-test");
        assert_eq!(totals[&7][&key].input_tokens, 11);
        handle.shutdown().await.unwrap();
    };

    let ((), _exit) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that an idle persistence worker wakes on actor lifecycle
/// notifications before its bounded idle probe interval elapses. This covers
/// the shared side-effect worker wait primitive used by persistence, hooks,
/// render, client-output flushing, and generic side-effect drains.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_persistence_side_effect_service_wakes_on_lifecycle_change_without_idle_poll() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let worker_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let client = async move {
        let worker = tokio::spawn(async move {
            run_async_persistence_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                },
                |_, state| {
                    matches!(
                        state,
                        RuntimeLifecycleState::Stopping
                            | RuntimeLifecycleState::Killed
                            | RuntimeLifecycleState::Failed
                    )
                },
            )
            .await
            .unwrap()
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(
            !worker.is_finished(),
            "idle persistence worker should not wake before its idle probe interval"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "persistence lifecycle wake test".to_string(),
            force: true,
            failed: false,
        }));
        shutdown_handle.submit_runtime_events(batch).await.unwrap();
        let report = tokio::time::timeout(Duration::from_millis(250), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 0);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Killed);
        shutdown_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that persistence write modes can preserve create-new semantics for
/// future snapshot and default-config migrations. The first create-new write
/// succeeds, the second conflicting write is reported as a typed persistence
/// failure, and the original private file contents remain intact.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_honors_create_new_mode() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-create-new-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("config.toml");
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::Persist {
                    target: PersistenceTarget::Config,
                    path: path.clone(),
                    bytes: b"first\n".to_vec(),
                    mode: PersistenceWriteMode::CreateNew,
                },
                RuntimeSideEffect::Persist {
                    target: PersistenceTarget::Config,
                    path: path.clone(),
                    bytes: b"second\n".to_vec(),
                    mode: PersistenceWriteMode::CreateNew,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 2);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 2);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.bytes_written, 6);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
        #[cfg(unix)]
        {
            assert_eq!(unix_mode(&root), 0o700);
            assert_eq!(unix_mode(&path), 0o600);
        }
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that persistence worker write failures become diagnostic runtime
/// events instead of crashing the worker or daemon supervisor. This keeps
/// latency-sensitive persistence paths debuggable while preserving actor
/// ownership of visible error state.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_reports_failures_without_crashing() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-failed-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                target: PersistenceTarget::Config,
                path: root.clone(),
                bytes: b"will fail".to_vec(),
                mode: PersistenceWriteMode::Replace,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.bytes_written, 0);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies async persistence side effect service rejects config symlink destinations.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_rejects_config_symlink_destinations() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-symlink-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let link_path = root.join("config.toml");
    let linked_target = root.join("linked-target.toml");
    std::os::unix::fs::symlink(&linked_target, &link_path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                target: PersistenceTarget::Config,
                path: link_path.clone(),
                bytes: b"secret = true\n".to_vec(),
                mode: PersistenceWriteMode::Replace,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 0);
        assert_eq!(report.failed, 1);
        assert!(!linked_target.exists());
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies adjacent presentation effects for one conversation are sequenced
/// and persisted as one ordered worker batch.
///
/// The actor supplies immutable unsequenced entries. Coalescing must stop at a
/// conversation boundary while preserving entry order and emitting one
/// persistence completion for the combined append.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_batches_adjacent_presentations() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-presentation-batch-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let presentation = |turn_id: &str, created_at_unix_seconds| AgentPresentationEntry {
        conversation_id: "worker-presentation".to_string(),
        sequence: 0,
        created_at_unix_seconds,
        pane_id: "%9".to_string(),
        turn_id: Some(turn_id.to_string()),
        terminal_width: 80,
        style_names: vec!["assistant".to_string()],
        display_lines: vec![format!("mez> {turn_id}")],
        copy_lines: Vec::new(),
        ansi_text: None,
        source_text: None,
        source_content_type: None,
    };
    let path = store.presentation_path("worker-presentation").unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        assert_eq!(
            handle
                .queue_runtime_side_effects(vec![
                    RuntimeSideEffect::PersistPresentationEntries {
                        store: store.clone(),
                        path: path.clone(),
                        entries: vec![presentation("turn-1", 10)],
                    },
                    RuntimeSideEffect::PersistPresentationEntries {
                        store: store.clone(),
                        path: path.clone(),
                        entries: vec![presentation("turn-2", 11)],
                    },
                ])
                .await
                .unwrap(),
            2
        );
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();

        assert_eq!(report.drained, 2);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert!(report.bytes_written > 0);
        let entries = store.inspect_presentation("worker-presentation").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sequence, 1);
        assert_eq!(entries[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(entries[1].sequence, 2);
        assert_eq!(entries[1].turn_id.as_deref(), Some("turn-2"));
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies saved-session archival executes on the blocking persistence worker
/// and returns a typed lifecycle completion through the actor event boundary.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_archives_saved_sessions() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-session-archive-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "worker-archive".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-worker-archive".to_string(),
            agent_id: "agent-worker".to_string(),
            pane_id: "%9".to_string(),
            content: "archive on the persistence worker".to_string(),
        })
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        assert_eq!(
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistSessionArchive {
                    store: store.clone(),
                    conversation_id: "worker-archive".to_string(),
                    operation: SessionArchiveOperation::Archive {
                        archived_at_unix_seconds: 20,
                    },
                }])
                .await
                .unwrap(),
            1
        );
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert!(report.bytes_written > 0);
        assert!(root.join("archived/worker-archive.tar.zst").is_file());
        assert!(!root.join("worker-archive").exists());
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""target":"session_archive""#)
            && event.payload.contains(r#""operation":"archive""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies actor startup runs retention only after live durable bindings are
/// available, protects those conversations, and schedules the next daily pass.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_enforces_saved_session_retention() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-session-retention-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let mut store = AgentTranscriptStore::new(root.clone());
    store
        .set_saved_session_retention_policy(
            crate::storage::transcript::SavedSessionRetentionPolicy {
                max_active_sessions: 10,
                retention_days: 1,
            },
        )
        .unwrap();
    for conversation_id in ["protected-old", "expired-old"] {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: 1,
                created_at_unix_seconds: 1,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-{conversation_id}"),
                agent_id: "agent-retention".to_string(),
                pane_id: "%9".to_string(),
                content: "old retained session".to_string(),
            })
            .unwrap();
    }
    let mut service = test_service_with_event_log();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "protected-old", 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert!(store.saved_session("protected-old").unwrap().is_some());
        assert!(store.saved_session("expired-old").unwrap().is_none());
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        let timer_key = match timers.as_slice() {
            [RuntimeSideEffect::ScheduleTimer { key, delay_ms }]
                if key.kind == RuntimeTimerKind::SavedSessionRetention
                    && *delay_ms == 24 * 60 * 60 * 1_000 =>
            {
                key.clone()
            }
            _ => panic!("missing daily retention timer: {timers:?}"),
        };
        let mut timer_batch = RuntimeEventBatch::new();
        timer_batch.push(RuntimeEvent::Timer(TimerEvent {
            now_ms: timer_key.generation,
            key: timer_key,
        }));
        let timer_report = handle.submit_runtime_events(timer_batch).await.unwrap();
        assert_eq!(timer_report.applied, 1);
        let periodic = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(periodic.drained, 1);
        assert_eq!(periodic.completed, 1);
        assert_eq!(periodic.failed, 0);
        assert!(matches!(
            handle.drain_timer_side_effects(8).await.unwrap().as_slice(),
            [RuntimeSideEffect::ScheduleTimer { key, delay_ms }]
                if key.kind == RuntimeTimerKind::SavedSessionRetention
                    && *delay_ms == 24 * 60 * 60 * 1_000
        ));
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event
            .payload
            .contains(r#""target":"saved_session_retention""#)
            && event.payload.contains(r#""deleted":1"#)
    }));
    let _ = std::fs::remove_dir_all(root);
}
