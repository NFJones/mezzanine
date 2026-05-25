/// Verifies the control listener can accept an observer while another control
/// connection remains open. Observer attachment uses a long-lived control
/// socket, so the accept loop must dispatch each connection independently or a
/// pending observer request can never be registered for the primary to review.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_registers_observer_while_primary_connection_remains_open() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::oneshot;

    async fn read_control_body(stream: &mut UnixStream) -> String {
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        body
    }

    let path = std::env::temp_dir().join(format!(
        "mez-async-control-listener-{}-{}.sock",
        std::process::id(),
        "observer-concurrent"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
    let primary_initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let observer_initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"observer-init","method":"control/initialize","params":{"client_name":"observer-cli","requested_version":1,"requested_role":"observer","client":{"name":"observer-cli","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let list_observers = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"observer/list","params":{}}"#,
    );
    let (primary_ready_tx, primary_ready_rx) = oneshot::channel();
    let (observer_ready_tx, observer_ready_rx) = oneshot::channel();

    let primary_client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&primary_initialize).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(body.contains(r#""granted_role":"primary""#), "{body}");
        primary_ready_tx.send(()).unwrap();
        observer_ready_rx.await.unwrap();

        stream.write_all(&list_observers).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(body.contains(r#""observers""#), "{body}");
        assert!(body.contains(r#""state":"pending""#), "{body}");
        assert!(body.contains("observer-cli"), "{body}");
    };
    let observer_client = async {
        primary_ready_rx.await.unwrap();
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&observer_initialize).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(
            body.contains(r#""granted_role":"pending_observer""#),
            "{body}"
        );
        observer_ready_tx.send(()).unwrap();
    };
    let server = async {
        let served = serve_async_runtime_control_listener(
            &listener,
            &handle,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), _exit) = tokio::join!(primary_client, observer_client, server, actor.run());
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
    use crate::message::{decode_mmp_frame, encode_mmp_body};
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

    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
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
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_file(&path);
}

/// Verifies async message connection dispatches hello.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_dispatches_hello() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let input = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_mmp_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""type":"welcome""#));
    };
    let server = async {
        let mut connection = MessageConnection::default();
        let served = serve_async_runtime_message_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            4096,
            10,
            100,
        )
        .await
        .unwrap();
        assert_eq!(served, input.len());
        assert!(connection.agent_id.is_some());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert_eq!(exit.commands_processed, 3);
}

/// Verifies async message connection loop preserves agent connection.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_loop_preserves_agent_connection() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);
    let discover = encode_mmp_body(r#"{"protocol":"mmp/1","type":"discover"}"#);

    let client = async {
        client_stream.write_all(&hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = client_stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));

        client_stream.write_all(&discover).await.unwrap();
        let mut second = vec![0; 4096];
        let read = client_stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_mmp_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""type":"discover_result""#));
        assert!(body.contains(r#""role":"default""#));
    };
    let server = async {
        let mut connection = MessageConnection::default();
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |served| 10 + served,
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert!(connection.agent_id.is_some());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies async message listener serves stateful connection until client closes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_serves_stateful_connection_until_client_closes() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-message-listener-{}-{}.sock",
        std::process::id(),
        "stateful"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);
    let discover = encode_mmp_body(r#"{"protocol":"mmp/1","type":"discover"}"#);

    let client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));

        stream.write_all(&discover).await.unwrap();
        let mut second = vec![0; 4096];
        let read = stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_mmp_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""type":"discover_result""#));
    };
    let server = async {
        let served = serve_async_runtime_message_listener(
            &listener,
            &handle,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            10,
            |served, _| served >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}

/// Verifies async message listener can schedule multiple connections.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_can_schedule_multiple_connections() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-message-listener-{}-{}.sock",
        std::process::id(),
        "concurrent"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);

    let client_one_path = path.clone();
    let client_one_hello = hello.clone();
    let client_one = async move {
        let mut stream = UnixStream::connect(&client_one_path).await.unwrap();
        stream.write_all(&client_one_hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));
    };
    let client_two_path = path.clone();
    let client_two = async move {
        let mut stream = UnixStream::connect(&client_two_path).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));
    };
    let server = async {
        let served = serve_async_runtime_message_listener_concurrent(
            &listener,
            &handle,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            10,
            2,
            |served, _| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), exit) = tokio::join!(client_one, client_two, server, actor.run());

    assert!(exit.commands_processed >= 5);
    let _ = std::fs::remove_file(&path);
}

