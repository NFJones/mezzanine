/// Verifies that process watcher failures are applied as diagnostic runtime
/// events instead of being silently accepted and dropped. Async pane wait,
/// resize, write, or termination tasks need this path so failures can be
/// replayed to clients and inspected after the worker that observed them exits.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_process_failure_events_to_event_log() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Failed {
            pane_id: "%1".to_string(),
            error: "wait task failed".to_string(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        let _ = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""process_state":"failed""#)
            && event.payload.contains("wait task failed")
    }));
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that pane I/O completion events from the async driver are applied
/// through the actor instead of being treated as accepted-only bookkeeping.
/// Write failures must become replayable diagnostics, and resize completions
/// must update retained terminal state and lifecycle output for attached
/// clients.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_pane_io_completion_events_to_event_log() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::WriteFailed {
            pane_id: "%1".to_string(),
            error: "broken pipe".to_string(),
        }));
        batch.push(RuntimeEvent::Pane(PaneEvent::Resized {
            pane_id: "%1".to_string(),
            size: Size::new(100, 30).unwrap(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 2);
        assert_eq!(report.side_effects, 2);
        assert_eq!(handle.drain_runtime_side_effects(8).await.unwrap().len(), 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pane_io":"write_failed""#)
            && event.payload.contains("broken pipe")
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pty_resize":"applied""#)
            && event.payload.contains(r#""columns":100"#)
            && event.payload.contains(r#""rows":30"#)
    }));
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that supervisor shutdown events are applied through typed runtime
/// ingress even when there is no active primary client. This is the async
/// supervisor path for forced daemon shutdown, failed critical services, and
/// signal handling where the multiplexer must terminate live panes and remove the
/// registry record without routing through a primary-owned control command.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_forced_shutdown_events_without_primary() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test supervisor shutdown".to_string(),
            force: true,
            failed: false,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Killed
    );
    assert!(exit.service.session().windows().is_empty());
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .all(|client| client.state != ClientState::Attached)
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""lifecycle":"shutdown""#)
            && event.payload.contains("test supervisor shutdown")
    }));
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that primary-client detach only changes attachment lifecycle when
/// pane processes have moved to async workers. This protects detachable daemon
/// sessions from treating the loss of the foreground client as a pane shutdown
/// request after process ownership leaves the synchronous manager.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_primary_detach_retains_worker_owned_panes() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary.clone(),
            reason: "primary fd closed".to_string(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            Vec::<RuntimeSideEffect>::new()
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
        let _ = process.terminate(Duration::from_millis(10));
        pane_id
    };

    let (pane_id, exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Detached
    );
    assert!(
        exit.service.pane_process_is_async_owned(&pane_id),
        "detaching the primary client must not release async worker ownership"
    );
}

/// Verifies that a primary control attach resizes pane geometry even when the
/// initial pane process has already moved to an async worker. Default `mez`
/// launch starts a background daemon, the pane supervisor can claim the shell
/// before the foreground attach initializes, and the first agent prompt must
/// still render at the live terminal bottom rather than the daemon's bootstrap
/// 80x24 size.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_control_initialize_resizes_worker_owned_initial_pane() {
    use crate::control::{decode_control_frame, encode_control_body};

    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (_pane_id, mut process) = processes.pop().unwrap();
        let mut connection = ControlConnectionState::new(true, true);
        let initialize = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
        );

        let result = handle
            .handle_control_input_for_connection(initialize, 1024 * 1024, connection.clone())
            .await
            .unwrap();
        connection = result.connection;
        let (body, _) = decode_control_frame(&result.output, 1024 * 1024).unwrap();
        assert!(body.contains(r#""granted_role":"primary""#), "{body}");
        let show = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        );
        let show_result = handle
            .handle_control_input_for_connection(show, 1024 * 1024, connection.clone())
            .await
            .unwrap();
        connection = show_result.connection;
        let (show_body, _) = decode_control_frame(&show_result.output, 1024 * 1024).unwrap();
        assert!(show_body.contains(r#""visible":true"#), "{show_body}");
        let frame = handle
            .render_client_frame(
                ClientViewRole::Primary,
                Size::new(100, 40).unwrap(),
                TerminalClientLoopConfig::default(),
                true,
            )
            .await
            .unwrap();
        let view = frame.view.unwrap();
        assert_eq!(view.authoritative_size, Size::new(100, 40).unwrap());
        let region = view.agent_prompt_region.unwrap();
        assert_eq!(region.rows, 38);
        let prompt_row = view
            .lines
            .iter()
            .rposition(|line| line.contains("agent>"))
            .unwrap();
        assert!(
            prompt_row >= 38,
            "agent prompt text should render at attached terminal bottom: {view:?}"
        );
        assert!(
            view.cursor_row >= 38,
            "agent prompt cursor should render at attached terminal bottom: {view:?}"
        );
        let effects = handle.drain_pane_io_side_effects("%1", 8).await.unwrap();
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ResizePane {
                    pane_id,
                    size,
                } if pane_id == "%1" && *size == Size::new(100, 38).unwrap()
            )),
            "{effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        assert!(connection.caller_client_id().is_some());
        let _ = process.terminate(Duration::from_millis(10));
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
}

