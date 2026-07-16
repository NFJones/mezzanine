//! Async-runtime tests owned by runtime behavior.

use super::super::*;

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
        timeout(Duration::from_secs(1), async {
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
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
            std::future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(
            daemon_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let ((), (), report, _exit) = tokio::join!(control_client, message_client, daemon, actor.run());
    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(services.len(), 7);
    assert_eq!(services[0].name, "agent-provider");
    assert_eq!(services[0].exit.work_units, 0);
    assert_eq!(services[1].name, "control");
    assert_eq!(services[1].exit.work_units, 1);
    assert_eq!(services[2].name, "hook");
    assert_eq!(services[2].exit.work_units, 0);
    assert_eq!(services[3].name, "message");
    assert_eq!(services[3].exit.work_units, 1);
    assert_eq!(services[4].name, "pane-process-supervisor");
    assert_eq!(services[4].exit.work_units, 0);
    assert_eq!(services[5].name, "persistence");
    assert_eq!(services[5].exit.work_units, 0);
    assert_eq!(services[6].name, "timer");
    assert_eq!(services[6].exit.work_units, 0);

    let _ = std::fs::remove_file(&control_path);
    let _ = std::fs::remove_file(&message_path);
}
