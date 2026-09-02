//! Async-runtime tests owned by bounded host clipboard behavior.

use super::super::*;

static IROH_COPY_INTEGRATION_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());

static IROH_COPY_FALLBACK_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record_iroh_copy_integration_write(content: &str) -> bool {
    IROH_COPY_INTEGRATION_WRITES
        .lock()
        .unwrap()
        .push(content.to_string());
    true
}

fn record_iroh_copy_fallback_write(content: &str) -> bool {
    IROH_COPY_FALLBACK_WRITES
        .lock()
        .unwrap()
        .push(content.to_string());
    true
}

fn empty_iroh_copy_integration_read() -> Option<String> {
    None
}

/// Verifies one real attached copy-mode selection preserves the internal buffer
/// and routes the copied text to the initiating primary while an active client
/// clipboard route suppresses server-host clipboard command effects.
///
/// The second primary has no registered v2 route, so the selected bytes must
/// remain isolated to the initiating primary even though both clients are live.
#[tokio::test(flavor = "current_thread")]
async fn iroh_copy_mode_selection_suppresses_host_copy_and_routes_exactly() {
    IROH_COPY_INTEGRATION_WRITES.lock().unwrap().clear();
    let mut service = test_service();
    service.set_host_clipboard_for_tests(HostClipboard::new(
        record_iroh_copy_integration_write,
        empty_iroh_copy_integration_read,
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let other = service
        .attach_primary("other", true, Size::new(80, 24).unwrap(), 2)
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: Vec::new(),
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let mut screen = mez_terminal::TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap();
    screen.feed(b"client clipboard integration");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .ensure_active_copy_mode("%1")
        .unwrap()
        .select_range(
            mez_mux::copy::CopyPosition { line: 0, column: 0 },
            mez_mux::copy::CopyPosition {
                line: 0,
                column: 28,
            },
        )
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let route = handle
            .register_client_clipboard_route(primary.clone())
            .await
            .unwrap();
        let generation = route.generation();
        handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleCopyMode(
                        mez_mux::copy::CopyModeKeyAction::BeginSelection,
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

        let write = handle
            .take_client_clipboard_write(primary.clone(), generation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(write.content(), "client clipboard integration");
        assert!(
            handle
                .take_client_clipboard_write(other, generation)
                .await
                .unwrap()
                .is_none()
        );
        let buffers = handle
            .execute_terminal_command(primary.clone(), "list-buffers".to_string())
            .await
            .unwrap();
        assert!(
            buffers.contains("client clipboard integration"),
            "{buffers}"
        );
        assert!(
            IROH_COPY_INTEGRATION_WRITES.lock().unwrap().is_empty(),
            "an active client clipboard route must suppress server-host copy commands"
        );
        assert!(route.close().await.unwrap());
        handle.shutdown().await.unwrap();
    };

    let ((), _) = tokio::join!(client, actor.run());
}

/// Verifies a copy-mode selection falls back to server-host clipboard commands
/// when the initiating primary owns no client clipboard route.
#[tokio::test(flavor = "current_thread")]
async fn iroh_copy_mode_selection_without_client_route_preserves_host_copy() {
    IROH_COPY_FALLBACK_WRITES.lock().unwrap().clear();
    let mut service = test_service();
    service.set_host_clipboard_for_tests(HostClipboard::new(
        record_iroh_copy_fallback_write,
        empty_iroh_copy_integration_read,
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: Vec::new(),
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let mut screen = mez_terminal::TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap();
    screen.feed(b"host clipboard fallback");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .ensure_active_copy_mode("%1")
        .unwrap()
        .select_range(
            mez_mux::copy::CopyPosition { line: 0, column: 0 },
            mez_mux::copy::CopyPosition {
                line: 0,
                column: 23,
            },
        )
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleCopyMode(
                        mez_mux::copy::CopyModeKeyAction::BeginSelection,
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

        assert_eq!(
            IROH_COPY_FALLBACK_WRITES.lock().unwrap().as_slice(),
            ["host clipboard fallback"]
        );
        handle.shutdown().await.unwrap();
    };

    let ((), _) = tokio::join!(client, actor.run());
}

/// Verifies exact-client clipboard routes coalesce unsent writes, preserve a
/// monotonic sequence, reject unrelated clients and oversize payloads, and
/// never expose clipboard contents through `Debug` formatting.
#[tokio::test(flavor = "current_thread")]
async fn iroh_client_clipboard_routes_are_bounded_private_and_exact() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let other = service
        .attach_primary("other", true, Size::new(80, 24).unwrap(), 2)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let route = handle
            .register_client_clipboard_route(primary.clone())
            .await
            .unwrap();
        let generation = route.generation();
        assert!(
            handle
                .enqueue_client_clipboard_write(primary.clone(), "superseded secret".to_string())
                .await
                .unwrap()
        );
        assert!(
            handle
                .enqueue_client_clipboard_write(primary.clone(), "newest secret".to_string())
                .await
                .unwrap()
        );
        assert!(
            !handle
                .enqueue_client_clipboard_write(other, "wrong client".to_string())
                .await
                .unwrap()
        );
        assert!(
            !handle
                .enqueue_client_clipboard_write(
                    primary.clone(),
                    "x".repeat(crate::runtime::MAX_CLIENT_CLIPBOARD_BYTES + 1),
                )
                .await
                .unwrap()
        );

        let write = handle
            .take_client_clipboard_write(primary.clone(), generation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(write.sequence(), 2);
        assert_eq!(write.content(), "newest secret");
        assert_eq!(write.byte_len(), "newest secret".len());
        let debug = format!("{write:?}");
        assert!(debug.contains("byte_len"));
        assert!(!debug.contains("secret"));
        assert!(
            handle
                .take_client_clipboard_write(primary.clone(), generation)
                .await
                .unwrap()
                .is_none()
        );
        assert!(route.close().await.unwrap());
        assert!(
            !handle
                .enqueue_client_clipboard_write(primary, "after close".to_string())
                .await
                .unwrap()
        );
        handle.shutdown().await.unwrap();
    };

    let ((), _) = tokio::join!(client, actor.run());
}

/// Aborting an event task drops its route lease and synchronously queues
/// generation-fenced actor cleanup, so later clipboard effects are rejected.
#[tokio::test(flavor = "current_thread")]
async fn aborted_iroh_event_owner_removes_clipboard_route() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let task_handle = handle.clone();
        let task_primary = primary.clone();
        let (registered_tx, registered_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let route = task_handle
                .register_client_clipboard_route(task_primary)
                .await
                .unwrap();
            registered_tx.send(route.generation()).unwrap();
            std::future::pending::<()>().await;
            drop(route);
        });
        let _generation = registered_rx.await.unwrap();

        owner.abort();
        assert!(owner.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !handle
                    .enqueue_client_clipboard_write(primary.clone(), "after abort".to_string())
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborted event owner should remove its clipboard route");
        handle.shutdown().await.unwrap();
    };

    let ((), _) = tokio::join!(client, actor.run());
}

/// Cleanup from an older event-stream generation cannot remove the route
/// installed by a replacement stream for the same exact client.
#[tokio::test(flavor = "current_thread")]
async fn stale_iroh_event_cleanup_preserves_replacement_clipboard_route() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let stale = handle
            .register_client_clipboard_route(primary.clone())
            .await
            .unwrap();
        let stale_generation = stale.generation();
        let replacement = handle
            .register_client_clipboard_route(primary.clone())
            .await
            .unwrap();
        let replacement_generation = replacement.generation();
        assert_ne!(stale_generation, replacement_generation);

        drop(stale);
        assert!(
            handle
                .enqueue_client_clipboard_write(primary.clone(), "replacement".to_string())
                .await
                .unwrap()
        );
        assert!(
            handle
                .take_client_clipboard_write(primary.clone(), stale_generation)
                .await
                .unwrap()
                .is_none()
        );
        let write = handle
            .take_client_clipboard_write(primary.clone(), replacement_generation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(write.content(), "replacement");
        assert!(replacement.close().await.unwrap());
        handle.shutdown().await.unwrap();
    };

    let ((), _) = tokio::join!(client, actor.run());
}

/// Verifies a slow host clipboard helper runs outside serialized actor
/// ownership and reports one typed completion after its bounded worker exits.
///
/// A lifecycle heartbeat must complete while the helper is still sleeping. If
/// clipboard acquisition moves back into input application, this heartbeat
/// waits behind the subprocess and the regression fails.
#[tokio::test(flavor = "current_thread")]
async fn async_host_clipboard_worker_does_not_block_actor_heartbeats() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let clipboard = HostClipboard::commands(
        Vec::new(),
        vec![HostClipboardCommand::new(
            "sh",
            vec!["-c".to_string(), "sleep 0.2; printf clipboard".to_string()],
        )],
    )
    .with_read_limits(Duration::from_secs(1), 1024);

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ReadHostClipboard {
                generation: 1,
                plan: clipboard.read_plan(),
            }])
            .await
            .unwrap();
        let worker_handle = handle.clone();
        let worker = async move {
            run_async_host_clipboard_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 2,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                },
                |polls, _| polls >= 2,
            )
            .await
            .unwrap()
        };
        let heartbeat = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tokio::time::timeout(Duration::from_millis(50), handle.lifecycle_state())
                .await
                .expect("actor heartbeat should not wait for clipboard acquisition")
                .unwrap()
        };

        let (report, lifecycle) = tokio::join!(worker, heartbeat);

        assert_eq!(lifecycle, RuntimeLifecycleState::Running);
        assert_eq!(report.drained, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 0);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 5);
}