/// Verifies that forced supervisor shutdown crosses the async pane ownership
/// boundary. When a pane process is worker-owned, the runtime actor must queue
/// a termination side effect for the worker instead of assuming the
/// compatibility process manager can terminate the PTY directly.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_forced_shutdown_terminates_worker_owned_panes() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "forced async shutdown".to_string(),
            force: true,
            failed: false,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::TerminatePane {
                pane_id: pane_id.clone(),
                force: true,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
        let _ = process.terminate(Duration::from_millis(10));
        pane_id
    };

    let (pane_id, exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Killed
    );
    assert!(
        !exit.service.pane_process_is_async_owned(&pane_id),
        "forced shutdown must release async ownership after queuing worker termination"
    );
    assert!(exit.service.session().windows().is_empty());
}

/// Verifies that non-forced supervisor shutdown requests apply through the
/// same typed runtime event ingress as forced shutdown without detaching the
/// primary client or killing panes. This covers graceful supervisor paths where
/// peer services should observe the stopping lifecycle before any later forced
/// cleanup decision.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_graceful_shutdown_events() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test graceful shutdown".to_string(),
            force: false,
            failed: false,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Stopping
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Stopping
    );
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .any(|client| client.state == ClientState::Attached)
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""lifecycle":"stopping""#), "{events}");
    assert!(
        events.contains(r#""shutdown_reason":"test graceful shutdown""#),
        "{events}"
    );
    assert!(events.contains(r#""force":false"#), "{events}");
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that supervisor failure shutdown events are represented as failed
/// runtime state rather than being collapsed into graceful stopping or forced
/// kill semantics. This gives the Tokio supervisor a typed failure path for
/// critical-service failures while still recording a replayable lifecycle
/// diagnostic and notifying attached clients through render side effects.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_failed_shutdown_events() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "critical service failed".to_string(),
            force: false,
            failed: true,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Failed
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Failed
    );
    assert_eq!(
        exit.service.session().state,
        crate::session::SessionState::Failed
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""lifecycle":"failed""#), "{events}");
    assert!(
        events.contains(r#""shutdown_reason":"critical service failed""#),
        "{events}"
    );
    assert!(events.contains(r#""force":false"#), "{events}");
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the deterministic async attached-terminal fake behaves like an
/// ordered terminal endpoint: readiness, input truncation, size responses,
/// presentation guards, invalidation, and styled-frame writes are all visible
/// without using wall-clock sleeps. This gives later Tokio client-loop tests a
/// stable fake before production file descriptors are migrated to `AsyncFd`.
#[tokio::test]
async fn async_fake_attached_terminal_io_records_ordered_operations() {
    let mut io = AsyncFakeAttachedTerminalIo::default();
    let readiness = AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    };
    io.push_readiness(vec![readiness]);
    io.push_input(b"abcdef".to_vec());
    io.push_terminal_size(Some(Size::new(100, 30).unwrap()));

    assert_eq!(io.poll_readiness().await.unwrap(), vec![readiness]);
    assert_eq!(io.read_input(3).await.unwrap(), b"abc");
    assert_eq!(
        io.terminal_size().await.unwrap(),
        Some(Size::new(100, 30).unwrap())
    );

    io.enter_presentation().await.unwrap();
    io.invalidate_output_frame().await.unwrap();
    let modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_row: 2,
        cursor_column: 3,
        ..AttachedTerminalOutputModes::default()
    };
    let lines = vec!["hello".to_string(), "world".to_string()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], modes)
        .await
        .unwrap();
    io.restore_presentation().await.unwrap();

    assert_eq!(bytes, "helloworld".len());
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.presentation_restores, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, lines);
    assert_eq!(io.written_frames[0].modes.cursor_row, 2);
    assert_eq!(io.written_frames[0].modes.cursor_column, 3);
}

/// Verifies that the shared attached-terminal presentation guard validates the
/// raw-mode descriptor before entering the foreground terminal path. This keeps
/// daemon and control-socket attach clients on one setup boundary and prevents
/// invalid descriptors from partially constructing async fd state that would
/// later be difficult to clean up.
#[test]
fn async_attached_terminal_presentation_guard_rejects_invalid_raw_fd() {
    let error = AsyncAttachedTerminalPresentationGuard::new(-1, -1, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("terminal raw mode file descriptor is invalid"),
        "{error}"
    );
}

/// Verifies that the transitional sync-to-async terminal adapter preserves the
/// existing `AttachedTerminalClientLoopIo` behavior while exposing the new async
/// trait. The adapter is a migration bridge only, so this test protects current
/// behavior while making its replacement with a native Tokio implementation
/// mechanically straightforward.
#[tokio::test]
async fn sync_attached_terminal_io_adapter_preserves_existing_fake_behavior() {
    let mut sync = FakeAttachedTerminalLoopIo::default();
    let readiness = AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Output,
        fd: 1,
        interest: TerminalFdInterest::write(),
        readable: false,
        writable: true,
        hangup: false,
        error: false,
    };
    sync.readiness_batches.push(vec![readiness]);
    sync.input_batches.push(b"input".to_vec());
    let mut adapter = SyncAttachedTerminalIoAdapter::new(sync);

    assert_eq!(adapter.poll_readiness().await.unwrap(), vec![readiness]);
    assert_eq!(adapter.read_input(2).await.unwrap(), b"in");
    let lines = vec!["frame".to_string()];
    let bytes = adapter
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();

    let sync = adapter.into_inner();
    assert_eq!(bytes, "frame".len());
    assert_eq!(sync.written_batches, vec![lines]);
}

/// Verifies that the Tokio `AsyncFd` attached-terminal endpoint can read and
/// write through Unix file descriptors without the synchronous terminal polling
/// trait. The test uses a Unix socket pair as a deterministic fd source, which
/// exercises nonblocking flag setup, async input readiness, async output
/// flushing, and terminal-frame encoding without requiring a real foreground
/// TTY.
#[tokio::test]
async fn async_fd_attached_terminal_io_reads_and_writes_socket_pair() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    peer.write_all(b"input").unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let input = io.read_input(5).await.unwrap();
    assert_eq!(input, b"input");

    let lines = vec!["async-frame".to_string()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();
    assert!(bytes > "async-frame".len());

    let mut output = vec![0u8; 4096];
    let read = peer.read(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output[..read]);
    assert!(output.contains("async-frame"), "{output:?}");
}

/// Verifies that the native async terminal endpoint's normal frame-write API
/// completes frames larger than the adaptive bounded-write chunk. Control-socket
/// attach rendering uses this API directly; returning after the first chunk
/// leaves the rest of a scroll or copy-mode repaint retained but never flushed,
/// which appears as large unrendered regions on the attached terminal.
#[tokio::test]
async fn async_fd_attached_terminal_io_unbounded_write_completes_large_frame() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let large_line = format!(
        "{}tail-marker",
        "x".repeat(DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES + 1024)
    );
    let lines = vec![large_line.clone()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();

    assert!(bytes > DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES);
    assert_eq!(io.pending_output_bytes(), 0);

    let mut output = Vec::new();
    drop(io);
    drop(driver_output);
    drop(driver);
    peer.read_to_end(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("tail-marker"), "{output:?}");
}

/// Verifies that the native async terminal endpoint reports pending input
/// before the always-writable output side of an interactive PTY-like fd pair.
/// This protects foreground attach loops from starving user keystrokes while
/// redraws remain possible on every iteration.
#[tokio::test]
async fn async_fd_attached_terminal_io_prioritizes_input_over_writable_output() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.write_all(b"x").unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let readiness = io.poll_readiness().await.unwrap();

    assert!(
        readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable),
        "{readiness:?}"
    );
}

