//! Async-runtime tests owned by panes behavior.

use super::super::*;

/// Verifies that a typed pane-output runtime event is applied by the actor using
/// the same terminal-screen, OSC, shell-transaction, event-log, and title-update
/// machinery as the legacy polling path. This closes the first production gap
/// between the async pane driver boundary and visible runtime state.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_pane_output_events_to_rendered_view() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"event-output\n".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.families, vec!["pane"]);

        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            view.lines.iter().any(|line| line.contains("event-output")),
            "{:?}",
            view.lines
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 3);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies pane output and queued I/O are fenced by the exact process
/// generation after async ownership begins.
///
/// A replacement process can reuse the same pane id while the prior worker is
/// still draining. Output from a stale generation must not mutate the current
/// screen, and one generation must not consume another generation's input or
/// termination effects.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_fences_stale_pane_process_generations() {
    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_process_instances_for_adapter(8)
            .await
            .unwrap();
        let (current, mut process) = processes.pop().unwrap();
        let stale = PaneProcessInstance {
            pane_id: current.pane_id.clone(),
            generation: current.generation.saturating_add(1),
        };

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::PaneProcess {
            instance: stale.clone(),
            event: PaneProcessEvent::Pane(PaneEvent::Output {
                pane_id: stale.pane_id.clone(),
                bytes: b"stale-generation-output\n".to_vec(),
            }),
        });
        batch.push(RuntimeEvent::PaneProcess {
            instance: current.clone(),
            event: PaneProcessEvent::Pane(PaneEvent::Output {
                pane_id: current.pane_id.clone(),
                bytes: b"current-generation-output\n".to_vec(),
            }),
        });
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 1);

        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::PaneProcessIo {
                    instance: stale.clone(),
                    effect: PaneProcessIoEffect::Terminate { force: true },
                },
                RuntimeSideEffect::PaneProcessIo {
                    instance: current.clone(),
                    effect: PaneProcessIoEffect::WriteInput {
                        bytes: b"current-input".to_vec(),
                    },
                },
            ])
            .await
            .unwrap();
        assert_eq!(
            handle
                .drain_pane_process_io_side_effects(current.clone(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::PaneProcessIo {
                instance: current.clone(),
                effect: PaneProcessIoEffect::WriteInput {
                    bytes: b"current-input".to_vec(),
                },
            }]
        );
        assert_eq!(
            handle
                .drain_pane_process_io_side_effects(stale.clone(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::PaneProcessIo {
                instance: stale,
                effect: PaneProcessIoEffect::Terminate { force: true },
            }]
        );

        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        let _ = process.terminate(Duration::from_millis(10));
        current.pane_id
    };

    let (pane_id, mut exit) = tokio::join!(client, actor.run());
    let content = exit
        .service
        .pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(content.contains("current-generation-output"), "{content:?}");
    assert!(!content.contains("stale-generation-output"), "{content:?}");
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that foreground process metadata from an async pane worker updates
/// pane title state through the actor. This protects automatic title refresh
/// after live PTY ownership has moved out of the synchronous manager.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_foreground_process_metadata_to_pane_title() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::ForegroundProcess {
            pane_id: "%1".to_string(),
            process_name: "vim".to_string(),
            process_group_id: 4242,
            current_working_directory: Some("/tmp/mez-async-title".to_string()),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.families, vec!["pane"]);
        assert!(
            report.side_effects > 0,
            "title changes should invalidate rendered pane frames"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let window = exit.service.session().active_window().unwrap();
    assert_eq!(window.active_pane().title, "vim");
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""title":"vim""#), "{events}");
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that typed process-exit events close the pane through the actor
/// using the same session, registry, event-log, pane-pipe, and agent-turn
/// cleanup path as the legacy process polling loop. Async process watchers need
/// this path before pane lifecycle polling can be removed from daemon ticks.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_process_exit_events_to_session_state() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Exited {
            pane_id: "%1".to_string(),
            primary_pid: None,
            exit_code: Some(0),
            signal: None,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.families, vec!["process"]);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 2);
    assert!(exit.service.session().windows().is_empty());
}

