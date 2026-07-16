//! Async-runtime tests owned by rendering behavior.

use super::super::*;

/// Verifies that a foreground resize signal can wake the render path without
/// directly mutating geometry in the actor event. The attached terminal service
/// owns the actual terminal-size read, so the signal event should only enqueue
/// a resize render invalidation for the target client.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_client_resize_signal_as_render_invalidation() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::ResizeSignal {
            client_id: primary.clone(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        let effects = handle
            .drain_render_side_effects_for_client(primary.clone(), 8)
            .await
            .unwrap();
        assert_eq!(
            effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::Resize,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(80, 24).unwrap()
    );
    assert_eq!(exit.commands_processed, 3);
}

/// Verifies that client output-readiness events are applied only for attached
/// clients and enqueue a render side effect without composing or writing the
/// frame inside the actor. This keeps slow or backpressured frame delivery on
/// the side-effect boundary while still waking the eventual render worker as
/// soon as stdout becomes writable again.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_client_output_ready_events_as_render_side_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::OutputReady {
            client_id: primary.clone(),
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
    assert_eq!(exit.commands_processed, 3);
}

/// Verifies that render-only runtime timers become actor-owned side-effect
/// producers instead of accepted-only bookkeeping. Resize debounce and cursor
/// blink timers should not mutate session state, but they must wake frame
/// delivery so attached clients repaint at the correct moments without a blind
/// compatibility tick.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_render_timer_events_as_side_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let resize_key =
            RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, primary.as_str(), 1);
        let cursor_key = RuntimeTimerKey::new(RuntimeTimerKind::CursorBlink, primary.as_str(), 2);
        let status_key = RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, primary.as_str(), 3);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: resize_key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: cursor_key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: status_key.clone(),
                    delay_ms: 1,
                },
            ])
            .await
            .unwrap();
        let scheduled = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(scheduled.len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: resize_key,
            now_ms: 100,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: cursor_key,
            now_ms: 200,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: status_key,
            now_ms: 300,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: RuntimeTimerKey::new(RuntimeTimerKind::IdleCleanup, "session", 4),
            now_ms: 400,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 4);
        assert_eq!(report.applied, 3);
        assert_eq!(report.side_effects, 4);
        let side_effects = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(
            side_effects,
            vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::Resize,
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::StatusRefresh,
                        primary.as_str(),
                        1300,
                    ),
                    delay_ms: 1000,
                },
            ]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
    assert_eq!(exit.metrics.runtime_timer_events_ignored, 1);
}

/// Verifies that resize debounce timer events are generation checked by the
/// actor before producing a render invalidation. Rapid resize activity cancels
/// the old debounce key and schedules a new one, so a late firing for the stale
/// key must not force another full-size repaint.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_ignores_stale_resize_debounce_timer_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let stale_key = RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, primary.as_str(), 1);
        let active_key =
            RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, primary.as_str(), 2);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: stale_key.clone(),
                    delay_ms: 50,
                },
                RuntimeSideEffect::CancelTimer {
                    key: stale_key.clone(),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: active_key.clone(),
                    delay_ms: 50,
                },
            ])
            .await
            .unwrap();
        let scheduled = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(scheduled.len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: stale_key,
            now_ms: 100,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: active_key,
            now_ms: 100,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::Resize,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
}

/// Verifies that render drains coalesce redundant invalidations for the same
/// client before a render worker composes frames. The async render queue can
/// receive several causes while a client is not writable; the worker should
/// render that client once, keep the strongest redraw reason, and preserve
/// unrelated side-effect families in queue order.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_render_side_effects_by_client() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let other = ClientId::new('c', 9006);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: AgentId::opaque("agent-%1").unwrap(),
                    turn_id: "turn-1".to_string(),
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::FullRedraw,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: other.clone(),
                    reason: RenderInvalidationReason::CursorBlink,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 4);

        let render_effects = handle.drain_render_side_effects(8).await.unwrap();
        assert_eq!(
            render_effects,
            vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary,
                    reason: RenderInvalidationReason::FullRedraw,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: other,
                    reason: RenderInvalidationReason::CursorBlink,
                },
            ]
        );
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque("agent-%1").unwrap(),
                turn_id: "turn-1".to_string(),
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 3);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 3);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 1);
}