/// Verifies that the native async terminal endpoint's input-focused readiness
/// wait does not wake merely because stdout is writable. This is the attach
/// service idle-CPU guard: redraws should come from actor render notifications
/// or explicit fallback timers, while user input still wakes the service
/// promptly.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_fd_attached_terminal_input_readiness_ignores_writable_output() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();

    let idle = tokio::time::timeout(Duration::from_millis(1), io.poll_input_readiness()).await;
    assert!(idle.is_err(), "writable output should not wake input wait");

    peer.write_all(b"x").unwrap();
    let readiness = tokio::time::timeout(Duration::from_millis(1), io.poll_input_readiness())
        .await
        .unwrap()
        .unwrap();
    assert!(
        readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable),
        "{readiness:?}"
    );
}

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
        exit_code: Some(7),
        signal: None,
    })));
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
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
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let process = spawn_pane_process(
        &shell,
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

/// Verifies that the pane driver service loop submits output events to the
/// runtime actor and reports both submitted and applied event counts. This is
/// the reusable bridge that live per-pane PTY tasks use after actor handoff.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_driver_service_submits_output_to_actor() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let process = spawn_pane_process(
        &shell,
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

/// Verifies that pane write, resize, and termination side effects can be
/// drained and executed by a per-pane async worker. The worker must leave
/// unrelated side-effect families in the actor queue, submit typed completion
/// events back through the actor, and keep backend I/O details outside the
/// serialized runtime actor.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_io_side_effect_service_executes_pane_effects() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(5));
    backend.push_resize_result(Ok(()));
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
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

/// Verifies that the combined pane process service defers large input
/// remainders after one bounded write.
///
/// This keeps a paste-sized pane input side effect from monopolizing the PTY
/// write path. The next service poll can read full-screen application redraw
/// output before accepting the following input chunk while preserving input
/// ordering ahead of later actor-queued pane input.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_defers_large_input_remainders() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
                large_input[..crate::process::PTY_INPUT_WRITE_CHUNK_BYTES].to_vec(),
                large_input[crate::process::PTY_INPUT_WRITE_CHUNK_BYTES
                    ..crate::process::PTY_INPUT_WRITE_CHUNK_BYTES * 2]
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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

/// Verifies that an unbounded pane I/O side-effect worker parks on actor
/// notifications instead of polling at its idle interval while waiting for
/// pane-specific work. This protects the production worker path from idle CPU
/// churn while retaining prompt wakeups for queued input, resize, and terminate
/// side effects.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_io_side_effect_service_unbounded_waits_for_notifications() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an idle pane I/O side-effect worker wakes from lifecycle
/// notifications without relying on its idle interval. This keeps worker-owned
/// pane input, resize, and terminate drains responsive to daemon shutdown even
/// when no pane-specific side effects are queued.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_io_side_effect_service_wakes_on_lifecycle_change_without_idle_poll() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the combined pane process service serializes PTY output and
/// pane I/O side effects through one driver. This is the ownership shape needed
/// before production live pane processes can move out of global manager
/// polling without introducing cross-task write/output/exit ordering races.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_serializes_output_and_side_effects() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"combined-service-output\n".to_vec());
    backend.push_write_result(Ok(5));
    backend.push_resize_result(Ok(()));
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"final output before exit\n".to_vec());
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let process = spawn_pane_process(
        &shell,
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

/// Verifies that the dynamic pane-process supervisor can claim a live
/// manager-owned pane through the actor and start a per-pane worker without a
/// startup-only handoff list. This is the daemon path needed for panes created
/// after the initial session boot.
#[tokio::test]
async fn async_pane_process_supervisor_claims_live_manager_panes() {
    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let supervisor_handle = handle.clone();
    let supervisor = async move {
        let report = run_async_pane_process_supervisor_service(
            supervisor_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: 2,
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
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.spawned_workers, 1);
        assert_eq!(
            handle
                .take_running_pane_processes_for_async_owner(8)
                .await
                .unwrap()
                .len(),
            0
        );
        let _ = handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(supervisor, actor.run());

    assert_eq!(report.polls, 2);
    assert_eq!(report.spawned_workers, 1);
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies that the dynamic pane-process supervisor observes child worker
/// completion directly instead of waking on its fallback idle interval. This
/// keeps production supervision responsive to short-lived panes without adding
/// an idle poll while no new handoffs are available.
#[tokio::test]
async fn async_pane_process_supervisor_wakes_on_worker_completion() {
    let mut service = test_service();
    service.start_initial_pane_process(Some("true")).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let supervisor_handle = handle.clone();
    let supervisor = async move {
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_pane_process_supervisor_service(
                supervisor_handle,
                AsyncPaneProcessSupervisorServiceConfig {
                    max_polls: u64::MAX,
                    take_limit: 8,
                    idle_interval: Duration::from_secs(60),
                    pane_service: AsyncPaneProcessServiceConfig {
                        max_polls: u64::MAX,
                        output_drain_limit: 1,
                        drain_limit: 8,
                        idle_interval: Duration::from_secs(60),
                        foreground_metadata_interval: Duration::from_secs(60),
                    },
                },
                |polls, _| polls >= 3,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(supervisor, actor.run());

    assert_eq!(report.spawned_workers, 1);
    assert_eq!(report.completed_workers, 1);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that timer side effects are consumed by the timer worker rather
/// than remaining as inert actor queue entries. The scheduled provider-poll
/// timer must re-enter the actor as a typed `TimerEvent`, which then produces a
/// provider-dispatch side effect through the same path used by direct timer
/// ingress.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_timer_side_effect_service_fires_scheduled_timers() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer { key, delay_ms: 1 }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let timer = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            100,
            |polls, _| polls >= 4,
        );
        let clock = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        };
        let (report, ()) = tokio::join!(timer, clock);
        let report = report.unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.scheduled, 1);
        assert_eq!(report.fired, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that cancelled runtime timers are removed before they can emit
/// stale events. This prevents old readiness, shell transaction, or resize
/// generations from racing later actor state after a newer timer supersedes
/// them.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_timer_side_effect_service_cancels_scheduled_timers() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::CursorBlink, "primary", 9);
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::CancelTimer { key },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 2);

        let timer = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            0,
            |polls, _| polls >= 2,
        );
        let clock = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        };
        let (report, ()) = tokio::join!(timer, clock);
        let report = report.unwrap();
        assert_eq!(report.drained, 2);
        assert_eq!(report.scheduled, 1);
        assert_eq!(report.cancelled, 1);
        assert_eq!(report.fired, 0);
        assert_eq!(report.submitted_events, 0);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    assert_eq!(exit.metrics.runtime_timer_schedules_queued, 1);
    assert_eq!(exit.metrics.runtime_timer_cancellations_queued, 1);
}

