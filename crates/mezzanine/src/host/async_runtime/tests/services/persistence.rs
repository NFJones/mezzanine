//! Async-runtime tests owned by persistence behavior.

use super::super::*;

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
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"audit_log""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
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
        .replay_for(&EventAudience::Primary);
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
        .replay_for(&EventAudience::Primary);
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
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}
