//! Async-runtime tests owned by runtime behavior.

use super::super::*;

/// Allows real PTY startup and daemon scheduling to settle under parallel test load.
const ASYNC_DAEMON_PTY_RENDER_SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Verifies that supervised async pane workers feed PTY output into runtime
/// terminal screens even when the daemon has no compatibility tick service.
/// Attached-client rendering depends on pane-driver events in the Tokio daemon
/// path.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_daemon_pane_worker_feeds_pty_output_into_rendered_view() {
    use tokio::net::UnixListener;
    use tokio::time::timeout;

    let path =
        std::env::temp_dir().join(format!("mez-async-daemon-tick-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("sh -c 'printf async-daemon-tick; sleep 1'"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let services = build_async_runtime_daemon_services(
        handle.clone(),
        AsyncRuntimeDaemonListeners::control_only(listener),
        AsyncRuntimeDaemonConfig {
            control: AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                .unwrap(),
            ..AsyncRuntimeDaemonConfig::default()
        },
    )
    .unwrap();
    let poll_handle = handle.clone();
    let cancellation = async move {
        timeout(ASYNC_DAEMON_PTY_RENDER_SETUP_TIMEOUT, async {
            loop {
                let view = poll_handle
                    .render_client_view(
                        ClientViewRole::Primary,
                        Size::new(80, 24).unwrap(),
                        TerminalClientLoopConfig::default(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                if view.lines.join("\n").contains("async-daemon-tick") {
                    break;
                }
                poll_handle.wait_for_event_delivery().await;
            }
        })
        .await
        .unwrap();
    };
    let shutdown_handle = handle.clone();
    let daemon = async move {
        let report = supervise_async_runtime_services(services, cancellation)
            .await
            .unwrap();
        let _ = shutdown_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(daemon, actor.run());

    assert!(report.shutdown_requested);
    assert!(
        report
            .services
            .iter()
            .any(|service| service.name == "pane-process-supervisor")
    );
    assert!(!report.services.iter().any(|service| service.name == "tick"));
    exit.service.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_file(&path);
}

/// Verifies async runtime daemon supervises named control and message listeners.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_daemon_supervises_named_control_and_message_listeners() {
    use crate::control::{decode_control_frame, encode_control_body};
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let control_path = std::env::temp_dir().join(format!(
        "mez-async-daemon-control-{}.sock",
        std::process::id()
    ));
    let message_path = std::env::temp_dir().join(format!(
        "mez-async-daemon-message-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&control_path);
    let _ = std::fs::remove_file(&message_path);
    let control_listener = UnixListener::bind(&control_path).unwrap();
    let message_listener = UnixListener::bind(&message_path).unwrap();

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);

    let control_client = async {
        let mut stream = UnixStream::connect(&control_path).await.unwrap();
        stream.write_all(&initialize).await.unwrap();
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));
    };
    let message_client = async {
        let mut stream = UnixStream::connect(&message_path).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_mmp_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));
    };
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let clients = async move {
        let ((), ()) = tokio::join!(control_client, message_client);
        cancel_tx.send(()).unwrap();
    };
    let daemon_handle = handle.clone();
    let daemon = async move {
        let report = run_async_runtime_daemon(
            daemon_handle.clone(),
            AsyncRuntimeDaemonListeners {
                control: Some(control_listener),
                message: Some(message_listener),
                event: None,
            },
            AsyncRuntimeDaemonConfig {
                control: AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                    .unwrap(),
                message_max_content_length: 4096,
                max_control_connections: 1,
                max_message_connections: 1,
                ..AsyncRuntimeDaemonConfig::default()
            },
            async {
                let _ = cancel_rx.await;
            },
        )
        .await
        .unwrap();
        assert_eq!(
            daemon_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let ((), report, _exit) = tokio::join!(clients, daemon, actor.run());
    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(report.shutdown_requested);
    assert_eq!(services.len(), 9);
    assert_eq!(services[0].name, "agent-provider");
    assert_eq!(services[0].exit.work_units, 0);
    assert_eq!(services[1].name, "control");
    assert_eq!(services[1].exit.work_units, 0);
    assert_eq!(services[2].name, "hook");
    assert_eq!(services[2].exit.work_units, 0);
    assert_eq!(services[3].name, "host-clipboard");
    assert_eq!(services[3].exit.work_units, 0);
    assert_eq!(services[4].name, "message");
    assert_eq!(services[4].exit.work_units, 0);
    assert_eq!(services[5].name, "pane-process-supervisor");
    assert_eq!(services[5].exit.work_units, 0);
    assert_eq!(services[6].name, "persistence");
    assert_eq!(services[6].exit.work_units, 0);
    assert_eq!(services[7].name, "status-pill");
    assert_eq!(services[7].exit.work_units, 0);
    assert_eq!(services[8].name, "timer");
    assert_eq!(services[8].exit.work_units, 0);

    let _ = std::fs::remove_file(&control_path);
    let _ = std::fs::remove_file(&message_path);
}

/// Verifies closing the final window through the supervised control listener
/// flushes its complete success response before terminal lifecycle shutdown
/// drains the daemon services.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_daemon_flushes_final_window_close_response() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::time::timeout;

    let control_path = std::env::temp_dir().join(format!(
        "mez-async-daemon-final-window-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&control_path);
    let listener = UnixListener::bind(&control_path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let close = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"close","method":"window/close","params":{"target":{"window_id":"@1"},"force":true,"idempotency_key":"final-window-close"}}"#,
    );

    let client = async {
        let mut stream = UnixStream::connect(&control_path).await.unwrap();
        stream.write_all(&initialize).await.unwrap();
        let mut initialized = vec![0; 4096];
        let read = stream.read(&mut initialized).await.unwrap();
        initialized.truncate(read);
        let (body, _) = decode_control_frame(&initialized, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#), "{body}");

        stream.write_all(&close).await.unwrap();
        let mut closed = vec![0; 4096];
        let read = timeout(Duration::from_secs(1), stream.read(&mut closed))
            .await
            .unwrap()
            .unwrap();
        assert!(read > 0, "final-window close response must precede EOF");
        closed.truncate(read);
        let (body, consumed) = decode_control_frame(&closed, 4096).unwrap();
        assert_eq!(consumed, closed.len());
        assert!(body.contains(r#""id":"close""#), "{body}");
        assert!(body.contains(r#""closed":true"#), "{body}");
        assert!(body.contains(r#""session_empty":true"#), "{body}");
    };
    let daemon_handle = handle.clone();
    let daemon = async move {
        let report = timeout(
            Duration::from_secs(1),
            run_async_runtime_daemon(
                daemon_handle.clone(),
                AsyncRuntimeDaemonListeners::control_only(listener),
                AsyncRuntimeDaemonConfig {
                    control: AsyncRuntimeControlConnectionConfig::new(
                        4096,
                        current_effective_uid(),
                    )
                    .unwrap(),
                    ..AsyncRuntimeDaemonConfig::default()
                },
                std::future::pending(),
            ),
        )
        .await
        .expect("terminal daemon shutdown should remain bounded")
        .unwrap();
        daemon_handle.shutdown().await.unwrap();
        report
    };

    let ((), report, _exit) = tokio::join!(client, daemon, actor.run());
    assert!(
        report
            .services
            .iter()
            .any(|service| service.name == "control" && service.exit.work_units == 1)
    );
    let _ = std::fs::remove_file(&control_path);
}