/// Verifies that program hook side effects are executed by the async hook
/// worker and reported back through typed actor events. This keeps lifecycle
/// hook process latency out of the actor while preserving ordered runtime
/// application of hook results.
#[tokio::test(flavor = "current_thread")]
async fn async_hook_side_effect_service_executes_program_hooks() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-hook-complete-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let payload_path = root.join("payload.json");
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RunProgramHook {
                plan: Box::new(HookExecutionPlan {
                    hook_id: "async-hook".to_string(),
                    event: HookEvent::ClientDetach,
                    run_in_focused_shell: false,
                    target_pane_id: None,
                    blocks_on_shell_availability: false,
                    program: Some("/bin/sh".to_string()),
                    args: vec![
                        "-c".to_string(),
                        "cat > \"$1\"".to_string(),
                        "hook".to_string(),
                        payload_path.display().to_string(),
                    ],
                    shell_command: None,
                    event_payload_json: r#"{"client_id":"primary"}"#.to_string(),
                    timeout_ms: 1_000,
                    on_failure: HookOnFailure::Warn,
                }),
                triggering_event_completed: true,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_hook_side_effect_service(
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
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(
            std::fs::read_to_string(&payload_path).unwrap(),
            r#"{"client_id":"primary"}"#
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-applied lifecycle events defer non-blocking configured
/// program hooks as hook-worker side effects. Blocking pre-action hooks remain
/// synchronous for now, but completed lifecycle hooks should no longer spawn
/// hook processes inside the actor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_completed_program_hooks_to_hook_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-hook-deferral-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let payload_path = root.join("detach.json");
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[hooks.detach]\nevent = \"client_detach\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"cat > \\\"$1\\\"\", \"hook\", \"{}\"]\non_failure = \"warn\"\n",
                payload_path.display()
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary,
            reason: "test disconnect".to_string(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let effects = handle.drain_hook_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            RuntimeSideEffect::RunProgramHook { plan, triggering_event_completed: true }
                if plan.hook_id == "detach"
        ));
        assert!(!payload_path.exists());
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
}

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
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
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

/// Verifies that audit records created through actor-owned runtime commands are
/// queued for the persistence worker instead of written from inside the actor.
/// The command still mutates policy state immediately, while the audit JSONL
/// append is drained through the target-specific persistence path.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_audit_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-audit-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_audit_log(crate::audit::AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: true,
        required: true,
    }));
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/approval full-access".to_string())
            .await
            .unwrap();
        assert!(response.contains("changed=true"), "{response}");
        assert!(!audit_path.exists());

        let persistence = run_async_persistence_side_effect_service(
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
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);

        let audit = std::fs::read_to_string(&audit_path).unwrap();
        assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
        assert!(
            audit.contains(r#""permission_id":"permissions.approval_policy""#),
            "{audit}"
        );
        assert!(audit.contains(r#""hash":"#), "{audit}");
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

/// Verifies that file-backed pane pipes start and append output through the
/// persistence worker in the async actor path. This keeps both `pipe-pane -o`
/// setup and later pane-output application from blocking on file I/O while
/// preserving the existing user behavior.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_file_pane_pipe_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary, format!("pipe-pane -o {}", path.display()))
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(
            !path.exists(),
            "async actor should not create file-backed pipe output before persistence worker drains"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-async\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let persistence = run_async_persistence_side_effect_service(
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
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("pipe-async")
        );
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
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that file-backed pane pipe persistence failures stop the active
/// pipe through the actor. A failed async append otherwise leaves runtime state
/// believing that pane output is still being captured even though subsequent
/// writes will continue to fail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_file_pane_pipe_after_persistence_failure() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-failed-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane -o {}", path.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-fail\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let persistence = run_async_persistence_side_effect_service(
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
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 0);
        assert_eq!(persistence.failed, 1);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
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
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""reason":"persistence-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that command-backed pane pipes are checked by actor-owned timers
/// after accepted pane output. The command writer can fail after `write_output`
/// has already accepted bytes into its bounded queue; the timer makes that
/// asynchronous failure visible and stops the active pipe without requiring a
/// later pane-output write or an explicit `pipe-pane --stop`.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_command_pane_pipe_after_health_timer() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-command-pane-pipe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("pipe-command.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nhead -c 1 >/dev/null\nsleep 0.02\nexit 7\n",
    )
    .unwrap();
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane /bin/sh {}", script.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    assert!(started.contains("mode=command"), "{started}");
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-command-health\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);
        tokio::time::sleep(Duration::from_millis(80)).await;

        let timers = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            1_000,
            |polls, _| polls >= 4,
        )
        .await
        .unwrap();
        assert_eq!(timers.drained, 1);
        assert_eq!(timers.fired, 1);
        assert_eq!(timers.submitted_events, 1);
        assert_eq!(timers.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""mode":"command""#)
            && event.payload.contains(r#""reason":"command-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-owned command-backed pane pipes receive a health timer
/// as soon as the terminal command starts the pipe and reschedule that health
/// check while the pipe command is still active. This protects command pipe
/// lifecycle cleanup from depending on unrelated pane output and keeps quick
/// exits or deferred startup failures discoverable through timer ingress.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_actor_schedules_command_pane_pipe_health_after_start() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary.clone(), "pipe-pane cat >/dev/null".to_string())
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(started.contains("mode=command"), "{started}");

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let first_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected pane-pipe health timer, got {other:?}"),
        };

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: first_key.clone(),
            now_ms: 1_060,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 0);
        assert_eq!(report.side_effects, 1);

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let second_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected rescheduled pane-pipe health timer, got {other:?}"),
        };
        assert!(second_key.generation > first_key.generation);

        let stopped = handle
            .execute_terminal_command(primary, "pipe-pane --stop".to_string())
            .await
            .unwrap();
        assert!(stopped.contains("pipe=stopped"), "{stopped}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
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
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
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

/// Verifies that config-target persistence rejects symlink destinations before
/// opening the file. Config writes carry user secrets and must preserve the
/// synchronous config writer's direct-private-file expectation when they move
/// onto the async persistence worker.
#[cfg(unix)]
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
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
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

/// Runs the test supervised service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_supervised_service(
    name: &'static str,
    exit: AsyncRuntimeServiceExit,
) -> AsyncRuntimeService {
    AsyncRuntimeService::new(name, async move { Ok(exit) })
}