/// Verifies async message connection flushes fanout after response write.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_flushes_fanout_after_response_write() {
    use crate::message::{Envelope, Recipient, decode_mmp_frame, encode_mmp_body};
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio::time::timeout;

    let mut service = test_service();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", Vec::new());
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", Vec::new());
    service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    let message = Envelope {
        protocol: "mmp/1",
        id: "m1".to_string(),
        message_type: "send".to_string(),
        time: "message:test".to_string(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".to_string(),
        payload: "hello".to_string(),
        extension_fields: Vec::new(),
    };
    service
        .message_service_mut()
        .accept_at(&sender.agent_id, message, 10)
        .unwrap();
    let mut connection = MessageConnection {
        agent_id: Some(target.agent_id.clone()),
        delivery_cursor: service
            .message_service()
            .subscription(&target.agent_id)
            .cloned(),
    };
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let input = encode_mmp_body(r#"{"protocol":"mmp/1","type":"heartbeat","id":"hb1"}"#);

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = Vec::new();
        timeout(Duration::from_secs(1), async {
            loop {
                let mut chunk = [0u8; 1024];
                let read = client_stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "message stream closed before fanout delivery");
                output.extend_from_slice(&chunk[..read]);
                let Ok((_, first_len)) = decode_mmp_frame(&output, 4096) else {
                    continue;
                };
                if decode_mmp_frame(&output[first_len..], 4096).is_ok() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let (ack, first_len) = decode_mmp_frame(&output, 4096).unwrap();
        let (deliver, _) = decode_mmp_frame(&output[first_len..], 4096).unwrap();
        assert!(ack.contains(r#""type":"ack""#));
        assert!(deliver.contains(r#""type":"deliver""#));
        assert!(deliver.contains(r#""payload":"hello""#));
    };
    let server = async {
        let served = serve_async_runtime_message_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            4096,
            11,
            100,
        )
        .await
        .unwrap();
        assert_eq!(served, input.len());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());
    let remaining = exit
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, 12, usize::MAX)
        .unwrap();

    assert!(remaining.messages.is_empty());
}

/// Verifies async message connection notification flushes later fanout.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_message_connection_notification_flushes_later_fanout() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;
    use tokio::time::{advance, timeout};

    let mut service = test_service();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", Vec::new());
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", Vec::new());
    let target_cursor = service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap()
        .clone();
    let mut target_connection = MessageConnection {
        agent_id: Some(target.agent_id.clone()),
        delivery_cursor: Some(target_cursor),
    };
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let producer_handle = handle.clone();
    let target_id = target.agent_id.clone();
    let sender_id = sender.agent_id.clone();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let delivered = Arc::new(AtomicBool::new(false));
    let client_delivered = delivered.clone();
    let server_delivered = delivered.clone();

    let client = async {
        let mut output = Vec::new();
        timeout(Duration::from_secs(1), async {
            loop {
                let mut chunk = [0u8; 1024];
                let read = client_stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "message stream closed before fanout delivery");
                output.extend_from_slice(&chunk[..read]);
                if decode_mmp_frame(&output, 4096).is_ok() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let (deliver, consumed) = decode_mmp_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(deliver.contains(r#""type":"deliver""#));
        assert!(deliver.contains(r#""payload":"idle hello""#));
        client_delivered.store(true, Ordering::SeqCst);
    };
    let producer = async move {
        advance(Duration::from_millis(10)).await;
        let mut sender_connection = MessageConnection {
            agent_id: Some(sender_id),
            delivery_cursor: None,
        };
        let recipient_json = format!(r#"{{"agent_id":"{}"}}"#, target_id);
        let send = format!(
            r#"{{"protocol":"mmp/1","type":"send","id":"m-idle","time":"message:client-idle","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"idle hello"}}"#,
            sender_connection.agent_id.as_ref().unwrap(),
            recipient_json
        );
        let result = producer_handle
            .handle_message_input(encode_mmp_body(&send), 4096, sender_connection.clone(), 20)
            .await
            .unwrap();
        sender_connection = result.connection;
        assert!(sender_connection.agent_id.is_some());
        let (ack, _) = decode_mmp_frame(&result.output, 4096).unwrap();
        assert!(ack.contains(r#""status":"accepted""#));
    };
    let server = async {
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut target_connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |served| 20 + served,
            |_served, _state| server_delivered.load(Ordering::SeqCst),
        )
        .await
        .unwrap();
        assert_eq!(served, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), exit) = tokio::join!(client, producer, server, actor.run());
    let remaining = exit
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, 21, usize::MAX)
        .unwrap();

    assert!(remaining.messages.is_empty());
}

/// Verifies that an idle subscribed message connection wakes from the actor
/// lifecycle channel instead of relying on its long fallback poll interval.
/// This protects shutdown responsiveness for agent message sockets when no
/// message fanout is pending and the peer keeps the Unix stream open.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_message_connection_exits_on_lifecycle_change_without_idle_poll() {
    use tokio::net::UnixStream;
    use tokio::sync::oneshot;

    let mut service = test_service();
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", Vec::new());
    let target_cursor = service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap()
        .clone();
    let mut target_connection = MessageConnection {
        agent_id: Some(target.agent_id.clone()),
        delivery_cursor: Some(target_cursor),
    };
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let trigger_handle = handle.clone();
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let (release_client, hold_client) = oneshot::channel::<()>();

    let client_guard = async move {
        let _client_stream = client_stream;
        let _ = hold_client.await;
    };
    let trigger_shutdown = async move {
        tokio::task::yield_now().await;
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test lifecycle wake".to_string(),
            force: true,
            failed: false,
        }));
        let report = trigger_handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.applied, 1);
        let metrics = trigger_handle.metrics().await.unwrap();
        assert_eq!(metrics.lifecycle_state_notifications, 1);
        assert_eq!(
            trigger_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
    };
    let server = async {
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut target_connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |served| 20 + served,
            |_served, state| {
                matches!(
                    state,
                    RuntimeLifecycleState::Stopping
                        | RuntimeLifecycleState::Killed
                        | RuntimeLifecycleState::Failed
                )
            },
        )
        .await
        .unwrap();
        assert_eq!(served, 0);
        let _ = release_client.send(());
    };

    let ((), (), (), exit) = tokio::join!(client_guard, trigger_shutdown, server, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Killed
    );
}

/// Verifies async event flush writes notifications and advances cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_flush_writes_notifications_and_advances_cursor() {
    use crate::control::decode_control_frame;
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach("events-primary", EventAudience::Primary, true, 0)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();

    let client = async {
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_control_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""method":"event/client_attached""#));
        assert!(body.contains(r#""event_id":1"#));
    };
    let server = async {
        let delivered = flush_async_runtime_event_wakeups_to_stream(
            &mut server_stream,
            &handle,
            &mut connections,
            10,
        )
        .await
        .unwrap();
        assert_eq!(delivered, 1);
        assert_eq!(
            flush_async_runtime_event_wakeups_to_stream(
                &mut server_stream,
                &handle,
                &mut connections,
                10,
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert_eq!(exit.commands_processed, 3);
}

/// Verifies async event connection serves until shutdown predicate.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_connection_serves_until_shutdown_predicate() {
    use crate::control::decode_control_frame;
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach("events-primary", EventAudience::Primary, true, 0)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();

    let client = async {
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""method":"event/client_attached""#));
    };
    let server = async {
        let served = serve_async_runtime_event_connection(
            &mut server_stream,
            &handle,
            &mut connections,
            AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid()).unwrap(),
            |delivered, _state| delivered >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert!(exit.commands_processed >= 2);
}

/// Verifies async event connection notification flushes later events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_event_connection_notification_flushes_later_events() {
    use crate::control::{ControlConnectionState, decode_control_frame, encode_control_body};
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;
    use tokio::time::{advance, timeout};

    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let last_event_id = service.event_log().unwrap().latest_event_id();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach(
            "events-primary",
            EventAudience::Primary,
            true,
            last_event_id,
        )
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let producer_handle = handle.clone();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();

    let client = async {
        let mut output = Vec::new();
        timeout(Duration::from_secs(1), async {
            loop {
                let mut chunk = [0u8; 1024];
                let read = client_stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "event stream closed before later event delivery");
                output.extend_from_slice(&chunk[..read]);
                if decode_control_frame(&output, 4096).is_ok() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let mut decoded = Vec::new();
        let mut offset = 0usize;
        while offset < output.len() {
            match decode_control_frame(&output[offset..], 4096) {
                Ok((body, consumed)) => {
                    decoded.push(body);
                    offset = offset.saturating_add(consumed);
                }
                Err(error) if !decoded.is_empty() => {
                    assert!(
                        matches!(error.kind(), crate::error::MezErrorKind::InvalidArgs),
                        "unexpected trailing event stream decode error: {error}"
                    );
                    break;
                }
                Err(error) => panic!("event stream did not contain a complete frame: {error}"),
            }
        }
        assert!(
            decoded
                .iter()
                .any(|body| body.contains(r#""method":"event/"#)),
            "{decoded:?}"
        );
        assert!(
            decoded.iter().all(|body| !body.contains(r#""event_id":1"#)),
            "{decoded:?}"
        );
    };
    let producer = async move {
        advance(Duration::from_millis(10)).await;
        let input = encode_control_body(
            r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"events","shell_command":"true","idempotency_key":"event-window"}}"#,
        );
        let control_connection = ControlConnectionState::trusted_existing_client(primary);
        let result = producer_handle
            .handle_control_input_for_connection(input, 4096, control_connection)
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""window":{"#), "{body}");
    };
    let server = async {
        let delivered = serve_async_runtime_event_connection(
            &mut server_stream,
            &handle,
            &mut connections,
            AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid()).unwrap(),
            |delivered, _state| delivered >= 1,
        )
        .await
        .unwrap();
        assert!(delivered >= 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), exit) = tokio::join!(client, producer, server, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies async event connection rejects wrong unix peer owner.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_connection_rejects_wrong_unix_peer_owner() {
    use crate::event::EventAudience;
    use tokio::net::UnixStream;

    let (handle, _actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log()).build().unwrap();
    let (_client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach("events-primary", EventAudience::Primary, true, 0)
        .unwrap();

    let error = serve_async_runtime_event_connection(
        &mut server_stream,
        &handle,
        &mut connections,
        AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid().saturating_add(1))
            .unwrap(),
        |_delivered, _state| true,
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies async event listener accepts and streams visible events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_listener_accepts_and_streams_visible_events() {
    use crate::control::decode_control_frame;
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-event-listener-{}-{}.sock",
        std::process::id(),
        "primary"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

    let client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""method":"event/client_attached""#));
        assert!(body.contains(r#""event_id":1"#));
    };
    let server = async {
        let served = serve_async_runtime_event_listener(
            &listener,
            &handle,
            AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid()).unwrap(),
            |index| Ok((format!("events-{index}"), EventAudience::Primary, 0)),
            |accepted, delivered, _state| accepted >= 1 || delivered >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}
