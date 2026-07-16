//! Async-runtime tests owned by terminal service behavior.

use super::super::*;

/// Verifies that the higher-level attached-terminal client service can use the
/// deferred pane I/O mode across its prepolled batch boundary. Foreground
/// daemon attach uses this service wrapper, so the production handoff needs the
/// service-level path as well as the single-loop path.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_can_defer_pane_input_to_worker() {
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
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        input_batches: vec![b"service-input\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert!(
            report
                .loop_report
                .actions
                .contains(&TerminalClientLoopAction::ForwardToPane(
                    b"service-input\n".to_vec()
                ))
        );
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"service-input\n".to_vec(),
            }]
        );
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that slow client-output flushing does not block foreground input
/// routing. The first service batch starts a large frame and leaves bytes
/// pending; the second batch observes user input before that frame has been
/// fully written and still forwards the payload to the primary pane worker.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_routes_input_while_output_is_pending() {
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
    let mut io = SlowOutputAttachedTerminalLoopIo::new(
        vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        vec![b"hello\n".to_vec()],
        64,
    );

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert!(report.loop_report.partial_writes > 0);
        assert!(report.loop_report.pending_output_bytes > 0);
        assert!(
            report
                .loop_report
                .actions
                .contains(&TerminalClientLoopAction::ForwardToPane(
                    b"hello\n".to_vec()
                ))
        );
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        assert_eq!(io.completed_frames, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies async attached terminal service runs batches until hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_runs_batches_until_hangup() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9002),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 8 },
            |iteration| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::Plain,
                    text: format!("service-{iteration}"),
                }))
            },
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.iterations, 2);
        assert_eq!(report.loop_report.output_frames, 1);
        assert_eq!(report.loop_report.input_hangups, 1);
        assert_eq!(io.written_batches.len(), 1);
        assert_eq!(io.written_batches[0][23].trim_end(), "service-0");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that an attached-terminal service wakes between batches when the
/// actor queues side effects. This keeps render/output work responsive now that
/// quiet periods no longer have a periodic foreground redraw sleep.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_wakes_between_batches_on_side_effects() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![Vec::new(), Vec::new()],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let service_handle = handle.clone();
    let notify_handle = handle.clone();
    let client = async move {
        let service = run_async_attached_terminal_client_service(
            &service_handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9022),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        );
        let notifier = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(10)).await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: ClientId::new('c', 9022),
                    reason: RenderInvalidationReason::CursorBlink,
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(service, notifier)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.iterations, 2);
        assert_eq!(
            service_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
}

/// Verifies that a quiet attached-terminal service does not advance from an
/// idle timeout after its initial frame. This protects the foreground path from
/// reintroducing a periodic redraw clock that consumes CPU while the terminal is
/// idle.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_has_no_idle_batch_timer() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let mut io = IdleAsyncAttachedTerminalLoopIo::new(write_count.clone(), write_notify.clone());

    let client = async {
        let service = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9023),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        );
        tokio::pin!(service);
        tokio::select! {
            _ = write_notify.notified() => {}
            result = &mut service => panic!("attached terminal service completed before idling: {result:?}"),
        }
        let advance = async {
            tokio::time::advance(Duration::from_millis(250)).await;
        };
        let (result, ()) = tokio::join!(
            tokio::time::timeout(Duration::from_millis(200), &mut service),
            advance
        );
        assert!(result.is_err());
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
}