/// Verifies that daemon supervision rejects invalid service sets before
/// spawning tasks. This matters because duplicate or missing listener names
/// would make later failure and shutdown reports ambiguous.
#[test]
fn async_runtime_service_supervisor_validates_service_set() {
    let empty_error = AsyncRuntimeServiceSupervisor::new(Vec::new()).unwrap_err();
    assert_eq!(empty_error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let unnamed_error = AsyncRuntimeServiceSupervisor::new(vec![test_supervised_service(
        " ",
        AsyncRuntimeServiceExit::completed(0),
    )])
    .unwrap_err();
    assert_eq!(
        unnamed_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );

    let duplicate_error = AsyncRuntimeServiceSupervisor::new(vec![
        test_supervised_service("control", AsyncRuntimeServiceExit::completed(0)),
        test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
    ])
    .unwrap_err();
    assert_eq!(
        duplicate_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert!(duplicate_error.message().contains("control"));
}

/// Exercises the successful path for multiple supervised services. The
/// assertion sorts by name so the test verifies task scheduling without
/// relying on Tokio's completion order for ready futures.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_reports_named_completions() {
    let report = supervise_async_runtime_services(
        vec![
            test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
            test_supervised_service("message", AsyncRuntimeServiceExit::completed(2)),
        ],
        std::future::pending(),
    )
    .await
    .unwrap();

    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(
        services,
        vec![
            AsyncRuntimeServiceReport {
                name: "control".to_string(),
                exit: AsyncRuntimeServiceExit::completed(1),
            },
            AsyncRuntimeServiceReport {
                name: "message".to_string(),
                exit: AsyncRuntimeServiceExit::completed(2),
            },
        ]
    );
}

/// Verifies that an auxiliary maintenance task does not keep supervision alive
/// after all primary services have completed. This protects daemon tests and
/// bounded listener runs from hanging behind the long-lived tick service while
/// still reporting that the tick task stopped without requesting shutdown.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_stops_auxiliary_after_primary_completion() {
    let report = supervise_async_runtime_services(
        vec![
            test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
            AsyncRuntimeService::new_auxiliary("tick", async {
                std::future::pending::<Result<AsyncRuntimeServiceExit>>().await
            }),
        ],
        std::future::pending(),
    )
    .await
    .unwrap();

    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(
        services,
        vec![
            AsyncRuntimeServiceReport {
                name: "control".to_string(),
                exit: AsyncRuntimeServiceExit::completed(1),
            },
            AsyncRuntimeServiceReport {
                name: "tick".to_string(),
                exit: AsyncRuntimeServiceExit::completed(0),
            },
        ]
    );
}

