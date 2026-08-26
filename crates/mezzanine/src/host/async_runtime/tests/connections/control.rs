//! Async-runtime tests owned by control behavior.

use super::super::*;
use crate::host::async_runtime::serve_authenticated_async_runtime_control_connection_loop_with_snapshots;

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
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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

/// Verifies malformed input affects only its connection and the listener still
/// accepts a valid client for the same live session afterward.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_isolates_malformed_connection_input() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-control-listener-{}-reap.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let client = async {
        let mut malformed = UnixStream::connect(&path).await.unwrap();
        malformed
            .write_all(b"Content-Length: invalid\r\n\r\n")
            .await
            .unwrap();
        malformed.shutdown().await.unwrap();
        drop(malformed);

        let mut valid = UnixStream::connect(&path).await.unwrap();
        valid.write_all(&initialize).await.unwrap();
        let mut output = vec![0; 4096];
        let read = valid.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#), "{body}");
    };
    let server = async {
        let served = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            serve_async_runtime_control_listener(
                &listener,
                &handle,
                AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
                |served, _| served >= 2,
            ),
        )
        .await
        .expect("listener should isolate malformed input and serve the valid client")
        .unwrap();
        assert_eq!(served, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}

/// Verifies the control listener can attach an observer while another control
/// connection remains open. The accept loop must dispatch each long-lived
/// connection independently.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_attaches_observer_while_primary_connection_remains_open() {
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

    let path = std::path::PathBuf::from("/tmp")
        .join(format!("mez-ctl-{}-observer.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let primary_initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let observer_initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"observer-init","method":"control/initialize","params":{"client_name":"observer-cli","requested_version":2,"requested_role":"observer","client":{"name":"observer-cli","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let list_clients =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"list","method":"client/list","params":{}}"#);
    let (primary_ready_tx, primary_ready_rx) = oneshot::channel();
    let (observer_ready_tx, observer_ready_rx) = oneshot::channel();
    let (observer_listed_tx, observer_listed_rx) = oneshot::channel();

    let primary_client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&primary_initialize).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(body.contains(r#""granted_role":"primary""#), "{body}");
        primary_ready_tx.send(()).unwrap();
        observer_ready_rx.await.unwrap();

        stream.write_all(&list_clients).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(body.contains(r#""clients""#), "{body}");
        assert!(body.contains(r#""role":"observer""#), "{body}");
        assert!(body.contains(r#""state":"attached""#), "{body}");
        assert!(body.contains("observer-cli"), "{body}");
        observer_listed_tx.send(()).unwrap();
    };
    let observer_client = async {
        primary_ready_rx.await.unwrap();
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&observer_initialize).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(body.contains(r#""granted_role":"observer""#), "{body}");
        observer_ready_tx.send(()).unwrap();
        observer_listed_rx.await.unwrap();
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

/// Verifies two same-named Unix control connections receive independent v2
/// primary identities and closing one connection does not detach the other.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_keeps_same_named_primaries_independent() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::oneshot;

    async fn read_control_body(stream: &mut UnixStream) -> String {
        let mut output = vec![0; 16 * 1024];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        decode_control_frame(&output, 16 * 1024).unwrap().0
    }

    let path = std::path::PathBuf::from("/tmp")
        .join(format!("mez-ctl-{}-two-primary.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let initialize = |id: &str| {
        encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"{id}","method":"control/initialize","params":{{"client_name":"same-name","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{{"name":"same-name","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"peer_credentials"}}}}}}"#
        ))
    };
    let (first_initialized_tx, first_initialized_rx) = oneshot::channel();
    let (second_initialized_tx, second_initialized_rx) = oneshot::channel();
    let (first_closed_tx, first_closed_rx) = oneshot::channel();

    let first_client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&initialize("first-init")).await.unwrap();
        let body = read_control_body(&mut stream).await;
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let client_id = value["result"]["client"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        first_initialized_tx.send(client_id).unwrap();
        second_initialized_rx.await.unwrap();
        stream.shutdown().await.unwrap();
        drop(stream);
        first_closed_tx.send(()).unwrap();
    };
    let second_client = async {
        let first_client_id = first_initialized_rx.await.unwrap();
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&initialize("second-init")).await.unwrap();
        let body = read_control_body(&mut stream).await;
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let second_client_id = value["result"]["client"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(first_client_id, second_client_id);
        assert_eq!(value["result"]["session"]["attached_primary_count"], 2);
        second_initialized_tx.send(()).unwrap();
        first_closed_rx.await.unwrap();

        let mut attached_primary_count = 2;
        for request_id in 0..20 {
            let request = format!(
                r#"{{"jsonrpc":"2.0","id":"state-{request_id}","method":"session/get","params":{{}}}}"#
            );
            stream
                .write_all(&encode_control_body(&request))
                .await
                .unwrap();
            let body = read_control_body(&mut stream).await;
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            attached_primary_count = value["result"]["session"]["attached_primary_count"]
                .as_u64()
                .unwrap();
            if attached_primary_count == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(attached_primary_count, 1);
        stream.shutdown().await.unwrap();
    };
    let server = async {
        let served = serve_async_runtime_control_listener(
            &listener,
            &handle,
            AsyncRuntimeControlConnectionConfig::new(16 * 1024, current_effective_uid()).unwrap(),
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
    };

    let ((), (), (), _exit) = tokio::join!(first_client, second_client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}

/// Verifies the shared control loop accepts a non-Unix async byte stream.
///
/// A Tokio duplex stream has no raw descriptor or peer credentials, so this
/// round trip proves framing and dispatch consume the explicit authenticated
/// peer rather than reaching back into a Unix socket.
#[tokio::test(flavor = "current_thread")]
async fn authenticated_control_loop_round_trips_over_duplex_stream() {
    use crate::control::{AuthenticatedPeer, decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = tokio::io::duplex(8192);
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let expected_peer = AuthenticatedPeer::unix_user(current_effective_uid());

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_control_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut server_stream,
            expected_peer.clone(),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            None,
            |served, _state| served >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(connection.authenticated_peer(), Some(&expected_peer));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());
    assert_eq!(exit.commands_processed, 2);
}

/// An authenticated remote peer that sends no complete frame must be reclaimed
/// by the opt-in application-idle deadline without affecting Unix defaults.
#[tokio::test(flavor = "current_thread")]
async fn authenticated_control_loop_times_out_silent_remote_peer() {
    use crate::control::AuthenticatedPeer;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (_client_stream, mut server_stream) = tokio::io::duplex(1024);
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let error = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut server_stream,
            AuthenticatedPeer::iroh_endpoint("silent-peer"),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                .unwrap()
                .with_application_idle_timeout(Duration::from_millis(50)),
            None,
            |_, _| false,
        )
        .await
        .unwrap_err();
        assert!(
            error.message().contains("application idle timeout"),
            "{error:?}"
        );
        assert!(
            error.message().contains("waiting for a control frame"),
            "{error:?}"
        );
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), _exit) = tokio::join!(server, actor.run());
}

/// Fragmented input remains valid when the complete frame arrives within one
/// idle interval; partial bytes do not get mistaken for a terminal timeout.
#[tokio::test(flavor = "current_thread")]
async fn authenticated_control_loop_accepts_fragmented_frame_within_idle_deadline() {
    use crate::control::{AuthenticatedPeer, decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = tokio::io::duplex(8192);
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let split_at = input.len() / 2;
    let client = async {
        client_stream.write_all(&input[..split_at]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        client_stream.write_all(&input[split_at..]).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut server_stream,
            AuthenticatedPeer::unix_user(current_effective_uid()),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                .unwrap()
                .with_application_idle_timeout(Duration::from_millis(100)),
            None,
            |served, _| served >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
}

/// Each fully flushed request/response cycle resets the idle interval, so
/// healthy periodic traffic can outlive one absolute timeout duration.
#[tokio::test(flavor = "current_thread")]
async fn authenticated_control_loop_resets_idle_deadline_after_active_traffic() {
    use crate::control::{AuthenticatedPeer, decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = tokio::io::duplex(8192);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let get_session =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);
    let client = async {
        tokio::time::sleep(Duration::from_millis(40)).await;
        client_stream.write_all(&initialize).await.unwrap();
        let mut first = vec![0; 4096];
        let read = client_stream.read(&mut first).await.unwrap();
        first.truncate(read);
        assert!(
            decode_control_frame(&first, 4096)
                .unwrap()
                .0
                .contains("granted_role")
        );

        tokio::time::sleep(Duration::from_millis(40)).await;
        client_stream.write_all(&get_session).await.unwrap();
        let mut second = vec![0; 4096];
        let read = client_stream.read(&mut second).await.unwrap();
        second.truncate(read);
        assert!(
            decode_control_frame(&second, 4096)
                .unwrap()
                .0
                .contains("session_id")
        );
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut server_stream,
            AuthenticatedPeer::unix_user(current_effective_uid()),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                .unwrap()
                .with_application_idle_timeout(Duration::from_millis(60)),
            None,
            |served, _| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
}

/// A peer that submits initialization but never reads its response cannot pin
/// the connection in an unbounded response write, and disconnect cleanup is
/// consumed exactly once when the write times out.
#[tokio::test(flavor = "current_thread")]
async fn authenticated_control_loop_times_out_blocked_response_write() {
    use crate::control::{AuthenticatedPeer, encode_control_body};
    use tokio::io::AsyncWriteExt;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = tokio::io::duplex(64);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let client = async {
        client_stream.write_all(&initialize).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let error = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut server_stream,
            AuthenticatedPeer::unix_user(current_effective_uid()),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                .unwrap()
                .with_application_idle_timeout(Duration::from_millis(50)),
            None,
            |_, _| false,
        )
        .await
        .unwrap_err();
        assert!(
            error.message().contains("application idle timeout"),
            "{error:?}"
        );
        assert!(
            error.message().contains("writing a control response"),
            "{error:?}"
        );
        assert!(connection.caller_client_id().is_some());
        assert!(connection.take_disconnect_client_id().is_none());
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
}
