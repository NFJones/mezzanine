//! Async-runtime tests owned by event behavior.

use super::super::*;

/// Verifies async event flush writes notifications and advances cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_flush_writes_notifications_and_advances_cursor() {
    use crate::control::decode_control_frame;
    use crate::protocol::event::EventAudience;
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
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
    use crate::protocol::event::EventAudience;
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
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
    use crate::protocol::event::EventAudience;
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
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
    use crate::protocol::event::EventAudience;
    use tokio::net::UnixStream;

    let (handle, _actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
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
    use crate::protocol::event::EventAudience;
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

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
