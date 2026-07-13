//! Async-runtime tests owned by pane driver behavior.

use super::super::*;

/// Verifies that the per-pane async driver converts PTY output from its backend
/// into an ordered runtime event without mutating shared session state. This is
/// the first step toward replacing global pane-output polling with one
/// independently scheduled pane task per live process.
#[tokio::test]
async fn async_pane_process_driver_converts_output_to_runtime_event() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"hello from pty".to_vec());
    let mut driver = AsyncPaneProcessDriver::new(
        "%1",
        backend,
        AsyncPaneProcessDriverConfig {
            max_output_bytes_per_event: 5,
        },
    )
    .unwrap();

    assert_eq!(driver.pane_id(), "%1");
    let event = driver.poll_output_event().await.unwrap();

    assert_eq!(
        event,
        Some(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"hello".to_vec(),
        }))
    );
}

/// Verifies that foreground process metadata observed by a pane worker becomes
/// a typed pane event. Automatic pane titles need this event once live pane
/// ownership has moved out of the synchronous process manager.
#[tokio::test]
async fn async_pane_process_driver_reports_foreground_process_metadata() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
        process_name: "vim".to_string(),
        process_group_id: 42,
        current_working_directory: Some(std::path::PathBuf::from("/tmp/project")),
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let event = driver.poll_foreground_process_event().await.unwrap();

    assert_eq!(
        event,
        Some(RuntimeEvent::Pane(PaneEvent::ForegroundProcess {
            pane_id: "%1".to_string(),
            process_name: "vim".to_string(),
            process_group_id: 42,
            current_working_directory: Some("/tmp/project".to_string()),
        }))
    );
}

/// Verifies that pane write, resize, and termination completions become typed
/// runtime events instead of panics or global driver failures. The async pane
/// migration needs this behavior so one pane's I/O failure can be rendered and
/// audited without blocking unrelated panes or attached clients.
#[tokio::test]
async fn async_pane_process_driver_reports_io_and_lifecycle_results() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Err(MezError::invalid_state("write failed")));
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

    let write = driver.write_input_event(b"input").await;
    let resize = driver.resize_event(Size::new(100, 30).unwrap()).await;
    let terminated = driver.terminate_event(false).await;
    let backend = driver.into_backend();

    assert_eq!(
        write,
        RuntimeEvent::Pane(PaneEvent::WriteFailed {
            pane_id: "%1".to_string(),
            error: "InvalidState: write failed".to_string(),
        })
    );
    assert_eq!(
        resize,
        RuntimeEvent::Pane(PaneEvent::Resized {
            pane_id: "%1".to_string(),
            size: Size::new(100, 30).unwrap(),
        })
    );
    assert_eq!(
        terminated,
        RuntimeEvent::Process(ProcessEvent::Exited {
            pane_id: "%1".to_string(),
            primary_pid: None,
            exit_code: Some(0),
            signal: None,
        })
    );
    assert_eq!(backend.writes, vec![b"input".to_vec()]);
    assert_eq!(backend.resizes, vec![Size::new(100, 30).unwrap()]);
    assert_eq!(backend.terminations, vec![false]);
}

/// Verifies that natural pane exits are polled as typed process events and
/// reported only once. A per-pane owner must not keep re-submitting the same
/// recorded process exit after the backend has reached a terminal state.
#[tokio::test]
async fn async_pane_process_driver_reports_polled_exit_once() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        primary_pid: None,
        exit_code: Some(7),
        signal: None,
    })));
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        primary_pid: None,
        exit_code: Some(7),
        signal: None,
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let first = driver.poll_exit_event().await.unwrap();
    let second = driver.poll_exit_event().await.unwrap();

    assert_eq!(
        first,
        Some(RuntimeEvent::Process(ProcessEvent::Exited {
            pane_id: "%1".to_string(),
            primary_pid: None,
            exit_code: Some(7),
            signal: None,
        }))
    );
    assert_eq!(second, None);
}