/// Ensures service task errors are propagated rather than hidden in a
/// nominal completion report. The service name is part of the diagnostic so
/// daemon startup can identify which listener failed.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_propagates_named_failures() {
    let error = supervise_async_runtime_services(
        vec![AsyncRuntimeService::new("events", async {
            Err(MezError::invalid_state("listener exited unexpectedly"))
        })],
        std::future::pending(),
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("events"));
    assert!(error.message().contains("listener exited unexpectedly"));
}

/// Covers external cancellation of a long-lived listener task. The task
/// never completes on its own, so the cancellation future is the only route
/// to a bounded shutdown report.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_reports_cancelled_services_as_shutdown() {
    use tokio::sync::oneshot;

    let (started_sender, started_receiver) = oneshot::channel();
    let (cancel_sender, cancel_receiver) = oneshot::channel();
    let pending_control = AsyncRuntimeService::new("control", async move {
        let _ = started_sender.send(());
        std::future::pending::<Result<AsyncRuntimeServiceExit>>().await
    });

    let supervision = supervise_async_runtime_services(vec![pending_control], async {
        let _ = cancel_receiver.await;
    });
    let canceller = async {
        started_receiver.await.unwrap();
        cancel_sender.send(()).unwrap();
    };

    let (report, ()) = tokio::join!(supervision, canceller);
    let report = report.unwrap();

    assert!(report.shutdown_requested);
    assert_eq!(
        report.services,
        vec![AsyncRuntimeServiceReport {
            name: "control".to_string(),
            exit: AsyncRuntimeServiceExit::shutdown(0),
        }]
    );
}

