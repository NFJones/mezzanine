//! Async-runtime tests owned by pane service behavior.

use super::super::*;

/// Verifies that the combined pane process service defers large input
/// remainders after one bounded write.
///
/// This keeps a paste-sized pane input side effect from monopolizing the PTY
/// write path. The next service poll can read full-screen application redraw
/// output before accepting the following input chunk while preserving input
/// ordering ahead of later actor-queued pane input.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_defers_large_input_remainders() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let backend = AsyncFakePaneProcessIo::default();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let large_input = vec![b'x'; 468_586];
        service_handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: large_input.clone(),
                },
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"after".to_vec(),
                },
            ])
            .await
            .unwrap();

        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 1,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        assert_eq!(report.drained, 2);
        assert_eq!(report.submitted_events, 2);
        assert_eq!(
            backend.writes,
            vec![
                large_input[..mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES].to_vec(),
                large_input[mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES
                    ..mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES * 2]
                    .to_vec()
            ]
        );
        assert_eq!(
            service_handle
                .drain_pane_io_side_effects("%1", 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"after".to_vec(),
            }]
        );
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that partial PTY write progress remains observable and ordered.
///
/// A backend can accept only part of a pane input chunk before applying
/// backpressure. The worker must surface that accepted byte count, keep the
/// unsent remainder ahead of later queued input, and retry the remainder on the
/// next poll instead of treating the whole write as failed or re-sending bytes
/// already accepted by the PTY.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_retries_partial_input_remainders() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(2));
    backend.push_write_result(Ok(4));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        service_handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"abcdef".to_vec(),
                },
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"after".to_vec(),
                },
            ])
            .await
            .unwrap();

        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 1,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        assert_eq!(report.drained, 2);
        assert_eq!(report.submitted_events, 2);
        assert_eq!(backend.writes, vec![b"abcdef".to_vec(), b"cdef".to_vec()]);
        assert_eq!(
            service_handle
                .drain_pane_io_side_effects("%1", 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"after".to_vec(),
            }]
        );
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies receiver-acknowledged delivery publishes bounded cumulative input
/// progress while reconciling the exact accepted byte total at completion.
/// Thousands of records must not create one serialized actor request each.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_aggregates_receiver_delivery_progress() {
    const RECORDS: usize = 300;
    const RECORD_BYTES: usize = 1024;
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut payload = Vec::with_capacity(RECORDS * RECORD_BYTES);
    for _ in 0..RECORDS {
        payload.extend(std::iter::repeat_n(b'x', RECORD_BYTES - 1));
        payload.push(b'\n');
    }
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.set_supports_shell_input_acknowledgements(true);
    backend.push_no_output();
    #[cfg(target_os = "macos")]
    for _ in 0..RECORDS {
        backend.push_output(vec![mez_mux::process::SHELL_INPUT_RECORD_ACK_BYTE]);
    }
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        service_handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::WritePaneShellInput {
                pane_id: "%1".to_string(),
                delivery: mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    payload,
                    "delivery-1",
                    true,
                ),
            }])
            .await
            .unwrap();
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: if cfg!(target_os = "macos") {
                    u64::try_from(RECORDS + 1).unwrap()
                } else {
                    1
                },
                output_drain_limit: 1,
                drain_limit: 1,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        let _ = service_handle.shutdown().await.unwrap();
        (report, backend)
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(backend.writes.len(), RECORDS);
    assert_eq!(report.submitted_events, 2, "{report:?}");
    assert_eq!(report.shell_input_progress_events, 2, "{report:?}");
    assert_eq!(
        report.shell_input_progress_bytes,
        RECORDS * RECORD_BYTES,
        "{report:?}"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies a receiver write failure publishes accepted partial bytes before
/// the immediate failure event. Aggregation must not lose local progress or
/// report bytes that the PTY rejected when a later suffix write fails.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_reconciles_receiver_progress_before_failure() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.set_supports_shell_input_acknowledgements(true);
    backend.push_write_result(Ok(2));
    backend.push_write_result(Err(MezError::invalid_state(
        "injected receiver write failure",
    )));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        service_handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::WritePaneShellInput {
                pane_id: "%1".to_string(),
                delivery: mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    b"abcdef\n".to_vec(),
                    "delivery-failure",
                    true,
                ),
            }])
            .await
            .unwrap();
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 1,
                output_drain_limit: 1,
                drain_limit: 1,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        let _ = service_handle.shutdown().await.unwrap();
        (report, backend)
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(
        backend.writes,
        vec![b"abcdef\n".to_vec(), b"cdef\n".to_vec()]
    );
    assert_eq!(report.submitted_events, 2, "{report:?}");
    assert_eq!(report.shell_input_progress_events, 1, "{report:?}");
    assert_eq!(report.shell_input_progress_bytes, 2, "{report:?}");
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the combined pane process service serializes PTY output and
/// pane I/O side effects through one driver. This is the ownership shape needed
/// before production live pane processes can move out of global manager
/// polling without introducing cross-task write/output/exit ordering races.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_serializes_output_and_side_effects() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"combined-service-output\n".to_vec());
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
                    force: false,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 4);

        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 1,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
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
        (report, driver.into_backend())
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.output_events, 1);
    assert_eq!(report.drained, 3);
    assert_eq!(report.submitted_events, 4);
    assert!(
        report.applied_events >= 1,
        "output should be applied before later pane lifecycle events: {report:?}"
    );
    assert_eq!(backend.writes, vec![b"input".to_vec()]);
    assert_eq!(backend.resizes, vec![Size::new(100, 30).unwrap()]);
    assert_eq!(backend.terminations, vec![false]);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies bursty pane output is submitted to the actor as one event batch.
