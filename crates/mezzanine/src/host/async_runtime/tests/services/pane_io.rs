//! Async-runtime tests owned by pane io behavior.

use super::super::*;

/// Verifies that pane write, resize, and termination side effects can be
/// drained and executed by a per-pane async worker. The worker must leave
/// unrelated side-effect families in the actor queue, submit typed completion
/// events back through the actor, and keep backend I/O details outside the
/// serialized runtime actor.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_io_side_effect_service_executes_pane_effects() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(5));
    backend.push_resize_result(Ok(()));
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        primary_pid: None,
        exit_code: Some(0),
        signal: None,
    }));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let provider_agent = AgentId::opaque("agent-%1").unwrap();
        let queued = service_handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"input".to_vec(),
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: provider_agent.clone(),
                    turn_id: "turn-1".to_string(),
                },
                RuntimeSideEffect::ResizePane {
                    pane_id: "%1".to_string(),
                    size: Size::new(100, 30).unwrap(),
                },
                RuntimeSideEffect::TerminatePane {
                    pane_id: "%1".to_string(),
                    force: true,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 4);

        let report = run_async_pane_io_side_effect_service(
            &service_handle,
            &mut driver,
            AsyncPaneIoSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        assert_eq!(backend.writes, vec![b"input".to_vec()]);
        assert_eq!(backend.resizes, vec![Size::new(100, 30).unwrap()]);
        assert_eq!(backend.terminations, vec![true]);
        assert_eq!(
            service_handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: provider_agent,
                turn_id: "turn-1".to_string(),
            }]
        );
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.drained, 3);
    assert_eq!(report.submitted_events, 3);
    assert_eq!(exit.metrics.runtime_side_effects_queued, 4);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 4);
}

/// Verifies default pane I/O drain limits interleave input chunks with output.
///
/// Large clipboard pastes into full-screen editors can fill the PTY output
/// side while the editor is still consuming input. Draining one pane input
/// effect per service poll gives the combined pane worker an opportunity to
/// read redraw output before accepting the next paste chunk.
#[test]
fn async_pane_io_default_drain_limits_interleave_paste_with_output() {
    assert_eq!(AsyncPaneIoSideEffectServiceConfig::default().drain_limit, 1);
    assert_eq!(
        AsyncPaneProcessServiceConfig::default().output_drain_limit,
        4
    );
    assert_eq!(AsyncPaneProcessServiceConfig::default().drain_limit, 1);
}

/// Verifies that an unbounded pane I/O side-effect worker parks on actor
/// notifications instead of polling at its idle interval while waiting for
/// pane-specific work. This protects the production worker path from idle CPU
/// churn while retaining prompt wakeups for queued input, resize, and terminate
/// side effects.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_io_side_effect_service_unbounded_waits_for_notifications() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(4));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let worker_handle = handle.clone();
    let worker = async move {
        let report = run_async_pane_io_side_effect_service(
            &worker_handle,
            &mut driver,
            AsyncPaneIoSideEffectServiceConfig {
                max_polls: u64::MAX,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        (report, driver.into_backend())
    };
    let producer_handle = handle.clone();
    let producer = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        producer_handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"wake".to_vec(),
            }])
            .await
            .unwrap();
    };
    let shutdown = async {
        let ((report, backend), ()) = tokio::join!(worker, producer);
        assert_eq!(report.polls, 2);
        assert_eq!(report.drained, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(backend.writes, vec![b"wake".to_vec()]);
        handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(shutdown, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that an idle pane I/O side-effect worker wakes from lifecycle
/// notifications without relying on its idle interval. This keeps worker-owned
/// pane input, resize, and terminate drains responsive to daemon shutdown even
/// when no pane-specific side effects are queued.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_io_side_effect_service_wakes_on_lifecycle_change_without_idle_poll() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let backend = AsyncFakePaneProcessIo::default();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let worker_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let worker = async move {
        let pane_worker = tokio::spawn(async move {
            run_async_pane_io_side_effect_service(
                &worker_handle,
                &mut driver,
                AsyncPaneIoSideEffectServiceConfig {
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
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(
            !pane_worker.is_finished(),
            "idle pane I/O worker should not wake from elapsed time alone"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "pane side-effect lifecycle wake test".to_string(),
            force: true,
            failed: false,
        }));
        shutdown_handle.submit_runtime_events(batch).await.unwrap();
        let report = tokio::time::timeout(Duration::from_millis(250), pane_worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 0);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Killed);
        shutdown_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(worker, actor.run());
    assert!(exit.commands_processed >= 3);
    exit.service.terminate_all_pane_processes().unwrap();
}