/// Verifies async agent provider service polls runtime queue.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_agent_provider_service_polls_runtime_queue() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 1,
        )
        .await
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.idle_polls, 1);
    assert_eq!(report.executions, 0);
    assert!(exit.commands_processed >= 3);
}

/// Verifies that an idle provider service performs a bounded actor-state probe
/// even when no notification arrives. This protects prompt submission on slow
/// systems from a missed side-effect notification permit: ordinary prompt work
/// still wakes the service immediately, while the bounded probe keeps queued
/// turns from staying stranded forever.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_uses_bounded_idle_probe() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let config = AsyncAgentProviderServiceConfig::new(1)
            .unwrap()
            .with_idle_interval(Duration::from_millis(10))
            .unwrap();
        let report = tokio::time::timeout(
            Duration::from_millis(50),
            run_async_agent_provider_service(&handle, config, |polls, _| polls >= 2),
        )
        .await
        .unwrap()
        .unwrap();
        let _ = handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(client, actor.run());
    assert_eq!(report.polls, 2);
    assert_eq!(report.idle_polls, 2);
    assert_eq!(report.executions, 0);
    assert!(exit.commands_processed >= 1);
}

/// Verifies that the provider service delegates provider-poll timer ownership
/// to the actor instead of retaining a local duplicate guard. With pending
/// provider work and no timer worker draining the queue, multiple idle provider
/// polls should leave exactly one scheduled provider-poll timer side effect.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_uses_actor_owned_provider_poll_guard() {
    let idle_interval = Duration::from_millis(25);
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let service_handle = handle.clone();
    let service = async move {
        run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1)
                .unwrap()
                .with_idle_interval(idle_interval)
                .unwrap(),
            |polls, _| polls >= 2,
        )
        .await
        .unwrap()
    };
    let clock = async {
        tokio::task::yield_now().await;
        tokio::time::advance(idle_interval).await;
        tokio::task::yield_now().await;
        tokio::time::advance(idle_interval).await;
    };
    let client = async {
        let (report, ()) = tokio::join!(service, clock);
        assert_eq!(report.polls, 2);
        assert_eq!(report.idle_polls, 2);
        assert_eq!(report.executions, 0);

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &timers[0] else {
            panic!("expected provider poll timer side effect, got {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ProviderPoll);
        assert_eq!(key.owner_id, "agent-provider");
        assert_eq!(*delay_ms, idle_interval.as_millis() as u64);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 1);
    assert!(exit.metrics.runtime_side_effects_drained >= 1);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that async provider worker failures can finish the active agent
/// turn through typed runtime event ingress. The failed event has enough
/// identity and error information to reuse the configured provider failure
/// path, including audit, prompt display, scheduler cleanup, and pending-task
/// removal, without returning an error to the daemon supervisor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_agent_provider_failure_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            kind: "invalid_state".to_string(),
            message: "provider worker failed before response".to_string(),
            provider_failure_json: None,
            provider_raw_text: None,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("provider_error"), "{pane_text}");
    assert!(
        pane_text.contains("provider worker failed before response"),
        "{pane_text}"
    );
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies model-correctable file-action failures enqueue the next provider
/// dispatch immediately through the async actor.
///
/// The runtime service stores the retry context, but the async actor owns
/// side-effect dispatch. A failed provider completion that queues action
/// failure feedback must therefore emit a fresh provider-dispatch side effect
/// instead of waiting for an unrelated timer path before the model can
/// self-correct.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_dispatches_provider_retry_after_file_action_failure_feedback() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "write then inspect")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .next()
        .expect("prompt should queue a provider task");
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 2,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let write_action = crate::agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "write a source file".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: src/generated.rs\n+content\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let read_action = crate::agent::AgentAction {
        id: "read-unsent".to_string(),
        rationale: "read the source file".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Read the source file".to_string(),
            command: "sed -n '1,120p' src/generated.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &write_action,
        crate::agent::ActionStatus::Failed,
        "pane_input_write_failed",
        "pane input write failed while sending shell action",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "state": "pane-input-write-failed"
            }
        })
        .to_string(),
    );
    let pending = crate::agent::ActionResult::running(
        &turn,
        &read_action,
        vec!["local action accepted for pane execution".to_string()],
        Some(r#"{"state":"pending_dispatch"}"#.to_string()),
    );
    let batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![write_action, read_action],
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
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::Shell,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "write then inspect".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "failed file action response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![failed, pending],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Failed,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let report = handle.submit_runtime_events(provider_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(dispatches.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }
                if turn_id == &task.turn_id
        )));
        let pending = handle.pending_agent_provider_tasks().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].turn_id, task.turn_id);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that async provider worker completions can apply a model-produced