/// Verifies that the attached-terminal service treats queued render work as
/// level-triggered state instead of relying only on a retained notify permit.
///
/// A background service can consume the one stored side-effect notification
/// before the foreground client reaches its idle wait. The render invalidation
/// itself remains queued in the actor, and the client must drain it before
/// awaiting fresh input so a quiet terminal cannot strand a repaint forever.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_drains_stranded_render_effect_before_waiting() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let client_id = ClientId::new('c', 9024);
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let mut io = IdleAsyncAttachedTerminalLoopIo::new(write_count.clone(), write_notify.clone());

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: client_id.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        handle.wait_for_runtime_side_effects().await;

        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_attached_terminal_client_service(
                &handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Observer,
                    client_id,
                    primary_client_id: None,
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that bursty render invalidations are coalesced behind the
/// configured foreground render rate and still produce one trailing frame.
///
/// This protects slow remote clients from being flooded by intermediate frames
/// while preserving the final visible state after an output burst settles.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_rate_limits_bursty_render_invalidations() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_notify = write_notify.clone();
        let service_write_count = write_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io =
                IdleAsyncAttachedTerminalLoopIo::new(service_write_count, service_write_notify);
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        for _ in 0..3 {
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                }])
                .await
                .unwrap();
        }
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(199)).await;
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..8 {
            if write_count.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that the foreground attached-terminal service polls terminal
/// dimensions while otherwise idle.
///
/// Some hosting terminals can change cell dimensions without producing an
/// input or runtime event that wakes the render service. The idle resize poll
/// should notice that size change, invalidate retained diff state, and repaint
/// exactly once instead of waiting for user interaction.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_polls_terminal_size_while_idle() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let invalidate_count = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_invalidate_count = invalidate_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io = InvalidatingIdleAsyncAttachedTerminalLoopIo::new(
                service_write_count,
                service_write_notify,
                service_invalidate_count,
            )
            .with_terminal_size_batches(vec![None, Some(Size::new(100, 30).unwrap())]);
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        let deadline = Instant::now() + Duration::from_millis(500);
        while write_count.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(invalidate_count.load(Ordering::SeqCst), 1);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 5);
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(100, 30).unwrap()
    );
}

/// Verifies that resize render invalidations interrupt an already pending
/// ordinary render-rate wait. Slow remote terminals can leave pane-output
/// refreshes coalesced behind the frame cadence, but a hosting terminal resize
/// changes the visible geometry and must immediately discard retained diff
/// state before repainting instead of waiting for the next pane-output tick.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_resize_bypasses_pending_render_rate_limit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let invalidate_count = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_invalidate_count = invalidate_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io = InvalidatingIdleAsyncAttachedTerminalLoopIo::new(
                service_write_count,
                service_write_notify,
                service_invalidate_count,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(50)).await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::Resize,
            }])
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(1), write_notify.notified())
            .await
            .unwrap();
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(invalidate_count.load(Ordering::SeqCst), 1);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that a newer rate-limited render supersedes stale pending output.
///
/// Slow clients can leave bytes from an older frame pending. During rapid pane
/// output, the attached client should wait for the next render tick and write
/// the latest frame instead of streaming obsolete pending bytes immediately.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_does_not_flush_stale_pending_output_before_render_tick() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let pending_output_bytes = StdArc::new(AtomicUsize::new(0));
    let stale_flushes = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_pending_output_bytes = pending_output_bytes.clone();
        let service_stale_flushes = stale_flushes.clone();
        let service_task = tokio::spawn(async move {
            let mut io = SupersedablePendingOutputIo::new(
                service_write_count,
                service_write_notify,
                service_pending_output_bytes,
                service_stale_flushes,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        pending_output_bytes.store(1024, Ordering::SeqCst);

        for _ in 0..3 {
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                }])
                .await
                .unwrap();
        }

        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_millis(199)).await;
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..8 {
            if write_count.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);
        assert_eq!(pending_output_bytes.load(Ordering::SeqCst), 0);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that output writability for an unsuperseded partial frame only
/// flushes retained bytes and does not ask the actor to compose another frame.
///
/// Slow foreground clients can leave encoded bytes pending after the latest
/// rendered frame. When no render invalidation is queued, the service should
/// treat writable output as flush readiness rather than as a redraw trigger.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_flushes_idle_pending_output_without_redraw() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let pending_output_bytes = StdArc::new(AtomicUsize::new(0));
    let flushes = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_pending_output_bytes = pending_output_bytes.clone();
        let service_flushes = flushes.clone();
        let service_task = tokio::spawn(async move {
            let mut io = SupersedablePendingOutputIo::new(
                service_write_count,
                service_write_notify,
                service_pending_output_bytes,
                service_flushes,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        pending_output_bytes.store(1024, Ordering::SeqCst);
        tokio::task::yield_now().await;

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 1);
        assert_eq!(report.loop_report.partial_writes, 1);
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        assert_eq!(pending_output_bytes.load(Ordering::SeqCst), 1024);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.render_client_frame_requests, 1);
    assert!(exit.commands_processed >= 5);
}

/// Verifies that a closed foreground terminal output endpoint is treated as a
/// normal hangup instead of bubbling a `BrokenPipe` I/O error to the top-level
/// CLI error handler during clean primary shutdown or terminal teardown.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_treats_broken_pipe_as_output_hangup() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: vec![std::io::ErrorKind::BrokenPipe],
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9003),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 1);
        assert_eq!(report.loop_report.output_hangups, 1);
        assert_eq!(report.loop_report.output_frames, 0);
        assert!(io.written_batches.is_empty());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that the long-lived attached-terminal service treats an observed
