//! Async-runtime tests owned by bounded host clipboard behavior.

use super::super::*;

static IROH_COPY_INTEGRATION_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record_iroh_copy_integration_write(content: &str) -> bool {
    IROH_COPY_INTEGRATION_WRITES
        .lock()
        .unwrap()
        .push(content.to_string());
    true
}

fn empty_iroh_copy_integration_read() -> Option<String> {
    None
}

/// Verifies one real attached copy-mode selection preserves the internal and
/// server-host effects while adding exactly one initiator-scoped Iroh effect.
///
/// The second primary has no registered v2 route, so the selected bytes must
/// remain isolated to the initiating primary even though both clients are live.
#[tokio::test(flavor = "current_thread")]
async fn iroh_copy_mode_selection_preserves_host_and_buffer_and_routes_exactly() {
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
        handle
            .register_client_clipboard_route(primary.clone())
            .await
            .unwrap();
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
            .take_client_clipboard_write(primary.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(write.content(), "client clipboard integration");
        assert!(
            handle
                .take_client_clipboard_write(other)
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
        assert_eq!(
            IROH_COPY_INTEGRATION_WRITES.lock().unwrap().as_slice(),
            ["client clipboard integration"]
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
        handle
            .register_client_clipboard_route(primary.clone())
            .await
            .unwrap();
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
            .take_client_clipboard_write(primary.clone())
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
                .take_client_clipboard_write(primary.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            handle
                .unregister_client_clipboard_route(primary.clone())
                .await
                .unwrap()
        );
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
