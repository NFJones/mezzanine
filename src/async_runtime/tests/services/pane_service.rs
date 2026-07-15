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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
        .set_log_level("%1", crate::agent::AgentLogLevel::Verbose)
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
        let turn = crate::agent::AgentTurnRecord {
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            pane_id: task.pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
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
        let execution = crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
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
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: crate::agent::ContextSourceKind::UserInstruction,
                    content: "print a marker".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
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

        let first_seen = wait_for_rendered_text(
            &client_handle,
            ClientViewRole::Primary,
            "AGENT_ASYNC_FIRST_COMMAND",
        )
        .await
        .unwrap();
        assert!(
            first_seen.contains("AGENT_ASYNC_FIRST_COMMAND"),
            "{first_seen}"
        );
        wait_for_shell_transaction_timer_settlement(&client_handle, "first")
            .await
            .unwrap();

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
        let ready_again = client_handle
            .execute_terminal_command(
                primary.clone(),
                "mark-pane-ready --acknowledge-risk --reason async-agent-test-second-command"
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(ready_again.contains("override=applied"), "{ready_again}");
        let second_turn = crate::agent::AgentTurnRecord {
            turn_id: next_task.turn_id.clone(),
            agent_id: next_task.agent_id.clone(),
            pane_id: next_task.pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 2,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: mez_agent::AgentTurnState::Running,
            cooperation_mode: None,

            initial_capability: None,
        };
        let second_action = mez_agent::AgentAction {
            id: "shell-2".to_string(),
            rationale: "verify the pane shell still accepts input".to_string(),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: "Print a second marker".to_string(),
                command: "printf 'ASYNC_PANE_STILL_ALIVE\\n'".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(60_000),
            },
        };
        let second_batch = mez_agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: next_task.turn_id.clone(),
            agent_id: next_task.agent_id.clone(),
            actions: vec![second_action.clone()],
            final_turn: false,
        };
        let second_execution = crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
                provider: next_task.model_profile.provider.clone(),
                model: next_task.model_profile.model.clone(),
                reasoning_effort: next_task
                    .model_profile
                    .provider_options
                    .get("reasoning_effort")
                    .cloned()
                    .or_else(|| next_task.model_profile.reasoning_profile.clone()),
                thinking_enabled: next_task.model_profile.thinking_enabled(),
                latency_preference: next_task.model_profile.latency_preference.clone(),
                prompt_cache_retention: next_task
                    .model_profile
                    .provider_options
                    .get("prompt_cache_retention")
                    .cloned(),
                max_output_tokens: next_task.model_profile.max_output_tokens(),
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: next_task.turn_id.clone(),
                agent_id: next_task.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: crate::agent::ContextSourceKind::UserInstruction,
                    content: "print a second marker".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
                provider: next_task.model_profile.provider.clone(),
                model: next_task.model_profile.model.clone(),
                raw_text: "second shell command response".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(second_batch),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &second_turn,
                &second_action,
                vec!["second shell command accepted for pane execution".to_string()],
                Some(r#"{"state":"pending_dispatch"}"#.to_string()),
            )],
            final_turn: false,
            terminal_state: mez_agent::AgentTurnState::Running,
        };
        let mut second_provider_batch = RuntimeEventBatch::new();
        second_provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(next_task.agent_id).unwrap(),
            turn_id: next_task.turn_id,
            execution: Box::new(second_execution),
        }));
        let second_provider_report = client_handle
            .submit_runtime_events(second_provider_batch)
            .await
            .unwrap();
        assert_eq!(second_provider_report.accepted, 1);
        assert_eq!(second_provider_report.applied, 1);
        wait_for_shell_transaction_timer_settlement(&client_handle, "second")
            .await
            .unwrap();
        let alive_seen = wait_for_rendered_text(
            &client_handle,
            ClientViewRole::Primary,
            "ASYNC_PANE_STILL_ALIVE",
        )
        .await
        .unwrap();
        assert!(
            alive_seen.contains("ASYNC_PANE_STILL_ALIVE"),
            "{alive_seen}"
        );
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
    actor_exit
        .service
        .pane_processes_mut()
        .terminate_all()
        .unwrap();
}
