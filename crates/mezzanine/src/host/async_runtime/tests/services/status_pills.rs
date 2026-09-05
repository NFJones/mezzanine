//! Async-runtime tests owned by command-backed status-pill behavior.

use super::super::*;
use crate::host::async_runtime::run_async_status_pill_side_effect_service;

/// Verifies slow status-pill helpers run concurrently outside serialized actor
/// ownership while actor heartbeats remain responsive.
#[tokio::test(flavor = "current_thread")]
async fn async_status_pill_worker_does_not_block_actor_heartbeats() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let root = std::env::temp_dir().join(format!(
        "mez-status-pill-worker-gates-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let plans = ["first", "second"].map(|name| {
        let started = root.join(format!("{name}.started"));
        let release = root.join(format!("{name}.release"));
        crate::runtime::RuntimeStatusPillRefreshPlan::for_tests(
            name,
            1,
            &format!(
                "printf started > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf ready",
                started.display(),
                release.display()
            ),
            1_000,
            32,
        )
    });

    let client = async {
        handle
            .queue_runtime_side_effects(
                plans
                    .into_iter()
                    .map(|plan| RuntimeSideEffect::RefreshStatusPill { plan })
                    .collect(),
            )
            .await
            .unwrap();
        let worker_handle = handle.clone();
        let worker = async move {
            run_async_status_pill_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 2,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                },
                |polls, _| polls >= 2,
            )
            .await
            .unwrap()
        };
        let heartbeat = async {
            let started = [root.join("first.started"), root.join("second.started")];
            let deadline = Instant::now() + Duration::from_secs(1);
            while !started.iter().all(|path| path.is_file()) {
                assert!(
                    Instant::now() < deadline,
                    "status-pill helpers did not reach their test gates"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let lifecycle =
                tokio::time::timeout(Duration::from_millis(50), handle.lifecycle_state())
                    .await
                    .expect("actor heartbeat should not wait for status-pill helpers")
                    .unwrap();
            for name in ["first", "second"] {
                std::fs::write(root.join(format!("{name}.release")), b"release").unwrap();
            }
            lifecycle
        };

        let (report, lifecycle) = tokio::join!(worker, heartbeat);

        assert_eq!(lifecycle, RuntimeLifecycleState::Running);
        assert_eq!(report.drained, 2);
        assert_eq!(report.submitted_events, 2);
        assert_eq!(report.applied_events, 0);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 5);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies an actor render schedules one refresh, the worker applies its
/// typed completion, and only the changed cached value invalidates status.
#[tokio::test(flavor = "current_thread")]
async fn async_status_pill_render_completion_updates_cached_status() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-status-pill".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r##"[frames.window]
right_status = "#{pill.used}"
[frames.window.pills.used]
label = "USED"
command = "printf ready"
interval_seconds = 60
initial = "boot"
timeout_ms = 1000
"##
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let initial = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(initial.lines.iter().any(|line| line.contains("USED boot")));

        let report = run_async_status_pill_side_effect_service(
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
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::StatusLine,
            }]
        );

        let updated = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(updated.lines.iter().any(|line| line.contains("USED ready")));
        assert!(
            handle
                .drain_status_pill_side_effects(8)
                .await
                .unwrap()
                .is_empty()
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies a fast legacy window pill is applied while a larger pane-provider
/// queue remains blocked. Pane workers must stay continuously bounded without
/// batching window completion behind the slowest pane chunk.
#[tokio::test(flavor = "current_thread")]
async fn async_status_pill_worker_preserves_window_latency_during_pane_backlog() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "mixed-status-pill".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r##"[frames.window]
right_status = "#{pill.used}"
[frames.window.pills.used]
label = "USED"
command = "printf ready"
interval_seconds = 60
initial = "boot"
timeout_ms = 1000
"##
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let root = std::env::temp_dir().join(format!(
        "mez-mixed-status-pill-worker-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let release = root.join("release");

    let client = async {
        let pane_effects = (0..5)
            .map(|index| {
                let started = root.join(format!("pane-{index}.started"));
                let command = format!(
                    "printf started > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf pane-{index}",
                    started.display(),
                    release.display()
                );
                RuntimeSideEffect::RefreshPaneStatusProvider {
                    plan: Box::new(
                        crate::runtime::RuntimePaneStatusProviderRefreshPlan::for_tests(
                            &format!("pane-{index}"),
                            &command,
                            &root,
                            5_000,
                        ),
                    ),
                }
            })
            .collect::<Vec<_>>();
        handle
            .queue_runtime_side_effects(pane_effects)
            .await
            .unwrap();

        let initial = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(initial.lines.iter().any(|line| line.contains("USED boot")));

        let worker_handle = handle.clone();
        let worker = async move {
            run_async_status_pill_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 2,
                    drain_limit: 16,
                    idle_interval: Duration::from_millis(1),
                },
                |polls, _| polls >= 2,
            )
            .await
            .unwrap()
        };
        let observe_window = async {
            let started = (0..4)
                .map(|index| root.join(format!("pane-{index}.started")))
                .collect::<Vec<_>>();
            let start_deadline = Instant::now() + Duration::from_secs(2);
            while !started.iter().all(|path| path.is_file()) && Instant::now() < start_deadline {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let render_deadline = Instant::now() + Duration::from_secs(2);
            let mut applied_before_release = false;
            while Instant::now() < render_deadline {
                let view = handle
                    .render_client_view(
                        ClientViewRole::Primary,
                        Size::new(80, 24).unwrap(),
                        TerminalClientLoopConfig::default(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                if view.lines.iter().any(|line| line.contains("USED ready")) {
                    applied_before_release = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            std::fs::write(&release, b"release").unwrap();
            applied_before_release
        };

        let (report, applied_before_release) = tokio::join!(worker, observe_window);
        assert!(
            applied_before_release,
            "window completion waited for pane backlog"
        );
        assert_eq!(report.drained, 6);
        assert_eq!(report.submitted_events, 6);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 10);
    let _ = std::fs::remove_dir_all(root);
    let _ = primary;
}

/// Dropping the service future must cancel both running and queued provider
/// plans, even without a terminal lifecycle event or an actor acknowledgement.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_status_provider_service_drop_cancels_owned_batch() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let plans = (0..5)
        .map(|index| {
            crate::runtime::RuntimePaneStatusProviderRefreshPlan::for_tests(
                &format!("drop-{index}"),
                "sleep 2",
                &std::env::temp_dir(),
                5_000,
            )
        })
        .collect::<Vec<_>>();
    let fences = plans
        .iter()
        .map(|plan| plan.cancellation.clone())
        .collect::<Vec<_>>();
    let client = async {
        handle
            .queue_runtime_side_effects(
                plans
                    .into_iter()
                    .map(|plan| RuntimeSideEffect::RefreshPaneStatusProvider {
                        plan: Box::new(plan),
                    })
                    .collect(),
            )
            .await
            .unwrap();
        let worker = run_async_status_pill_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), worker)
                .await
                .is_err()
        );
        assert!(fences.iter().all(|fence| fence.is_cancelled()));
        handle.shutdown().await.unwrap();
    };
    tokio::join!(client, actor.run());
}

/// Verifies terminal lifecycle shutdown cancels an already-dispatched blocking
/// pane provider, kills its private process group, and waits for the worker to
/// reap the direct child without requiring another render pass.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_status_provider_shutdown_kills_gated_process() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let root = std::env::temp_dir().join(format!(
        "mez-pane-status-shutdown-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let started = root.join("started");
    let pid_path = root.join("pid");
    let command = format!(
        "printf '%s' \"$$\" > '{}'; printf started > '{}'; while :; do sleep 1; done",
        pid_path.display(),
        started.display()
    );
    let plan = crate::runtime::RuntimePaneStatusProviderRefreshPlan::for_tests(
        "blocked", &command, &root, 60_000,
    );

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RefreshPaneStatusProvider {
                plan: Box::new(plan),
            }])
            .await
            .unwrap();
        let worker_handle = handle.clone();
        let worker = async move {
            run_async_status_pill_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
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
        };
        let shutdown = async {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !started.is_file() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(
                started.is_file(),
                "pane provider never reached its process gate"
            );
            let pid = std::fs::read_to_string(&pid_path)
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap();
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
                reason: "pane status worker lifecycle regression".to_string(),
                force: true,
                failed: false,
            }));
            handle.submit_runtime_events(batch).await.unwrap();
            pid
        };

        let (report, pid) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(worker, shutdown)
        })
        .await
        .expect("shutdown should cancel and reap the blocking pane provider");
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Killed);
        let gone_deadline = Instant::now() + Duration::from_secs(1);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < gone_deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "provider process survived shutdown"
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}
