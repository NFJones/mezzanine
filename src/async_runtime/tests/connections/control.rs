//! Async-runtime tests owned by control behavior.

use super::super::*;

/// Verifies async control connection authorizes and round trips control frame.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_connection_authorizes_and_round_trips_control_frame() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_control_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""control/initialize""#));
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_async_runtime_control_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(served, input.len());
        assert!(connection.initialized());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert_eq!(exit.commands_processed, 2);
}

/// Verifies async control connection loop preserves initialized caller.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_connection_loop_preserves_initialized_caller() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let get_session =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);

    let client = async {
        client_stream.write_all(&initialize).await.unwrap();
        let mut first = vec![0; 4096];
        let read = client_stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_control_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));

        client_stream.write_all(&get_session).await.unwrap();
        let mut second = vec![0; 4096];
        let read = client_stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_control_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""session_id""#));
        assert!(body.contains(r#""windows""#));
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_async_runtime_control_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert!(connection.initialized());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert!(exit.commands_processed >= 3);
}

/// Verifies async control listener serves stateful connection until client closes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_serves_stateful_connection_until_client_closes() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-control-listener-{}-{}.sock",
        std::process::id(),
        "stateful"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let get_session =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);

    let client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&initialize).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_control_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));

        stream.write_all(&get_session).await.unwrap();
        let mut second = vec![0; 4096];
        let read = stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_control_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""session_id""#));
    };
    let server = async {
        let served = serve_async_runtime_control_listener(
            &listener,
            &handle,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 1,
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
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