/// execution through typed runtime ingress. This keeps successful provider
/// output on the same actor-owned transcript, audit, scheduler, prompt display,
/// and pane rendering path as the compatibility provider poller while allowing
/// future workers to perform network I/O outside the actor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_agent_provider_completion_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let action = crate::agent::AgentAction {
        id: "say-1".to_string(),
        rationale: "complete with a visible summary".to_string(),
        payload: crate::agent::AgentActionPayload::Say {
            status: crate::agent::SayStatus::Final,
            text: "Typed completion applied.".to_string(),
            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: true,
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
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::RespondOnly,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "summarize the pane".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "Typed completion applied.".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![crate::agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["Typed completion applied.".to_string()],
            Some(
                r#"{"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Typed completion applied."}"#
                    .to_string(),
            ),
        )],
        final_turn: true,
        terminal_state: crate::agent::AgentTurnState::Completed,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("Typed completion applied."),
        "{pane_text}"
    );
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that provider completions queue durable transcript entries for the
/// persistence worker when a transcript store is configured. The actor assigns
/// transcript sequence numbers and records the pane reference immediately, while
/// filesystem writes happen only after the persistence worker drains the typed
/// transcript side effect.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_transcript_entries_to_persistence_worker() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-transcript-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let transcript_path = transcript_store.transcript_path(&conversation_id).unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let action = crate::agent::AgentAction {
        id: "say-1".to_string(),
        rationale: "complete with a visible summary".to_string(),
        payload: crate::agent::AgentActionPayload::Say {
            status: crate::agent::SayStatus::Final,
            text: "Typed transcript completion.".to_string(),
            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: true,
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
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::RespondOnly,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "summarize the pane".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "Typed transcript completion.".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![crate::agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["Typed transcript completion.".to_string()],
            Some(
                r#"{"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Typed transcript completion."}"#
                    .to_string(),
            ),
        )],
        final_turn: true,
        terminal_state: crate::agent::AgentTurnState::Completed,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 2);
        assert!(!transcript_path.exists());

        let persistence = run_async_persistence_side_effect_service(
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
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let entries = transcript_store.inspect(&conversation_id).unwrap();
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .any(|entry| { entry.content.contains("Typed transcript completion.") })
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies that submitted agent prompts are appended to the shared prompt
/// history through the persistence worker instead of being written while actor
/// state is applying the prompt. This keeps the hot input path non-blocking
/// while preserving the global prompt-history UX across sessions.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_prompt_history_to_persistence_worker() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-prompt-history-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let prompt_history_path = transcript_store.prompt_history_file();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "remember this prompt".to_string())
            .await
            .unwrap();
        assert!(response.contains(r#""state":"running""#), "{response}");
        assert!(!prompt_history_path.exists());
        assert!(
            transcript_store
                .prompt_history(&conversation_id)
                .unwrap()
                .is_empty()
        );

        let persistence = run_async_persistence_side_effect_service(
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
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        assert_eq!(
            transcript_store.prompt_history(&conversation_id).unwrap(),
            vec![String::from("remember this prompt")]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies that the async actor defers `/init` scaffold creation to the
/// persistence worker. The command path records the user-visible mutation
/// immediately, but the project instruction file is created by the async
/// persistence worker so the actor does not perform the potentially slow file
/// write inline.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_init_scaffold_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-agent-init-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let scaffold = root.join("AGENTS.md");
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    service
        .apply_pane_foreground_process_event(
            started.pane_id.clone(),
            "sleep",
            started.primary_pid,
            Some(root.to_string_lossy().to_string()),
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/init".to_string())
            .await
            .unwrap();
        assert!(response.contains(r#""kind":"mutated""#), "{response}");
        assert!(response.contains(r#""command":"init""#), "{response}");
        assert!(response.contains("created=true"), "{response}");
        assert!(
            !scaffold.exists(),
            "async actor should not create AGENTS.md before persistence worker drains"
        );

        let persistence = run_async_persistence_side_effect_service(
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
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let text = std::fs::read_to_string(&scaffold).unwrap();
        assert!(text.contains("# Repository Guidelines"), "{text}");
        assert!(
            text.contains("## Build, Test, and Development Commands"),
            "{text}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(root);
}
