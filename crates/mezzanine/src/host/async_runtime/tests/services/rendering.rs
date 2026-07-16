//! Async-runtime tests owned by rendering behavior.

use super::super::*;

/// Verifies that the concrete render side-effect service converts actor-owned
/// render invalidations into styled client-output flush effects. The service
/// composes frames through the actor and hands the resulting flush to a worker
/// callback without draining unrelated side-effect families.
#[tokio::test(flavor = "current_thread")]
async fn async_render_side_effect_service_composes_flush_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let flushed = StdArc::new(Mutex::new(Vec::new()));
    let flushed_for_service = flushed.clone();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"render-service-output\n".to_vec(),
        }));
        let ingress = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(ingress.side_effects, 1);

        let report = run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |effect| {
                flushed_for_service.lock().unwrap().push(effect);
                Ok(())
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 1);
        assert_eq!(report.applied, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &timers[0] else {
            panic!("expected status refresh timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(*delay_ms, 1000);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_drained, 2);
    let flushed = flushed.lock().unwrap();
    assert_eq!(flushed.len(), 1);
    let RuntimeSideEffect::FlushClientOutput {
        client_id,
        lines,
        line_style_spans,
        modes,
    } = &flushed[0]
    else {
        panic!("render service should emit a flush side effect");
    };
    assert_eq!(client_id, &primary);
    assert_eq!(lines.len(), line_style_spans.len());
    assert!(
        lines
            .iter()
            .any(|line| line.contains("render-service-output"))
    );
    assert!(modes.cursor_visible);
}

/// Verifies that active agent pane status can drive status refresh timers even
/// when the window status line is disabled. The running status pill has an
/// animated scan background, so pane-frame-only configurations need the
/// animation refresh cadence while agent work is active.
#[tokio::test(flavor = "current_thread")]
async fn async_render_side_effect_service_refreshes_active_agent_status_without_window_status() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nenabled = false\n[frames.pane]\nenabled = true\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        let report = run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |_| Ok(()),
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.applied, 1);

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
                        && *delay_ms == 180
            )),
            "{timers:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed > 0);
}

/// Verifies that an existing slow status refresh timer is replaced by the
/// animation cadence when an agent starts running. A window status timer may
/// already exist before the prompt is submitted, but active agent indicators
/// should not wait for that slower deadline before animating.
#[tokio::test(flavor = "current_thread")]
async fn async_render_side_effect_service_retargets_status_refresh_for_agent_animation() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |_| Ok(()),
            |_, _| false,
        )
        .await
        .unwrap();
        let initial_timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            initial_timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
                        && *delay_ms == 1000
            )),
            "{initial_timers:?}"
        );

        let start = handle
            .execute_agent_shell_command(primary.clone(), "summarize the pane".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::AgentPrompt,
            }])
            .await
            .unwrap();
        run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |_| Ok(()),
            |_, _| false,
        )
        .await
        .unwrap();
        let animation_timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            animation_timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::CancelTimer { key }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
            )),
            "{animation_timers:?}"
        );
        assert!(
            animation_timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
                        && *delay_ms == 180
            )),
            "{animation_timers:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed > 0);
}

/// Verifies that flush side effects can be queued through the actor and drained
/// independently by the client output worker. This is the output half of the
/// render pipeline: render workers can enqueue styled flushes without sharing
/// a mutable queue, and output workers can write them without stealing render
/// or provider side effects.
#[tokio::test(flavor = "current_thread")]
async fn async_client_output_flush_service_writes_styled_flush_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let other_client = ClientId::new('c', 9005);
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["flush-one".to_string(), "flush-two".to_string()],
                    line_style_spans: vec![Vec::new(), Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_visible: true,
                        cursor_row: 1,
                        cursor_column: 2,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
                RuntimeSideEffect::FlushClientOutput {
                    client_id: other_client.clone(),
                    lines: vec!["other-client".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes::default(),
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 2);

        let mut io = AsyncFakeAttachedTerminalIo::default();
        let report = run_async_client_output_flush_service(
            &handle,
            primary.clone(),
            &mut io,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 1);
        assert_eq!(report.flushed, 1);
        assert_eq!(report.output_hangups, 0);
        assert_eq!(io.written_frames.len(), 1);
        assert_eq!(
            io.written_frames[0].lines,
            vec!["flush-one".to_string(), "flush-two".to_string()]
        );
        assert_eq!(io.written_frames[0].line_style_spans.len(), 2);
        assert_eq!(io.written_frames[0].modes.cursor_column, 2);
        let retained = handle
            .drain_client_output_flush_side_effects(Some(other_client), 8)
            .await
            .unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 2);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 2);
}

