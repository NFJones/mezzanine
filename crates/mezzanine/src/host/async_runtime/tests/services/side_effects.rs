//! Async-runtime tests owned by side effects behavior.

use super::super::*;

/// Verifies that queued runtime side effects can be consumed by a supervised
/// async service without taking mutable access to the runtime service itself.
/// This is the worker-side half of the actor side-effect boundary and prevents
/// later render, pane I/O, provider, hook, and persistence workers from growing
/// bespoke drain loops.
#[tokio::test(flavor = "current_thread")]
async fn async_side_effect_service_drains_actor_queue() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let applied = StdArc::new(Mutex::new(Vec::new()));

    let client = {
        let applied = applied.clone();
        async move {
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Pane(PaneEvent::Output {
                pane_id: "%1".to_string(),
                bytes: b"worker-side-effect-output\n".to_vec(),
            }));
            let report = handle.submit_runtime_events(batch).await.unwrap();
            assert_eq!(report.side_effects, 1);

            let side_effect_report = run_async_runtime_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                },
                |effect| {
                    applied.lock().unwrap().push(effect);
                    Ok(())
                },
                |_, _| false,
            )
            .await
            .unwrap();
            assert_eq!(side_effect_report.polls, 1);
            assert_eq!(side_effect_report.drained, 1);
            assert_eq!(side_effect_report.applied, 1);
            assert_eq!(
                handle.shutdown().await.unwrap(),
                RuntimeLifecycleState::Running
            );
        }
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        *applied.lock().unwrap(),
        vec![RuntimeSideEffect::RenderClient {
            client_id: primary,
            reason: RenderInvalidationReason::PaneOutput,
        }]
    );
    assert_eq!(exit.commands_processed, 3);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a bounded side-effect worker exits immediately after its
/// final empty poll instead of sleeping for the idle fallback interval. This
/// protects tests, supervised short runs, and shutdown paths from an avoidable
/// extra delay when no side effects are queued.
#[tokio::test(flavor = "current_thread")]
async fn async_side_effect_service_exits_after_final_empty_poll_without_sleep() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_runtime_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                },
                |_| Ok(()),
                |_, _| false,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 0);
        assert_eq!(report.applied, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 2);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that an unbounded side-effect worker still performs a bounded idle
/// actor-state probe. Runtime side-effect notifications are the fast path, but
/// this probe prevents a missed retained notification permit from stranding
/// queued side effects in a long-lived daemon.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_side_effect_service_uses_bounded_idle_probe_when_unbounded() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let worker_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let client = async move {
        let worker = tokio::spawn(async move {
            run_async_runtime_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(10),
                },
                |_| Ok(()),
                |polls, _| polls >= 2,
            )
            .await
            .unwrap()
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(9)).await;
        tokio::task::yield_now().await;
        assert!(
            !worker.is_finished(),
            "side-effect service should wait until its configured idle probe interval"
        );
        tokio::time::advance(Duration::from_millis(11)).await;
        tokio::task::yield_now().await;

        let report = worker.await.unwrap();
        assert_eq!(
            shutdown_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let (report, mut exit) = tokio::join!(client, actor.run());
    assert_eq!(report.polls, 2);
    assert_eq!(report.drained, 0);
    assert_eq!(report.applied, 0);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a filtered side-effect drain re-notifies retained work for
/// another worker. Prompt submission can queue timer/persistence/render work
/// beside provider dispatch work; if one worker consumes the original
/// notification and retains the provider dispatch, the next worker must be
/// woken immediately instead of waiting for its idle probe.
#[tokio::test(flavor = "current_thread")]
async fn async_filtered_side_effect_drain_renotifies_retained_work() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let agent_id = AgentId::opaque("agent-%1").unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1),
                    delay_ms: 1,
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id,
                    turn_id: "turn-retained".to_string(),
                },
            ])
            .await
            .unwrap();
        handle.wait_for_runtime_side_effects().await;

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        tokio::time::timeout(
            Duration::from_millis(50),
            handle.wait_for_runtime_side_effects(),
        )
        .await
        .expect("retained side-effect work should be re-notified immediately");

        let retained = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(retained.len(), 1);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.side_effect_delivery_notifications >= 2);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that side-effect delivery revisions wake every worker watching the
/// queue instead of acting like a single consumable permit. Side-effect
/// families run as independent workers, so a provider dispatch must not wait
/// for an idle probe merely because another worker observed the same enqueue.
#[tokio::test(flavor = "current_thread")]
async fn async_side_effect_delivery_watcher_broadcasts_to_all_workers() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut first_worker = handle.side_effect_delivery_watcher();
    let mut second_worker = handle.side_effect_delivery_watcher();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1),
                delay_ms: 1,
            }])
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_millis(50), first_worker.changed())
            .await
            .expect("first side-effect worker should observe the delivery revision")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(50), second_worker.changed())
            .await
            .expect("second side-effect worker should observe the same delivery revision")
            .unwrap();

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.side_effect_delivery_notifications, 1);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the side-effect worker wakes from actor notifications instead
/// of relying on its bounded idle probe. This keeps queued render or pane I/O
/// work responsive on the normal notification path while the probe remains only
/// a liveness backstop.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_side_effect_service_wakes_when_actor_queues_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let applied = StdArc::new(Mutex::new(Vec::new()));

    let worker_handle = handle.clone();
    let worker_applied = applied.clone();
    let worker_stop_applied = applied.clone();
    let worker = async move {
        tokio::time::timeout(
            Duration::from_millis(250),
            run_async_runtime_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                },
                |effect| {
                    worker_applied.lock().unwrap().push(effect);
                    Ok(())
                },
                |_, _| !worker_stop_applied.lock().unwrap().is_empty(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
    };
    let producer_handle = handle.clone();
    let producer = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"notified-side-effect-output\n".to_vec(),
        }));
        let report = producer_handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.side_effects, 1);
    };
    let shutdown = async {
        let (side_effect_report, ()) = tokio::join!(worker, producer);
        assert_eq!(side_effect_report.polls, 2);
        assert_eq!(side_effect_report.drained, 1);
        assert_eq!(side_effect_report.applied, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(shutdown, actor.run());
    assert_eq!(
        *applied.lock().unwrap(),
        vec![RuntimeSideEffect::RenderClient {
            client_id: primary,
            reason: RenderInvalidationReason::PaneOutput,
        }]
    );
    exit.service.terminate_all_pane_processes().unwrap();
}