/// Verifies that render invalidations are coalesced before the actor applies
/// its bounded queue capacity check. A pane can emit output faster than the
/// attached client redraw path drains invalidations, and redundant redraw
/// requests for the same client must not be able to exhaust the shared
/// side-effect queue.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_render_side_effects_before_capacity_check() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::CursorBlink,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::FullRedraw,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 3);
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
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
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 2);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
}

/// Verifies that full client-output flushes are coalesced before bounded queue
/// capacity is checked. Pane output bursts can produce new full-frame flushes
/// faster than a terminal can write them; only the latest pending frame for a
/// client needs to survive because the frame is a complete presentation state.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_client_flush_side_effects_before_capacity_check() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["stale-one".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_column: 1,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["stale-two".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_column: 2,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["latest".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_column: 3,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 3);

        let effects = handle
            .drain_client_output_flush_side_effects(Some(primary), 8)
            .await
            .unwrap();
        assert_eq!(effects.len(), 1);
        let RuntimeSideEffect::FlushClientOutput { lines, modes, .. } = &effects[0] else {
            panic!("expected retained client output flush, got {effects:?}");
        };
        assert_eq!(lines, &vec!["latest".to_string()]);
        assert_eq!(modes.cursor_column, 3);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 2);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
}

/// Verifies that a foreground attached client can drain only its own render
/// invalidations while preserving other clients and side-effect families. This
/// lets the live foreground service wake on side-effect notifications without
/// treating unrelated timer, provider, or observer work as a redraw request.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_render_side_effects_for_one_client() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let other = ClientId::new('c', 9016);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::RenderClient {
                    client_id: other.clone(),
                    reason: RenderInvalidationReason::CursorBlink,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: AgentId::opaque("agent-%1").unwrap(),
                    turn_id: "turn-1".to_string(),
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::FullRedraw,
                },
            ])
            .await
            .unwrap();

        assert_eq!(
            handle
                .drain_render_side_effects_for_client(primary.clone(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::FullRedraw,
            }]
        );
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: other,
                reason: RenderInvalidationReason::CursorBlink,
            }]
        );
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque("agent-%1").unwrap(),
                turn_id: "turn-1".to_string(),
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
}

/// Verifies that state-mutating runtime events can enqueue bounded actor-owned
/// side effects without executing those effects immediately. This is the
/// migration point for render invalidation, pane I/O writes, and other external
/// work that must eventually leave the actor through supervised async workers
/// instead of direct synchronous service calls.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_queues_render_side_effects_for_applied_events() {
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
            bytes: b"side-effect-output\n".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let effects = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(
            effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        assert!(
            handle
                .drain_runtime_side_effects(8)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 4);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that actor metrics expose rendered-view and terminal control
/// request counts that can be used for idle attach benchmarking. The counters
/// distinguish direct actor render calls from control-socket `terminal/view`
/// and `terminal/step` traffic so regressions toward periodic redraws remain
/// measurable.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_metrics_track_render_and_terminal_control_requests() {
    use crate::control::encode_control_body;

    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .render_client_frame(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
                true,
            )
            .await
            .unwrap();
        handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap();
        let terminal_step = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"step","method":"terminal/step","params":{"idempotency_key":"metrics-step","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[]}}"#,
        );
        let terminal_view = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        );
        handle
            .handle_control_input_for_connection(
                [terminal_step, terminal_view].concat(),
                1024 * 1024,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let metrics = handle.metrics().await.unwrap();
        assert_eq!(metrics.render_client_frame_requests, 1);
        assert_eq!(metrics.render_client_view_requests, 1);
        assert_eq!(metrics.terminal_step_control_requests, 1);
        assert_eq!(metrics.terminal_view_control_requests, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.render_client_frame_requests, 1);
    assert_eq!(exit.metrics.render_client_view_requests, 1);
    assert_eq!(exit.metrics.terminal_step_control_requests, 1);
    assert_eq!(exit.metrics.terminal_view_control_requests, 1);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies async actor serializes lifecycle render and shutdown.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_serializes_lifecycle_render_and_shutdown() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.authoritative_size, Size::new(80, 24).unwrap());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.commands_processed, 3);
}