///
/// SSH sessions are sensitive to event-loop and render invalidation churn. A
/// bounded output burst should therefore cross the actor boundary as one
/// ordered pane-output event with coalesced bytes.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_batches_bursty_output_events() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"one".to_vec());
    backend.push_output(b"two".to_vec());
    backend.push_output(b"three".to_vec());
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 1,
                output_drain_limit: 8,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.output_events, 3);
    assert_eq!(report.submitted_events, 1);
    assert_eq!(exit.metrics.runtime_event_batches, 1);
    assert_eq!(exit.metrics.pane_output_chunks, 1);
    assert_eq!(exit.metrics.pane_output_bytes, 11);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies foreground process metadata is not polled again for every output
/// chunk before its refresh interval elapses. Pane output should remain cheap
/// during bursty redraws, while process-title metadata still refreshes on its
/// own cadence.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_throttles_metadata_during_output_bursts() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"first".to_vec());
    backend.push_output(b"second".to_vec());
    backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
        process_name: "vim".to_string(),
        process_group_id: 42,
        current_working_directory: Some(std::path::PathBuf::from("/tmp/project")),
    })));
    backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
        process_name: "sh".to_string(),
        process_group_id: 43,
        current_working_directory: Some(std::path::PathBuf::from("/tmp/other")),
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.output_events, 2);
    assert_eq!(report.submitted_events, 3);
    assert_eq!(exit.metrics.pane_output_chunks, 2);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the combined pane process service wakes for queued pane I/O
/// side effects even when no PTY output is available. A live pane task must not
/// wait for its fallback interval before delivering user input, resize, or
/// termination requests.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_wakes_for_pane_side_effects() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_no_output();
    backend.push_no_output();
    backend.push_write_result(Ok(4));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let notify_handle = handle.clone();
    let service = async move {
        let pane_service = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_secs(60),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        );
        let notifier = async {
            tokio::task::yield_now().await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"wake".to_vec(),
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(pane_service, notifier)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        (report, driver.into_backend())
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 2);
    assert_eq!(report.output_events, 0);
    assert_eq!(report.drained, 1);
    assert_eq!(report.submitted_events, 1);
    assert_eq!(backend.writes, vec![b"wake".to_vec()]);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a quiet combined pane worker sleeps until the next foreground
/// metadata deadline instead of waking at the short compatibility idle
/// interval. This keeps idle pane workers from consuming CPU while preserving
/// periodic metadata refreshes and notification-driven side-effect wakeups.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_process_service_uses_metadata_deadline_for_quiet_panes() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_no_output();
    backend.push_no_output();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        (report, driver.into_backend())
    };
    let joined = async { tokio::join!(service, actor.run()) };
    tokio::pin!(joined);

    tokio::select! {
        _ = &mut joined => panic!("quiet pane worker woke before foreground metadata was due"),
        _ = tokio::time::sleep(Duration::from_millis(59_999)) => {}
    }
    tokio::time::advance(Duration::from_millis(1)).await;

    let ((report, _backend), mut exit) = joined.await;

    assert_eq!(report.polls, 2);
    assert_eq!(report.output_events, 0);
    assert_eq!(report.drained, 0);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that an idle combined pane worker wakes from the actor lifecycle