/// terminal size change as authoritative for the primary client. This covers
/// the runtime path used by foreground sessions after a hosting terminal resize:
/// the client loop observes the new size, updates session geometry through the
/// actor, and subsequent rendering uses the resized authoritative dimensions.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_propagates_primary_terminal_resize() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![Some(Size::new(100, 30).unwrap())],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 1 },
            |_| Ok(None),
        )
        .await
        .unwrap();
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(100, 30).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(view.authoritative_size, Size::new(100, 30).unwrap());
        assert_eq!(view.client_size, Size::new(100, 30).unwrap());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that a rapid sequence of foreground terminal-size changes
/// reschedules resize debounce work to the newest generation. Slow remote
/// clients can deliver resize signals close together, so the service should
/// cancel older debounce timers instead of letting each intermediate size force
/// a separate delayed full repaint.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_coalesces_resize_storm_timers() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new(), Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![
            Some(Size::new(100, 30).unwrap()),
            Some(Size::new(120, 35).unwrap()),
            Some(Size::new(130, 40).unwrap()),
        ],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig {
                    resize_debounce_ms: 25,
                    ..TerminalClientLoopConfig::default()
                },
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 3 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.terminal_resizes, 3);
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let resize_timer_effects = timer_effects
            .into_iter()
            .filter(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                | RuntimeSideEffect::CancelTimer { key } => {
                    key.kind == RuntimeTimerKind::ResizeDebounce
                }
                _ => false,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            resize_timer_effects,
            vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        1,
                    ),
                    delay_ms: 200,
                },
                RuntimeSideEffect::CancelTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        1,
                    ),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        2,
                    ),
                    delay_ms: 200,
                },
                RuntimeSideEffect::CancelTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        2,
                    ),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        3,
                    ),
                    delay_ms: 200,
                },
            ]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 13);
}

/// Verifies that resize handling immediately invalidates retained foreground
/// output state and also queues a resize debounce timer. The immediate
/// invalidation gives the resized terminal a full refresh right away, while the
/// actor-owned timer still coalesces follow-up resize work without a blind
/// compatibility client-loop deadline.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_schedules_resize_debounce_timer() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nresize_debounce_ms = 1\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new(), Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![Some(Size::new(100, 30).unwrap()), None],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 1 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(io.invalidated_output_frames, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        let resize_timer = timers
            .iter()
            .find(|effect| {
                matches!(
                    effect,
                    RuntimeSideEffect::ScheduleTimer { key, .. }
                        if key.kind == RuntimeTimerKind::ResizeDebounce
                )
            })
            .unwrap_or_else(|| panic!("expected resize debounce timer: {timers:?}"));
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = resize_timer else {
            panic!("expected resize debounce timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ResizeDebounce);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(*delay_ms, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 6);
}

/// Verifies async attached terminal service can be supervised by name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_can_be_supervised_by_name() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let actor_handle = handle.clone();
    let io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };
    let service = build_async_attached_terminal_client_service(
        "attached-terminal-primary",
        handle,
        io,
        AsyncAttachedTerminalLoopRequest {
            role: ClientViewRole::Observer,
            client_id: ClientId::new('c', 9004),
            primary_client_id: None,
            client_size: Size::new(80, 24).unwrap(),
            terminal_config: TerminalClientLoopConfig::default(),
            loop_config: AttachedTerminalClientLoopConfig {
                max_iterations: 1,
                max_input_bytes: 64,
            },
        },
        AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
        |_| Ok(None),
    )
    .unwrap();

    let actor_task = tokio::spawn(actor.run());
    let report = supervise_async_runtime_services(vec![service], std::future::pending())
        .await
        .unwrap();
    assert!(!report.shutdown_requested);
    assert_eq!(
        report.services,
        vec![AsyncRuntimeServiceReport {
            name: "attached-terminal-primary".to_string(),
            exit: AsyncRuntimeServiceExit::completed(2),
        }]
    );
    assert_eq!(
        actor_handle.shutdown().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    let exit = actor_task.await.unwrap();
    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal service exits cleanly after primary detach.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_exits_cleanly_after_primary_detach() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo::default();

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
            |_| Ok(None),
        )
        .await
        .unwrap();
        assert_eq!(report.batches, 0);
        assert!(report.stopped_by_lifecycle);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Detached);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
}