/// Verifies that typed process-spawn events initialize pane lifecycle state
/// through the actor instead of remaining accepted-only bookkeeping. Live async
/// pane ownership will emit this event after a process handle is created, so
/// clients need the normal pane-start lifecycle event and redraw invalidation.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_process_spawn_events_to_event_log() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Spawned {
            pane_id: "%1".to_string(),
            pid: Some(42),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::FullRedraw,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""process_state":"running""#)
            && event.payload.contains(r#""primary_pid":42"#)
    }));
    assert_eq!(exit.commands_processed, 3);
}

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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

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
    exit.service.terminate_all_pane_processes().unwrap();
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

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
    exit.service.terminate_all_pane_processes().unwrap();
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_adapter(8)
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
        exit.service.pane_process_is_adapter_owned(&pane_id),
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_adapter(8)
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
            .rposition(|line| line.contains("mez>"))
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_adapter(8)
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
        exit.service.pane_process_is_adapter_owned(&pane_id),
        "forced shutdown must retain the terminating process generation until its exit settles"
    );
    assert!(exit.service.session().windows().is_empty());
}

/// Verifies that pane input produced by the compatibility terminal-step path is
/// converted into actor side effects after a pane has been handed to an async
/// process owner. This protects older control-socket attach calls while the
/// production foreground path moves to fully deferred pane I/O.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_service_deferred_input_after_pane_handoff() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_process_instances_for_adapter(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (instance, mut process) = processes.pop().unwrap();
        let step = AttachedTerminalClientStepPlan {
            actions: vec![TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec())],
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let application = handle
            .apply_attached_terminal_step_plan(primary, step)
            .await
            .unwrap();
        assert_eq!(application.forwarded_bytes, 6);
        assert_eq!(
            handle
                .drain_pane_process_io_side_effects(instance.clone(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: PaneProcessIoEffect::WriteInput {
                    bytes: b"hello\n".to_vec(),
                },
            }]
        );
        let _ = process.terminate(Duration::from_millis(10));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.terminate_all_pane_processes().is_ok());
}

/// Verifies that pane close commands produce async termination side effects
/// after pane process ownership has moved out of the manager. A foreground
/// process observation can race with that termination after the pane has left
/// the layout, so current-generation metadata must become a harmless no-op
/// rather than failing the actor with `pane not found`.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_service_deferred_termination_after_pane_handoff() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .split_pane_with_process(
            &primary,
            mez_mux::layout::SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_process_instances_for_adapter(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 2);
        let removed_index = processes
            .iter()
            .position(|(instance, _)| instance.pane_id == "%1")
            .unwrap();
        let (instance, mut process) = processes.swap_remove(removed_index);
        let (_, mut remaining_process) = processes.pop().unwrap();
        let output = handle
            .execute_terminal_command(primary, "kill-pane --force -t %1".to_string())
            .await
            .unwrap();
        assert!(output.contains("closed=true"));

        let mut late_metadata = RuntimeEventBatch::new();
        late_metadata.push(RuntimeEvent::PaneProcess {
            instance: instance.clone(),
            event: PaneProcessEvent::Pane(PaneEvent::ForegroundProcess {
                pane_id: instance.pane_id.clone(),
                process_name: "cat".to_string(),
                process_group_id: process.primary_pid(),
                current_working_directory: Some("/tmp/removed-pane".to_string()),
            }),
        });
        let report = handle.submit_runtime_events(late_metadata).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 0);

        assert_eq!(
            handle
                .drain_pane_io_side_effects(instance.pane_id.as_str(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::TerminatePane {
                pane_id: instance.pane_id,
                force: true,
            }]
        );
        let _ = process.terminate(Duration::from_millis(10));
        let _ = remaining_process.terminate(Duration::from_millis(10));
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.terminate_all_pane_processes().is_ok());
}