/// watch channel and terminates its backend when the daemon enters a terminal
/// state. This prevents shutdown from relying on synchronous `Drop` cleanup for
/// worker-owned PTYs when no pane output, side effect, or short idle timer is
/// available to wake the task.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_wakes_on_terminal_lifecycle_and_terminates_backend() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        primary_pid: None,
        exit_code: None,
        signal: Some("killed".to_string()),
    }));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let service = async move {
        let pane_service = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: u64::MAX,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_secs(60),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, state| {
                matches!(
                    state,
                    RuntimeLifecycleState::Stopping
                        | RuntimeLifecycleState::Killed
                        | RuntimeLifecycleState::Failed
                )
            },
        );
        let shutdown = async {
            tokio::task::yield_now().await;
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
                reason: "terminal lifecycle pane worker test".to_string(),
                force: true,
                failed: false,
            }));
            shutdown_handle.submit_runtime_events(batch).await.unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(pane_service, shutdown)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        (report, driver.into_backend())
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.terminal_state, RuntimeLifecycleState::Killed);
    assert_eq!(report.exit_events, 1);
    assert_eq!(backend.terminations, vec![true]);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the combined pane process service submits a natural process
/// exit only after a preceding PTY output poll has been given its own service
/// turn. This protects the migration's output-before-exit ordering contract
/// before live pane process ownership moves into the service.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_reports_exit_after_output_turn() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"final output before exit\n".to_vec());
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        primary_pid: None,
        exit_code: Some(0),
        signal: None,
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 2);
    assert_eq!(report.output_events, 1);
    assert_eq!(report.exit_events, 1);
    assert_eq!(report.submitted_events, 2);
    assert!(
        report.applied_events >= 1,
        "output should apply before exit event teardown: {report:?}"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the live PTY backend does not report process exit before
/// preceding output bytes have been drained. The backend may observe child exit
/// before the PTY master reports closure, so exit reporting must be held until
/// no output remains pending.
#[tokio::test]
async fn async_pane_process_service_waits_for_live_output_before_exit() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let launch = PaneProcessLaunch::new("/bin/sh".into());
    let process = spawn_pane_process(
        &launch,
        Some("/bin/sh -c 'printf live-output-before-exit'"),
        &test_pane_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let backend = AsyncPtyPaneProcessIo::new("%1", process).unwrap();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_pane_process_service(
                &service_handle,
                &mut driver,
                AsyncPaneProcessServiceConfig {
                    max_polls: 20,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
                |_, _| false,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert!(
        report.output_events >= 1,
        "live output should be observed before exit: {report:?}"
    );
    assert_eq!(report.exit_events, 1);
    assert!(
        report.submitted_events >= 2,
        "output and exit should both be submitted: {report:?}"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies the production async ownership path certifies an agent subshell
/// after the isolated bootstrap child relinquishes the real pane PTY.
///
/// This is the end-to-end regression for the original failure: periodic
/// foreground metadata can observe the transaction's `setsid` process, but
/// bootstrap completion must request a fresh correlated worker observation,
/// retain the parsed environment while pending, and publish it once the
/// persistent agent shell regains the foreground process group.
#[tokio::test(flavor = "current_thread")]
async fn async_agent_subshell_bootstrap_certifies_with_fresh_worker_observation() {
    let mut service = test_service_with_shell("/bin/bash");
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let pane_worker_handle = handle.clone();
    let client_handle = handle.clone();
    let pane_worker_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_stop = StdArc::clone(&pane_worker_done);

    let pane_worker = async move {
        let result = run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 4,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_millis(10),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await;
        if let Err(error) = result {
            assert!(
                matches!(
                    error.message(),
                    "async runtime session actor is closed"
                        | "async runtime session actor reply was dropped"
                ),
                "pane supervisor failed before actor shutdown: {error}"
            );
        }
    };

    let client = async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let toggled = tokio::time::timeout(
            Duration::from_secs(3),
            client_handle.execute_terminal_command(primary, "agent-shell".to_string()),
        )
        .await
        .expect("agent-shell toggle should not hang")
        .unwrap();
        assert!(toggled.contains("agent-shell"), "{toggled}");
        tokio::time::sleep(Duration::from_secs(2)).await;
        pane_worker_done.store(true, Ordering::SeqCst);
        assert_eq!(
            client_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), mut actor_exit) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(client, pane_worker, actor.run())
    })
    .await
    .expect("agent-subshell bootstrap certification should not hang");
    assert!(
        actor_exit
            .service
            .pane_environment_signature("%1")
            .is_some(),
        "certification should publish the parsed pane environment"
    );
    assert!(
        !actor_exit.service.pane_bootstrap_is_pending_for_tests("%1"),
        "certification should clear the bootstrap pending gate"
    );
    assert_eq!(
        actor_exit.service.pane_readiness_state("%1"),
        mez_agent::PaneReadinessState::Ready
    );
    assert!(
        actor_exit
            .service
            .pane_agent_subshell_certification_rejection("%1")
            .is_none()
    );
    actor_exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies the production async owner certifies Fish when it is an
/// uninstrumented foreign child of the configured parent shell.
///
/// Component tests can reach loader-ready and the managed child prompt without
/// proving that async foreground observation, transaction delivery, and final
/// environment certification clear the pane's bootstrap gate. The pane must
/// leave bootstrapping while the managed foreign child remains active.
#[tokio::test(flavor = "current_thread")]
async fn async_foreign_fish_child_clears_bootstrap_pending() {
    let Some(fish) = [
        "/usr/bin/fish",
        "/usr/local/bin/fish",
        "/opt/homebrew/bin/fish",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file()) else {
        eprintln!("skipping async foreign Fish bootstrap because fish is unavailable");
        return;
    };
    let parent_shell = if Path::new("/bin/bash").is_file() {
        "/bin/bash"
    } else {
        "/bin/sh"
    };
    let mut service = test_service_with_shell(parent_shell);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let parent_environment = mez_agent::EnvironmentSignature::new(
        std::env::consts::OS,
        std::env::consts::ARCH,
        None,
        "foreign-fish-parent",
        "test-user",
        None,
        parent_shell,
        if parent_shell.ends_with("bash") {
            mez_agent::ShellClassification::Bash
        } else {
            mez_agent::ShellClassification::PosixSh
        },
        None,
        Some("/usr/bin:/bin".to_string()),
        "/tmp",
        None,
        false,
        None,
        Vec::new(),
    )
    .unwrap();
    service.set_pane_environment_signature_for_tests("%1", parent_environment);
    service.set_pane_readiness("%1", mez_agent::PaneReadinessState::Ready);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 512,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();
    let pane_worker_handle = handle.clone();
    let client_handle = handle.clone();
    let pane_worker_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_stop = StdArc::clone(&pane_worker_done);

    let pane_worker = async move {
        run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 4,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_millis(10),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
    };

    let client = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let launch = format!("{} --no-config -i\n", mez_agent::shell::shell_quote(fish));
        client_handle
            .write_input_to_pane(primary.clone(), "%1", launch.into_bytes())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        client_handle
            .execute_terminal_command(primary, "agent-shell".to_string())
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let (child_active, bootstrap_pending, _) = client_handle
                .managed_shell_lifecycle_state("%1")
                .await
                .unwrap();
            if child_active && !bootstrap_pending {
                pane_worker_done.store(true, Ordering::SeqCst);
                let lifecycle = client_handle.shutdown().await.unwrap();
                break (lifecycle, child_active, bootstrap_pending);
            }
            if tokio::time::Instant::now() >= deadline {
                pane_worker_done.store(true, Ordering::SeqCst);
                let lifecycle = client_handle.shutdown().await.unwrap();
                break (lifecycle, child_active, bootstrap_pending);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };

    let ((lifecycle, child_active, bootstrap_pending), supervisor, mut actor_exit) =
        tokio::time::timeout(Duration::from_secs(30), async {
            let (client, worker, actor) = tokio::join!(client, pane_worker, actor.run());
            (client, worker, actor)
        })
        .await
        .expect("foreign Fish bootstrap should not hang");
    assert_eq!(lifecycle, RuntimeLifecycleState::Running);
    if let Err(error) = supervisor {
        assert!(
            matches!(
                error.message(),
                "async runtime session actor is closed"
                    | "async runtime session actor reply was dropped"
            ),
            "pane supervisor failed before actor shutdown: {error}"
        );
    }
    assert!(
        child_active && !bootstrap_pending,
        "foreign Fish did not clear bootstrap pending: active={child_active} bootstrap_pending={bootstrap_pending} phase={:?} readiness={:?} foreground={:?} transactions={:?} screen={:?}",
        actor_exit
            .service
            .foreign_shell_bootstrap_phase_for_tests("%1"),
        actor_exit.service.pane_readiness_state("%1"),
        actor_exit.service.pane_foreground_process_diagnostic("%1"),
        actor_exit.service.running_shell_transactions_for_tests(),
        actor_exit
            .service
            .pane_screen("%1")
            .map(|screen| screen.normal_content_lines().join("\n"))
    );
    assert!(!actor_exit.service.pane_bootstrap_is_pending_for_tests("%1"));
    actor_exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies a managed Fish pane remains responsive when agent mode is entered
/// with an unsubmitted draft and exited before any agent prompt is submitted.
///
/// This exercises the production actor and pane-worker ownership path rather
/// than writing directly to the PTY. The original draft must remain discarded
/// after Fish publishes its authenticated parent-return event, while a fresh
/// command must execute immediately.
#[tokio::test(flavor = "current_thread")]
async fn async_fish_dirty_draft_no_prompt_exit_discards_draft_and_restores_responsive_parent() {
    let Some(fish) = [
        "/usr/bin/fish",
        "/usr/local/bin/fish",
        "/opt/homebrew/bin/fish",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file()) else {
        eprintln!("skipping async Fish draft regression because fish is unavailable");
        return;
    };

    let fixture_root = std::env::temp_dir().join(format!(
        "mez-async-fish-no-prompt-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fixture_root).unwrap();
    let fixture_fish = fixture_root.join("fish");
    std::fs::write(
        &fixture_fish,
        format!(
            "#!/bin/sh\nexec {} --no-config \"$@\"\n",
            mez_agent::shell::shell_quote(fish)
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&fixture_fish, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let executed_path = std::env::temp_dir().join(format!(
        "mez-async-fish-no-prompt-executed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let discarded_path = executed_path.with_extension("discarded");
    let _ = std::fs::remove_file(&executed_path);
    let _ = std::fs::remove_file(&discarded_path);
    let draft = format!(
        "command touch {}",
        mez_agent::shell::fish_quote(discarded_path.to_str().unwrap())
    );
    let fresh_input = format!(
        "builtin printf '__MEZ_ASYNC_FISH_PARENT_RESPONSIVE__\\n'; command stty -a > {}\n",
        mez_agent::shell::fish_quote(executed_path.to_str().unwrap())
    );
    let mut service = test_service_with_shell(fixture_fish.to_str().unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let pane_worker_handle = handle.clone();
    let client_handle = handle.clone();
    let pane_worker_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_stop = StdArc::clone(&pane_worker_done);
    let client_executed_path = executed_path.clone();

    let pane_worker = async move {
        run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 4,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_millis(10),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
    };

    let client = async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        client_handle
            .write_input_to_pane(
                primary.clone(),
                "%1",
                b"fish_vi_key_bindings; builtin printf '__MEZ_ASYNC_FISH_VI_READY__\\n'\n".to_vec(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut draft_input = b"\x1bi".to_vec();
        draft_input.extend_from_slice(draft.as_bytes());
        client_handle
            .write_input_to_pane(primary.clone(), "%1", draft_input)
            .await
            .unwrap();
        let shown = client_handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ExecuteMux(
                        MuxAction::ToggleAgentShell,
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(shown.mux_actions_applied, 1);
        let child_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let (child_active, bootstrap_pending, restoration_pending) = client_handle
                .managed_shell_lifecycle_state("%1")
                .await
                .unwrap();
            if child_active && bootstrap_pending && restoration_pending {
                break;
            }
            assert!(
                tokio::time::Instant::now() < child_deadline,
                "managed Fish child did not reach active bootstrap before exit: active={child_active} bootstrap_pending={bootstrap_pending} restoration_pending={restoration_pending}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let retained_process_text = client_handle
            .managed_shell_process_screen_text("%1")
            .await
            .unwrap();
        assert!(
            !retained_process_text.replace('\n', "").contains(&draft),
            "managed Fish retained process screen still displayed the discarded draft: {retained_process_text:?}"
        );
        let hidden = client_handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ExecuteMux(
                        MuxAction::ToggleAgentShell,
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(hidden.mux_actions_applied, 1);
        let restoration_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let (child_active, bootstrap_pending, restoration_pending) = client_handle
                .managed_shell_lifecycle_state("%1")
                .await
                .unwrap();
            if !child_active && !bootstrap_pending && !restoration_pending {
                break;
            }
            assert!(
                tokio::time::Instant::now() < restoration_deadline,
                "managed Fish parent did not finish restoration after no-prompt exit: active={child_active} bootstrap_pending={bootstrap_pending} restoration_pending={restoration_pending}"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let input = client_handle
            .apply_attached_terminal_step_plan(
                primary,
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(
                        fresh_input.into_bytes(),
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert!(input.forwarded_bytes > 1);
        let execution_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !client_executed_path.is_file() {
            assert!(
                tokio::time::Instant::now() < execution_deadline,
                "fresh Fish command did not execute after no-prompt exit"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // File creation proves Fish executed the fresh command, but its
        // preceding terminal output can still be waiting in the PTY worker.
        // Keep the production reader alive long enough to ingest that output
        // before shutdown freezes the final pane snapshot used below.
        tokio::time::sleep(Duration::from_millis(250)).await;
        pane_worker_done.store(true, Ordering::SeqCst);
        client_handle.shutdown().await.unwrap()
    };

    let (lifecycle, supervisor, mut actor_exit) =
        tokio::time::timeout(Duration::from_secs(45), async {
            let (client, worker, actor) = tokio::join!(client, pane_worker, actor.run());
            (client, worker, actor)
        })
        .await
        .expect("dirty Fish no-prompt exit should not hang");
    assert_eq!(lifecycle, RuntimeLifecycleState::Running);
    if let Err(error) = supervisor {
        assert!(
            matches!(
                error.message(),
                "async runtime session actor is closed"
                    | "async runtime session actor reply was dropped"
            ),
            "pane supervisor failed before actor shutdown: {error}"
        );
    }
    let responsive_terminal_state = std::fs::read_to_string(&executed_path).unwrap();
    assert!(
        !responsive_terminal_state
            .split(|character: char| character.is_whitespace() || character == ';')
            .any(|field| field == "-echo"),
        "Fish parent terminal echo remained disabled after no-prompt exit: {responsive_terminal_state}"
    );
    let pane_text = actor_exit
        .service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(
        pane_text.contains("__MEZ_ASYNC_FISH_PARENT_RESPONSIVE__"),
        "returned Fish parent output remained hidden after no-prompt exit: {pane_text:?}"
    );
    assert!(
        !discarded_path.exists(),
        "discarded Fish draft executed after no-prompt exit"
    );
    actor_exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_file(executed_path).unwrap();
}

/// Verifies a managed Zsh pane remains responsive when agent mode is entered
/// with an unsubmitted draft and exited before any agent prompt is submitted.
///
/// This exercises staged HOLD, BEGIN, and DATA delivery through the production
/// actor and pane worker. The original draft must remain discarded after ZLE
/// publishes authenticated parent readiness, while a fresh command must
/// execute immediately.
#[tokio::test(flavor = "current_thread")]
async fn async_zsh_dirty_draft_no_prompt_exit_discards_draft_and_restores_responsive_parent() {
    let Some(zsh) = ["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
    else {
        eprintln!("skipping async Zsh draft regression because zsh is unavailable");
        return;
    };

    let fixture_root = std::env::temp_dir().join(format!(
        "mez-async-zsh-no-prompt-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fixture_root).unwrap();
    let fixture_zsh = fixture_root.join("zsh");
    std::fs::write(
        &fixture_zsh,
        format!(
            "#!/bin/sh\nexec {} -d \"$@\"\n",
            mez_agent::shell::shell_quote(zsh)
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&fixture_zsh, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let executed_path = std::env::temp_dir().join(format!(
        "mez-async-zsh-no-prompt-executed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let discarded_path = executed_path.with_extension("discarded");
    let _ = std::fs::remove_file(&executed_path);
    let _ = std::fs::remove_file(&discarded_path);
    let draft = format!(
        "command touch {}",
        mez_agent::shell::shell_quote(discarded_path.to_str().unwrap())
    );
    let fresh_input = format!(
        "print -r -- __MEZ_ASYNC_ZSH_PARENT_RESPONSIVE__; command stty -a > {}\n",
        mez_agent::shell::shell_quote(executed_path.to_str().unwrap())
    );
    let mut service = test_service_with_shell(fixture_zsh.to_str().unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let pane_worker_handle = handle.clone();
    let client_handle = handle.clone();
    let pane_worker_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_stop = StdArc::clone(&pane_worker_done);
    let client_executed_path = executed_path.clone();

    let pane_worker = async move {
        run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 4,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_millis(10),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
    };

    let client = async move {
        let admission_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let admission_ready = client_handle
                .managed_zsh_admission_ready("%1")
                .await
                .unwrap();
            let (_, bootstrap_pending, restoration_pending) = client_handle
                .managed_shell_lifecycle_state("%1")
                .await
                .unwrap();
            if admission_ready && !bootstrap_pending && !restoration_pending {
                break;
            }
            assert!(
                tokio::time::Instant::now() < admission_deadline,
                "managed Zsh adapter and initial bootstrap did not settle before dirty-draft entry: admission_ready={admission_ready} bootstrap_pending={bootstrap_pending} restoration_pending={restoration_pending}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        client_handle
            .write_input_to_pane(
                primary.clone(),
                "%1",
                b"PS1='__MEZ_ASYNC_ZSH_READY__> '; print -r -- '__MEZ_ASYNC_ZSH_ROUND_TRIP__'\n"
                    .to_vec(),
            )
            .await
            .unwrap();
        let editor_ready_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let retained_process_text = client_handle
                .managed_shell_process_screen_text("%1")
                .await
                .unwrap();
            // Screen text intentionally strips terminal-grid padding, including the prompt's
            // trailing space. Require adjacent complete lines so an echoed setup command cannot
            // be mistaken for the round-trip output followed by the restored editor prompt.
            if retained_process_text
                .lines()
                .zip(retained_process_text.lines().skip(1))
                .any(|(output, prompt)| {
                    output == "__MEZ_ASYNC_ZSH_ROUND_TRIP__" && prompt == "__MEZ_ASYNC_ZSH_READY__>"
                })
            {
                break;
            }
            let (child_active, bootstrap_pending, restoration_pending) = client_handle
                .managed_shell_lifecycle_state("%1")
                .await
                .unwrap();
            assert!(
                tokio::time::Instant::now() < editor_ready_deadline,
                "managed Zsh editor did not complete its semantic readiness round trip: screen={retained_process_text:?} active={child_active} bootstrap_pending={bootstrap_pending} restoration_pending={restoration_pending}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        client_handle
            .write_input_to_pane(primary.clone(), "%1", draft.as_bytes().to_vec())
            .await
            .unwrap();
        let draft_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let retained_process_text = client_handle
                .managed_shell_process_screen_text("%1")
                .await
                .unwrap();
            if retained_process_text.replace('\n', "").contains(&draft) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < draft_deadline,
                "managed Zsh draft was not displayed before agent-shell entry: {retained_process_text:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let shown = client_handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ExecuteMux(
                        MuxAction::ToggleAgentShell,
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(shown.mux_actions_applied, 1);
        let child_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let (child_active, bootstrap_pending, restoration_pending) = client_handle
                .managed_shell_lifecycle_state("%1")
                .await
                .unwrap();
            if child_active && bootstrap_pending && restoration_pending {
                break;
            }
            assert!(
                tokio::time::Instant::now() < child_deadline,
                "managed Zsh child did not reach active bootstrap before exit: active={child_active} bootstrap_pending={bootstrap_pending} restoration_pending={restoration_pending}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let editor_clear_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let retained_process_text = client_handle
                .managed_shell_process_screen_text("%1")
                .await
                .unwrap();
            if !retained_process_text.replace('\n', "").contains(&draft) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < editor_clear_deadline,
                "managed Zsh retained process screen still displayed the discarded draft after admission: {retained_process_text:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let hidden = client_handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ExecuteMux(
                        MuxAction::ToggleAgentShell,
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(hidden.mux_actions_applied, 1);
        let restoration_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let (child_active, bootstrap_pending, restoration_pending) = client_handle
                .managed_shell_lifecycle_state("%1")
                .await
                .unwrap();
            if !child_active && !bootstrap_pending && !restoration_pending {
                break;
            }
            assert!(
                tokio::time::Instant::now() < restoration_deadline,
                "managed Zsh parent did not finish restoration after no-prompt exit: active={child_active} bootstrap_pending={bootstrap_pending} restoration_pending={restoration_pending}"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let input = client_handle
            .apply_attached_terminal_step_plan(
                primary,
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(
                        fresh_input.into_bytes(),
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert!(input.forwarded_bytes > 1);
        let execution_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !client_executed_path.is_file() {
            assert!(
                tokio::time::Instant::now() < execution_deadline,
                "fresh Zsh command did not execute after no-prompt exit"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        pane_worker_done.store(true, Ordering::SeqCst);
        client_handle.shutdown().await.unwrap()
    };

    let (lifecycle, supervisor, mut actor_exit) =
        tokio::time::timeout(Duration::from_secs(60), async {
            let (client, worker, actor) = tokio::join!(client, pane_worker, actor.run());
            (client, worker, actor)
        })
        .await
        .expect("dirty Zsh no-prompt exit should not hang");
    assert_eq!(lifecycle, RuntimeLifecycleState::Running);
    if let Err(error) = supervisor {
        assert!(
            matches!(
                error.message(),
                "async runtime session actor is closed"
                    | "async runtime session actor reply was dropped"
            ),
            "pane supervisor failed before actor shutdown: {error}"
        );
    }
    let responsive_terminal_state = std::fs::read_to_string(&executed_path).unwrap();
    assert!(
        !responsive_terminal_state
            .split(|character: char| character.is_whitespace() || character == ';')
            .any(|field| field == "-echo"),
        "Zsh parent terminal echo remained disabled after no-prompt exit: {responsive_terminal_state}"
    );
    let pane_text = actor_exit
        .service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(
        pane_text.contains("__MEZ_ASYNC_ZSH_PARENT_RESPONSIVE__"),
        "returned Zsh parent output remained hidden after no-prompt exit: {pane_text:?}"
    );
    assert!(
        !discarded_path.exists(),
        "discarded Zsh draft executed after no-prompt exit"
    );
    actor_exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_file(executed_path).unwrap();
    std::fs::remove_dir_all(fixture_root).unwrap();
}

/// Verifies that the async-owned pane path keeps the pane shell alive after the
/// first agent shell command dispatch. This covers the production daemon shape:
/// a real PTY shell is claimed by the Tokio pane worker, a provider completion
/// queues a shell action, and a later pane input still reaches the same shell
/// instead of observing a process exit or supervisor shutdown.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_worker_keeps_shell_alive_after_first_agent_command() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", mez_agent::AgentLogLevel::Verbose)
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let pane_worker_handle = handle.clone();
    let client_handle = handle.clone();
    let pane_worker_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_stop = StdArc::clone(&pane_worker_done);
    let (pane_worker_stopped_tx, pane_worker_stopped_rx) = tokio::sync::oneshot::channel();

    let pane_worker = async move {
        let report = run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap();
        let _ = pane_worker_stopped_tx.send(());
        report
    };

    let client = async move {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let start = client_handle
            .execute_agent_shell_command(primary.clone(), "print a marker".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        let task = client_handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.turn_id == "turn-1")
            .expect("agent prompt should queue turn-1 provider task");
        assert_eq!(
            client_handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
                turn_id: task.turn_id.clone(),
            }]
        );
        let turn = mez_agent::AgentTurnRecord {
            turn_id: task.turn_id.clone(),
            conversation_id: "conversation-1".to_string(),
            agent_id: task.agent_id.clone(),
            pane_id: task.pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            deadline_at_unix_millis: 0,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: mez_agent::AgentTurnState::Running,
            cooperation_mode: None,

            initial_capability: None,
        };
        let action = mez_agent::AgentAction {
            id: "shell-1".to_string(),
            rationale: "print a marker".to_string(),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: "Print a marker".to_string(),
                command: "printf 'AGENT_ASYNC_FIRST_COMMAND\\n'".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(60_000),
            },
        };
        let batch = mez_agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            actions: vec![action.clone()],
            final_turn: false,
        };
        let execution = mez_agent::AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: task.model_profile.provider.clone(),
                model: task.model_profile.model.clone(),
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
                turn_id: task.turn_id.clone(),
                agent_id: task.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::for_capability(
                    mez_agent::AgentCapability::Shell,
                ),
                recovery_input: None,
                messages: vec![mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    content: "print a marker".to_string(),
                }]
                .into(),
            },
            response: mez_agent::ModelResponse {
                provider: task.model_profile.provider.clone(),
                model: task.model_profile.model.clone(),
                raw_text: "shell command response".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(batch),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                vec!["shell command accepted for pane execution".to_string()],
                Some(r#"{"state":"pending_dispatch"}"#.to_string()),
            )],
            final_turn: false,
            terminal_state: mez_agent::AgentTurnState::Running,
        };
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let provider_report = client_handle
            .submit_runtime_events(provider_batch)
            .await
            .unwrap();
        assert_eq!(provider_report.accepted, 1);
        assert_eq!(provider_report.applied, 1);

        let mut next_task = None;
        for _ in 0..200 {
            if let Some(task) = client_handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .into_iter()
                .find(|pending| pending.turn_id == "turn-1")
            {
                next_task = Some(task);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let next_task =
            next_task.expect("first shell transaction should queue provider continuation");
        assert_eq!(
            client_handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque(next_task.agent_id.clone()).unwrap(),
                turn_id: next_task.turn_id.clone(),
            }],
            "shell settlement must publish the provider continuation without fallback polling"
        );
        let hidden = client_handle
            .execute_terminal_command(primary.clone(), "agent-shell".to_string())
            .await
            .unwrap();
        assert!(hidden.contains("visibility=hidden"), "{hidden}");
        let ready_again = client_handle
            .execute_terminal_command(
                primary.clone(),
                "mark-pane-ready --acknowledge-risk --reason async-agent-test-second-command"
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(ready_again.contains("override=applied"), "{ready_again}");
        let direct_input = client_handle
            .write_input_to_pane(
                primary.clone(),
                "%1",
                b"echo ASYNC_PANE_STILL_ALIVE\n".to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(
            direct_input.bytes_written,
            b"echo ASYNC_PANE_STILL_ALIVE\n".len()
        );
        assert!(direct_input.primary_pid > 0);
        assert_eq!(
            client_handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        pane_worker_done.store(true, Ordering::SeqCst);
        pane_worker_stopped_rx
            .await
            .expect("pane worker should stop before actor shutdown");
        assert_eq!(
            client_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), supervisor_report, mut actor_exit) =
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(client, pane_worker, actor.run())
        })
        .await
        .expect("async pane worker shell liveness test should not hang indefinitely");
    assert_eq!(
        actor_exit.service.lifecycle_state(),
        RuntimeLifecycleState::Running
    );
    assert!(supervisor_report.spawned_workers >= 1);
    assert_eq!(
        supervisor_report.terminal_state,
        RuntimeLifecycleState::Running
    );
    actor_exit.service.terminate_all_pane_processes().unwrap();
}