/// Verifies that the Tokio PTY backend can drive a real portable-pty pane
/// process without blocking the async test task. This keeps the live pane path
/// honest: the async driver boundary is not only a fake-test facade, and it can
/// read live PTY bytes and report process termination as a typed lifecycle
/// event.
#[tokio::test]
async fn async_pty_pane_process_io_bridges_live_portable_pty() {
    let launch = PaneProcessLaunch::new("/bin/sh".into());
    let process = spawn_pane_process(
        &launch,
        Some("/bin/sh -c 'printf async-bridge-output; sleep 5'"),
        &test_pane_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut backend = AsyncPtyPaneProcessIo::new("%bridge", process).unwrap();

    let mut output = Vec::new();
    for _ in 0..50 {
        if let Some(bytes) = backend.read_output(4096).await.unwrap() {
            output.extend(bytes);
        }
        if String::from_utf8_lossy(&output).contains("async-bridge-output") {
            break;
        }
        if let Some(activity) = backend.output_activity()
            && let Ok(result) = tokio::time::timeout(Duration::from_millis(500), activity).await
        {
            result.unwrap();
        }
    }

    assert!(
        String::from_utf8_lossy(&output).contains("async-bridge-output"),
        "{}",
        String::from_utf8_lossy(&output)
    );
    let event = backend.terminate(true).await.unwrap();

    let ProcessEvent::Exited {
        pane_id,
        exit_code,
        signal,
        ..
    } = event
    else {
        panic!("expected process exit event, got {event:?}");
    };
    assert_eq!(pane_id, "%bridge");
    assert!(
        exit_code.is_some() || signal.is_some(),
        "terminated process should expose an exit code or signal"
    );
}

/// Verifies that the live PTY backend can observe a minimal full-screen/TUI
/// mode transition over real pane output.
///
/// This regression extends the deterministic TUI suite with a portable-pty
/// process that enters alternate screen, enables focus events, clears the
/// viewport, draws sentinel text, and restores the normal screen. The async
/// PTY backend must preserve both the host-mode bytes and the visible content
/// in output order so later end-to-end compatibility tests can build on a
/// proven live-pane transport path.
#[tokio::test]
async fn async_pty_pane_process_io_preserves_full_screen_mode_bytes() {
    let launch = PaneProcessLaunch::new("/bin/sh".into());
    let process = spawn_pane_process(
        &launch,
        Some(
            "/bin/sh -c \"printf '\\033[?1049h\\033[?1004h\\033[H\\033[2Jmini-tui\\nready\\033[?1004l\\033[?1049l'\"",
        ),
        &test_pane_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut backend = AsyncPtyPaneProcessIo::new("%fullscreen", process).unwrap();

    let mut output = Vec::new();
    for _ in 0..50 {
        if let Some(bytes) = backend.read_output(4096).await.unwrap() {
            output.extend(bytes);
        }
        let text = String::from_utf8_lossy(&output);
        if text.contains("\x1b[?1049h")
            && text.contains("\x1b[?1004h")
            && text.contains("mini-tui")
            && text.contains("ready")
            && text.contains("\x1b[?1004l")
            && text.contains("\x1b[?1049l")
        {
            break;
        }
        if let Some(activity) = backend.output_activity()
            && let Ok(result) = tokio::time::timeout(Duration::from_millis(500), activity).await
        {
            result.unwrap();
        }
    }

    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("\x1b[?1049h"), "{text}");
    assert!(text.contains("\x1b[?1004h"), "{text}");
    assert!(text.contains("mini-tui"), "{text}");
    assert!(text.contains("ready"), "{text}");
    assert!(text.contains("\x1b[?1004l"), "{text}");
    assert!(text.contains("\x1b[?1049l"), "{text}");

    let event = backend.terminate(true).await.unwrap();

    let ProcessEvent::Exited {
        pane_id,
        exit_code,
        signal,
        ..
    } = event
    else {
        panic!("expected process exit event, got {event:?}");
    };
    assert_eq!(pane_id, "%fullscreen");
    assert!(
        exit_code.is_some() || signal.is_some(),
        "terminated process should expose an exit code or signal"
    );
}

/// Verifies that the pane driver service loop submits output events to the
/// runtime actor and reports both submitted and applied event counts. This is
/// the reusable bridge that live per-pane PTY tasks use after actor handoff.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_driver_service_submits_output_to_actor() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"driver-service-output\n".to_vec());
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_driver_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessDriverServiceConfig {
                max_polls: 1,
                idle_interval: Duration::from_millis(1),
            },
            |_| false,
        )
        .await
        .unwrap();
        let view = service_handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            view.lines
                .iter()
                .any(|line| line.contains("driver-service-output")),
            "{:?}",
            view.lines
        );
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.submitted_events, 1);
    assert_eq!(report.applied_events, 1);
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an idle pane driver service wakes between empty output polls
/// when the actor queues side effects. Bounded runs still keep a fallback
/// interval so finite tests can complete, but actor-side work must wake the
/// service before that full interval elapses.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_process_driver_service_wakes_between_empty_polls_on_side_effects() {
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
    let notify_handle = handle.clone();
    let service = async move {
        let driver_service = run_async_pane_process_driver_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessDriverServiceConfig {
                max_polls: 2,
                idle_interval: Duration::from_secs(60),
            },
            |_| false,
        );
        let notifier = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(10)).await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::ResizePane {
                    pane_id: "%1".to_string(),
                    size: Size::new(90, 30).unwrap(),
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(driver_service, notifier)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        assert_eq!(report.polls, 2);
        assert_eq!(report.submitted_events, 0);
        assert_eq!(report.applied_events, 0);
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    assert!(exit.commands_processed >= 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an unbounded pane driver service waits only on output, actor
/// event, and side-effect notifications between empty output polls. This keeps
/// the production-shaped legacy driver path from falling back to a periodic
/// idle poll.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_process_driver_service_unbounded_waits_for_notifications() {
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
    let notify_handle = handle.clone();
    let service = async move {
        let driver_service = run_async_pane_process_driver_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessDriverServiceConfig {
                max_polls: u64::MAX,
                idle_interval: Duration::from_millis(1),
            },
            |polls| polls >= 2,
        );
        let notifier = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::ResizePane {
                    pane_id: "%1".to_string(),
                    size: Size::new(90, 30).unwrap(),
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::join!(driver_service, notifier);
        let report = report.unwrap();
        assert_eq!(report.polls, 2);
        assert_eq!(report.submitted_events, 0);
        assert_eq!(report.applied_events, 0);
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the live PTY backend can wake a pane driver from Tokio
/// readiness instead of waiting for a compatibility service interval. This
/// keeps idle panes asleep until the PTY master becomes readable.
#[tokio::test]
async fn async_pane_process_driver_service_wakes_on_live_output_activity() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let launch = PaneProcessLaunch::new("/bin/sh".into());
    let process = spawn_pane_process(
        &launch,
        Some("/bin/sh -c 'sleep 0.05; printf live-activity-output'"),
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
            run_async_pane_process_driver_service(
                &service_handle,
                &mut driver,
                AsyncPaneProcessDriverServiceConfig {
                    max_polls: 2,
                    idle_interval: Duration::from_secs(60),
                },
                |_| false,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        let view = service_handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            view.lines
                .iter()
                .any(|line| line.contains("live-activity-output")),
            "{:?}",
            view.lines
        );
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies zero-byte PTY writes are reported as bounded failures.
///
/// A zero-byte write makes no transport progress. Treating it as success would
/// drop the pending input remainder and leave higher-level shell transactions
/// waiting for markers that can never arrive.
#[tokio::test]
async fn async_pane_process_driver_rejects_zero_byte_input_progress() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(0));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let event = driver.write_input_event(b"input").await;

    assert_eq!(
        event,
        RuntimeEvent::Pane(PaneEvent::WriteFailed {
            pane_id: "%1".to_string(),
            error: "InvalidState: pane PTY write accepted zero bytes".to_string(),
        })
    );
}