/// Verifies that the client output worker prefers a newer frame over stale
/// pending output.
///
/// Slow terminal transports can return from a bounded write with bytes still
/// pending. If a newer frame is already queued, the worker should materialize
/// the latest state instead of spending bandwidth on obsolete frame bytes.
#[tokio::test(flavor = "current_thread")]
async fn async_client_output_flush_service_prefers_new_frame_over_stale_pending_output() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let pending_output_bytes = StdArc::new(AtomicUsize::new(256));
    let stale_flushes = StdArc::new(AtomicUsize::new(0));

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::FlushClientOutput {
                client_id: primary.clone(),
                lines: vec!["newer frame".to_string()],
                line_style_spans: vec![Vec::new()],
                modes: AttachedTerminalOutputModes::default(),
            }])
            .await
            .unwrap();
        let mut io = SupersedablePendingOutputIo::new(
            write_count.clone(),
            write_notify,
            pending_output_bytes.clone(),
            stale_flushes.clone(),
        );

        let report = run_async_client_output_flush_service(
            &handle,
            primary.clone(),
            &mut io,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();

        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 1);
        assert_eq!(report.flushed, 1);
        assert_eq!(report.partial_writes, 0);
        assert!(report.bytes_written > 0);
        assert_eq!(report.pending_output_bytes, 0);
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);
        assert_eq!(pending_output_bytes.load(Ordering::SeqCst), 0);
        assert_eq!(
            handle
                .drain_client_output_flush_side_effects(Some(primary), 8)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
}

/// Verifies that the async runtime terminal command path exposes the current
/// actor counters and histograms through `show-metrics` for pager rendering.
#[tokio::test(flavor = "current_thread")]
async fn async_terminal_show_metrics_command_renders_actor_metrics() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"show-metrics\n".to_vec(),
        }));
        handle.submit_runtime_events(batch).await.unwrap();
        let output = handle
            .execute_terminal_command(primary, "show-metrics".to_string())
            .await
            .unwrap();
        assert!(output.contains(r#""command":"show-metrics""#), "{output}");
        assert!(
            output.contains("metrics source=async-runtime status=available"),
            "{output}"
        );
        assert!(
            output.contains("metrics source=runtime-service status=available"),
            "{output}"
        );
        assert!(output.contains("[runtime counts]"), "{output}");
        assert!(output.contains("provider_requests_started ="), "{output}");
        assert!(output.contains("[runtime histograms]"), "{output}");
        assert!(
            output.contains("provider_prompt_cacheable_prefix_bytes"),
            "{output}"
        );
        assert!(
            output.contains("provider_prompt_stable_prefix_bytes"),
            "{output}"
        );
        assert!(output.contains("provider_request_shape_bytes"), "{output}");
        assert!(output.contains("[async runtime counts]"), "{output}");
        assert!(output.contains("commands_processed ="), "{output}");
        assert!(output.contains("[async runtime histograms]"), "{output}");
        assert!(output.contains("runtime_event_batch_sizes"), "{output}");
        assert!(output.contains("pane_output_chunk_bytes"), "{output}");
        assert!(
            output.contains("side_effect_queue_depth_samples"),
            "{output}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
}
