//! Async-runtime tests owned by control behavior.

use super::super::*;
use crate::host::async_runtime::serve_authenticated_async_runtime_control_connection_loop_with_snapshots;

/// Routed initialization must retain exact teardown across an actual failed
/// response write, dropped handoff futures, and explicit close. Same-named
/// replacement attachments stay live; opt-out attachments remain unowned.
#[tokio::test(flavor = "current_thread")]
async fn routed_initialization_cleanup_survives_write_failure_and_cancellation() {
    use crate::control::encode_control_body;
    use tokio::io::AsyncWriteExt;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());
    let initialize = |role: &str, detach: bool| {
        encode_control_body(
            &serde_json::json!({
                "jsonrpc":"2.0", "id":"init", "method":"control/initialize",
                "params":{"client_name":"same-name", "requested_version":2,
                    "requested_role":role,"detach_primary_on_disconnect":detach,
                    "client":{"name":"same-name","interactive":true,
                        "terminal":{"columns":80,"rows":24,"term":"xterm"}},
                    "authentication":{"mechanism":"peer_credentials"}}
            })
            .to_string(),
        )
    };
    let mut failed = handle
        .initialize_routed_control_connection(
            initialize("primary", true),
            4096,
            ControlConnectionState::new(true, true),
        )
        .await
        .unwrap();
    let failed_id = failed.connection.caller_client_id().unwrap().clone();
    let replacement = handle
        .handle_control_input_for_connection(
            initialize("primary", false),
            4096,
            ControlConnectionState::new(true, true),
        )
        .await
        .unwrap();
    let replacement_id = replacement.connection.caller_client_id().unwrap().clone();
    assert_ne!(failed_id, replacement_id);
    let (mut writer, reader) = tokio::io::duplex(64);
    drop(reader);
    assert!(
        writer.write_all(&failed.output).await.is_err(),
        "inject real response write failure"
    );
    failed
        .connection_cleanup
        .take()
        .unwrap()
        .close(Duration::from_secs(5))
        .await
        .unwrap();

    let mut cancelled = handle
        .initialize_routed_control_connection(
            initialize("primary", true),
            4096,
            ControlConnectionState::new(true, true),
        )
        .await
        .unwrap();
    let cancelled_id = cancelled.connection.caller_client_id().unwrap().clone();
    let lease = cancelled.connection_cleanup.take().unwrap();
    lease.own_disconnect(&mut cancelled.connection);
    assert!(cancelled.connection.take_disconnect_client_id().is_none());
    let (entered, reached) = tokio::sync::oneshot::channel();
    let handoff = tokio::spawn(async move {
        let _retained_owner = lease;
        entered.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    reached.await.unwrap();
    handoff.abort();
    assert!(handoff.await.unwrap_err().is_cancelled());

    let mut unowned = handle
        .initialize_routed_control_connection(
            initialize("primary", false),
            4096,
            ControlConnectionState::new(true, true),
        )
        .await
        .unwrap();
    let unowned_id = unowned.connection.caller_client_id().unwrap().clone();
    unowned
        .connection_cleanup
        .take()
        .unwrap()
        .close(Duration::from_secs(5))
        .await
        .unwrap();
    let observer = handle
        .initialize_routed_control_connection(
            initialize("observer", true),
            4096,
            ControlConnectionState::new(true, true),
        )
        .await
        .unwrap();
    let observer_id = observer.connection.caller_client_id().unwrap().clone();
    drop(observer); // A lost reply or dropped response must retain teardown too.

    // Cleanup has a fairness allowance of four requests, independent of mailbox
    // pressure. Barriers allow its queued Drop events to settle before shutdown.
    for _ in 0..8 {
        handle.lifecycle_state().await.unwrap();
    }
    handle.shutdown().await.unwrap();
    let exit = actor_task.await.unwrap();
    assert!(!exit.service.session().is_attached_primary(&failed_id));
    assert!(!exit.service.session().is_attached_primary(&cancelled_id));
    assert!(exit.service.session().is_attached_primary(&replacement_id));
    assert!(exit.service.session().is_attached_primary(&unowned_id));
    assert!(
        !exit
            .service
            .session()
            .clients()
            .iter()
            .any(|client| client.id == observer_id
                && client.state == mez_mux::session::ClientState::Attached)
    );
}

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
        #[cfg(target_os = "linux")]
        let origin_supported = {
            use std::os::fd::AsRawFd;
            match crate::runtime::capture_unix_origin(
                server_stream.as_raw_fd(),
                current_effective_uid(),
            ) {
                Ok(_) => true,
                Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => false,
                Err(error) => panic!("origin capture failed: {error}"),
            }
        };
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
        #[cfg(target_os = "linux")]
        {
            assert_eq!(connection.unix_origin().is_some(), origin_supported);
            if let Some(origin) = connection.unix_origin() {
                assert_eq!(origin.reobserve().unwrap().process_id, std::process::id());
                assert_eq!(origin.uid(), current_effective_uid());
                assert_eq!(connection.clone(), connection);
            }
        }
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

/// Injects wrong-UID and connection-local credential-read failures before a
/// real native admission on the same listener. Refusals must close the stream
/// before parsing initialize, allocate no primary, and consume no task capacity.
/// An already-attached same-name sibling answers state heartbeats throughout;
/// two further native primaries prove exact disconnect ownership and capacity
/// reaping while the actor and original live pane PID survive every refusal.
#[tokio::test(flavor = "current_thread")]
async fn async_control_peer_auth_refusals_preserve_listener_actor_and_native_pane() {
    use crate::control::encode_control_body;
    use crate::host::async_runtime::actor_types::serve_unix_control_listener_with_peer_auth;
    use crate::protocol::framing::ProtocolFrameCodec;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio_util::codec::Framed;

    let path = std::env::temp_dir().join(format!("mez-ctl-{}-peer-auth.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service();
    let started = service
        .start_initial_pane_process(Some("exec sleep 30"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"same-name","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"same-name","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let refusals = std::cell::RefCell::new(Vec::new());
    let client = async {
        let body = crate::control::decode_control_frame(&initialize, 16 * 1024)
            .unwrap()
            .0;
        let stream = UnixStream::connect(&path).await.unwrap();
        let mut sibling = Framed::new(stream, ProtocolFrameCodec::new(16 * 1024).unwrap());
        sibling
            .send(crate::protocol::framing::ProtocolFrame::new(
                crate::control::CONTROL_CONTENT_TYPE,
                body.clone(),
            ))
            .await
            .unwrap();
        let frame = sibling.next().await.unwrap().unwrap();
        let initialized: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert_eq!(
            initialized["result"]["session"]["attached_primary_count"],
            1
        );
        let sibling_id = initialized["result"]["client"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        for _ in 0..16 {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            // Even a complete initialization must not reach the actor on refusal.
            let _ = stream.write_all(&initialize).await;
            let mut output = [0; 1];
            match stream.read(&mut output).await {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) => {}
                other => panic!("refused stream returned protocol output: {other:?}"),
            }
            // This is a real framed round trip on an existing sibling, not
            // merely a handle probe. Refused initialize never creates a client.
            sibling
                .send(crate::protocol::framing::ProtocolFrame::new(
                    crate::control::CONTROL_CONTENT_TYPE,
                    r#"{"jsonrpc":"2.0","id":"heartbeat","method":"client/list","params":{}}"#,
                ))
                .await
                .unwrap();
            let frame = sibling.next().await.unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            let clients = value["result"]["clients"].as_array().unwrap();
            assert_eq!(clients.len(), 1);
            assert_eq!(clients[0]["id"], sibling_id);
            assert_eq!(clients[0]["state"], "attached");
        }
        assert_eq!(refusals.borrow().len(), 5);
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        // SAFETY: signal zero probes the test-owned child without changing it.
        assert_eq!(unsafe { libc::kill(started.primary_pid as i32, 0) }, 0);
        let mut previous_id = None;
        for _ in 0..2 {
            let stream = UnixStream::connect(&path).await.unwrap();
            let mut framed = Framed::new(stream, ProtocolFrameCodec::new(16 * 1024).unwrap());
            let body = crate::control::decode_control_frame(&initialize, 16 * 1024)
                .unwrap()
                .0;
            framed
                .send(crate::protocol::framing::ProtocolFrame::new(
                    crate::control::CONTROL_CONTENT_TYPE,
                    body,
                ))
                .await
                .unwrap();
            let frame = framed.next().await.unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(value["result"]["granted_role"], "primary");
            assert_eq!(value["result"]["session"]["attached_primary_count"], 2);
            let id = value["result"]["client"]["id"]
                .as_str()
                .unwrap()
                .to_string();
            assert_ne!(id, sibling_id);
            assert_ne!(previous_id.as_ref(), Some(&id));
            previous_id = Some(id);
            assert_eq!(
                handle.lifecycle_state().await.unwrap(),
                RuntimeLifecycleState::Running
            );
            // The original pane has not been replaced or killed by refusal/cleanup.
            assert_eq!(unsafe { libc::kill(started.primary_pid as i32, 0) }, 0);
            framed.get_mut().shutdown().await.unwrap();
            drop(framed);
            // Wait on real sibling replies until the exact closed attachment
            // is detached. The same-name sibling must remain authoritative.
            loop {
                sibling
                    .send(crate::protocol::framing::ProtocolFrame::new(
                        crate::control::CONTROL_CONTENT_TYPE,
                        r#"{"jsonrpc":"2.0","id":"state","method":"session/get","params":{}}"#,
                    ))
                    .await
                    .unwrap();
                let frame = sibling.next().await.unwrap().unwrap();
                let value: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
                if value["result"]["session"]["attached_primary_count"] == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        sibling.get_mut().shutdown().await.unwrap();
    };
    let server = async {
        let mut attempts = 0;
        let accepted = serve_unix_control_listener_with_peer_auth(
            &listener,
            &handle,
            AsyncRuntimeControlConnectionConfig::new(16 * 1024, current_effective_uid()).unwrap(),
            None,
            2,
            |accepted, _| accepted >= 3,
            |fd, owner| {
                attempts += 1;
                if (2..=17).contains(&attempts) {
                    if attempts % 2 == 0 {
                        crate::runtime::authorize_unix_peer_uid(owner.wrapping_add(1), owner)?;
                    } else {
                        let kind = [
                            std::io::ErrorKind::ConnectionReset,
                            std::io::ErrorKind::ConnectionAborted,
                            std::io::ErrorKind::NotConnected,
                            std::io::ErrorKind::BrokenPipe,
                            std::io::ErrorKind::UnexpectedEof,
                        ][(attempts / 2) % 5];
                        return Err(std::io::Error::new(
                            kind,
                            "private credential-read detail must not be reported",
                        )
                        .into());
                    }
                }
                crate::runtime::authenticated_unix_peer_uid(fd, owner)
            },
            |count, reason| refusals.borrow_mut().push((count, reason)),
        )
        .await
        .expect("peer-local refusal must not end the listener");
        assert_eq!(accepted, 3);
        assert_eq!(attempts, 19);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
    };
    let ((), (), mut exit) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(client, server, actor.run())
    })
    .await
    .expect("refusals must not leak listener capacity or hang shutdown");
    assert_eq!(
        exit.service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    assert_eq!(exit.service.session().clients().len(), 3);
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .all(|client| client.state != ClientState::Attached)
    );
    assert_eq!(
        refusals.into_inner(),
        vec![
            (1, "owner-mismatch"),
            (2, "credential-read"),
            (4, "credential-read"),
            (8, "credential-read"),
            (16, "credential-read")
        ]
    );
    exit.service.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_file(path);
}

/// Repeated refusals allocate no authority even when the listener future is
/// cancelled or the actor shuts down. Cancellation leaves the original bound
/// listener reusable: a real same-owner initialize then fills its single slot
/// and detaches normally. Actor shutdown must wake an idle refusal-only accept
/// loop rather than hang waiting for another peer or a leaked connection task.
#[tokio::test(flavor = "current_thread")]
async fn async_control_peer_auth_refusals_allow_cancellation_and_shutdown() {
    use crate::control::encode_control_body;
    use crate::host::async_runtime::actor_types::serve_unix_control_listener_with_peer_auth;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::AsyncReadExt;
    use tokio::net::{UnixListener, UnixStream};
    use tokio_util::codec::Framed;

    for cancel in [true, false] {
        let path = std::env::temp_dir().join(format!(
            "mez-ctl-{}-auth-stop-{cancel}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
            .build()
            .unwrap();
        let actor_task = tokio::spawn(actor.run());
        let config =
            AsyncRuntimeControlConnectionConfig::new(16 * 1024, current_effective_uid()).unwrap();
        let refusals = std::cell::Cell::new(0);
        tokio::time::timeout(Duration::from_secs(10), async {
            {
                let server = serve_unix_control_listener_with_peer_auth(
                    &listener, &handle, config, None, 1, |_, _| false,
                    |_, owner| {
                        crate::runtime::authorize_unix_peer_uid(owner.wrapping_add(1), owner)?;
                        unreachable!("wrong owner must be refused")
                    },
                    |count, reason| {
                        assert_eq!(reason, "owner-mismatch");
                        refusals.set(count);
                    },
                );
                tokio::pin!(server);
                let client = async {
                    for _ in 0..8 {
                        let mut stream = UnixStream::connect(&path).await.unwrap();
                        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
                    }
                    assert_eq!(refusals.get(), 8);
                    assert_eq!(handle.lifecycle_state().await.unwrap(), RuntimeLifecycleState::Running);
                };
                tokio::select! {
                    result = &mut server => panic!("refusal ended listener before cancellation/shutdown: {result:?}"),
                    () = client => {},
                }
                if !cancel {
                    handle.shutdown().await.unwrap();
                    assert_eq!(server.await.unwrap(), 0);
                }
                // Dropping the pinned future here cancels it, not the bound listener.
            }
            if cancel {
                let client = async {
                    let stream = UnixStream::connect(&path).await.unwrap();
                    let mut framed = Framed::new(stream, crate::protocol::framing::ProtocolFrameCodec::new(16 * 1024).unwrap());
                    let initialize = encode_control_body(r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"after-cancel","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"after-cancel","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}},"authentication":{"mechanism":"peer_credentials"}}}"#);
                    let body = crate::control::decode_control_frame(&initialize, 16 * 1024).unwrap().0;
                    framed.send(crate::protocol::framing::ProtocolFrame::new(crate::control::CONTROL_CONTENT_TYPE, body)).await.unwrap();
                    let frame = framed.next().await.unwrap().unwrap();
                    let value: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
                    assert_eq!(value["result"]["granted_role"], "primary");
                    assert_eq!(value["result"]["session"]["attached_primary_count"], 1);
                };
                let server = crate::host::async_runtime::serve_async_runtime_control_listener_with_snapshots(
                    &listener, &handle, config, None, 1, |accepted, _| accepted == 1,
                );
                let ((), accepted) = tokio::join!(client, server);
                assert_eq!(accepted.unwrap(), 1);
                handle.shutdown().await.unwrap();
            }
        }).await.expect("refusal-only cancellation and actor shutdown must be bounded");
        let exit = actor_task.await.unwrap();
        assert_eq!(exit.service.session().clients().len(), usize::from(cancel));
        assert!(
            exit.service
                .session()
                .clients()
                .iter()
                .all(|client| client.state != ClientState::Attached)
        );
        std::fs::remove_file(path).unwrap();
    }
}

/// Unsupported credential APIs, invalid descriptor/API state, and unknown
/// infrastructure failures remain essential-listener errors. Each injection
/// uses an owned connected socket and proves fail-closed admission: no response,
/// no client allocation, no refusal diagnostic, and no accidental success.
#[tokio::test(flavor = "current_thread")]
async fn async_control_peer_auth_infrastructure_failures_remain_critical() {
    use crate::host::async_runtime::actor_types::serve_unix_control_listener_with_peer_auth;
    use std::io::ErrorKind;
    use tokio::io::AsyncReadExt;
    use tokio::net::{UnixListener, UnixStream};

    let errors = [
        ErrorKind::Unsupported,
        ErrorKind::InvalidInput,
        ErrorKind::InvalidData,
        ErrorKind::PermissionDenied,
        ErrorKind::OutOfMemory,
        ErrorKind::Other,
        ErrorKind::Interrupted,
        ErrorKind::WouldBlock,
        ErrorKind::TimedOut,
    ]
    .into_iter()
    .map(|kind| std::io::Error::new(kind, "essential credential facility failed"))
    .chain(
        [
            libc::EBADF,
            libc::ENOTSOCK,
            libc::EINVAL,
            libc::ENOSYS,
            libc::ENOPROTOOPT,
        ]
        .into_iter()
        .map(std::io::Error::from_raw_os_error),
    );
    for error in errors {
        let kind = error.kind();
        let mut injected = Some(error);
        let path =
            std::env::temp_dir().join(format!("mez-ctl-{}-auth-critical.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
            .build()
            .unwrap();
        let client = async {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            let mut output = [0; 1];
            assert_eq!(stream.read(&mut output).await.unwrap(), 0);
        };
        let server = async {
            let error = serve_unix_control_listener_with_peer_auth(
                &listener,
                &handle,
                AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
                None,
                1,
                |_, _| false,
                |_, _| {
                    Err(injected
                        .take()
                        .expect("exactly one authentication attempt")
                        .into())
                },
                |_, _| panic!("critical errors must not be counted as peer refusals"),
            )
            .await
            .unwrap_err();
            assert_eq!(error.io_kind(), Some(kind));
            handle.shutdown().await.unwrap();
        };
        let ((), (), exit) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(client, server, actor.run())
        })
        .await
        .expect("critical authentication failure must fail closed without hanging");
        assert!(exit.service.session().clients().is_empty());
        std::fs::remove_file(path).unwrap();
    }
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
/// Paused time preserves the intended interval despite test-thread contention.
#[tokio::test(flavor = "current_thread", start_paused = true)]
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
/// Paused time isolates deadline resetting from host scheduler latency.
#[tokio::test(flavor = "current_thread", start_paused = true)]
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
        tokio::time::sleep(Duration::from_millis(150)).await;
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

        tokio::time::sleep(Duration::from_millis(150)).await;
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
                .with_application_idle_timeout(Duration::from_millis(250)),
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

/// A terminal `:exit` response must flush before supervisor-visible shutdown
/// revokes the connection-owned X11 route and permits service teardown.
#[tokio::test(flavor = "current_thread")]
async fn terminal_exit_flushes_before_x11_route_and_lifecycle_teardown() {
    use crate::control::{AuthenticatedPeer, decode_control_frame, encode_control_body};
    use crate::runtime::x11::{
        RuntimeX11Proxy, RuntimeX11RouteOwner, X11_FORWARDING_VERSION, X11AuthProtocol, X11Cookie,
        X11ForwardingMode, X11ForwardingOffer,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let root = std::env::temp_dir().join(format!(
        "mez-x11-exit-flush-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let mut service = test_service();
    let primary = service
        .attach_primary("x11-exit-owner", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut lifecycle = handle.lifecycle_state_watcher();

    let proxy = RuntimeX11Proxy::prepare(&root).unwrap();
    let proxy_handle = proxy.handle();
    let (_route, lease) = proxy_handle
        .reserve_route(
            RuntimeX11RouteOwner {
                session_id: "$exit-flush".to_string(),
                client_id: primary.to_string(),
                endpoint_id: "endpoint-exit-flush".to_string(),
                principal_id: Some("principal-exit-flush".to_string()),
                connection_id: "connection-exit-flush".to_string(),
            },
            X11ForwardingOffer {
                version: X11_FORWARDING_VERSION,
                mode: X11ForwardingMode::Untrusted,
                auth_protocol: X11AuthProtocol::MitMagicCookie1,
                fake_cookie: X11Cookie::new([0x61; 16]),
                takeover: false,
            },
        )
        .unwrap();
    lease.activate_without_transport().unwrap();

    let mut connection = ControlConnectionState::trusted_existing_client(primary);
    connection.install_x11_route_lease_for_test(lease);
    let (mut client_stream, mut server_stream) = tokio::io::duplex(64);
    let exit = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"exit","method":"terminal/step","params":{"idempotency_key":"exit-flush","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[1,58,101,120,105,116,13]}}"#,
    );

    let client = async {
        client_stream.write_all(&exit).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(*lifecycle.borrow(), RuntimeLifecycleState::Running);
        assert!(proxy_handle.diagnostics().route_active);

        let mut response = Vec::new();
        let body = loop {
            let mut buffer = [0u8; 64];
            let read =
                tokio::time::timeout(Duration::from_secs(2), client_stream.read(&mut buffer))
                    .await
                    .expect("terminal exit response must precede control EOF")
                    .unwrap();
            assert!(read > 0, "terminal exit response must precede control EOF");
            response.extend_from_slice(&buffer[..read]);
            if let Ok((body, consumed)) = decode_control_frame(&response, 4096) {
                assert_eq!(consumed, response.len());
                break body;
            }
        };
        assert!(body.contains(r#""session_terminated":true"#), "{body}");

        tokio::time::timeout(Duration::from_secs(2), async {
            while *lifecycle.borrow() != RuntimeLifecycleState::Killed {
                lifecycle.changed().await.unwrap();
            }
        })
        .await
        .expect("terminal lifecycle must publish after the response flush");
        assert!(!proxy_handle.diagnostics().route_active);
    };
    let server = async {
        let served = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut server_stream,
            AuthenticatedPeer::iroh_endpoint("endpoint-exit-flush"),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            None,
            |_, state| {
                matches!(
                    state,
                    RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
                )
            },
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert!(!connection.deactivate_x11_route().unwrap());
        handle.shutdown().await.unwrap();
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    drop(proxy);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies control EOF invalidates the exact X11 generation before waiting
/// for an unavailable actor to apply the ordinary client-disconnect event.
/// Resuming actor processing must complete delayed cleanup without reviving or
/// double-removing the route.
#[tokio::test(flavor = "current_thread")]
async fn control_disconnect_invalidates_x11_before_stalled_actor_submission() {
    use crate::control::AuthenticatedPeer;
    use crate::runtime::x11::{
        RuntimeX11Proxy, RuntimeX11RouteOwner, X11_FORWARDING_VERSION, X11AuthProtocol, X11Cookie,
        X11ForwardingMode, X11ForwardingOffer,
    };

    let root = std::env::temp_dir().join(format!(
        "mez-x11-stalled-disconnect-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let mut service = test_service();
    let primary = service
        .attach_primary("x11-owner", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let proxy = RuntimeX11Proxy::prepare(&root).unwrap();
    let proxy_handle = proxy.handle();
    let authority_path = proxy_handle.authority_path().to_path_buf();
    let (_route, lease) = proxy_handle
        .reserve_route(
            RuntimeX11RouteOwner {
                session_id: "$stalled-disconnect".to_string(),
                client_id: primary.to_string(),
                endpoint_id: "endpoint-stalled-disconnect".to_string(),
                principal_id: Some("principal-stalled-disconnect".to_string()),
                connection_id: "connection-stalled-disconnect".to_string(),
            },
            X11ForwardingOffer {
                version: X11_FORWARDING_VERSION,
                mode: X11ForwardingMode::Untrusted,
                auth_protocol: X11AuthProtocol::MitMagicCookie1,
                fake_cookie: X11Cookie::new([0x51; 16]),
                takeover: false,
            },
        )
        .unwrap();
    lease.activate_without_transport().unwrap();
    assert!(proxy_handle.diagnostics().route_active);
    assert!(!std::fs::read(&authority_path).unwrap().is_empty());

    let mut connection = ControlConnectionState::new(false, false);
    connection.rebind_caller_client(primary);
    connection.own_rebound_client_disconnect_for_test();
    connection.install_x11_route_lease_for_test(lease);
    let (client_stream, mut server_stream) = tokio::io::duplex(64);
    drop(client_stream);

    let mut serving = Box::pin(
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut server_stream,
            AuthenticatedPeer::iroh_endpoint("endpoint-stalled-disconnect"),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            None,
            |_, _| false,
        ),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut serving)
            .await
            .is_err(),
        "disconnect delivery should remain pending while the actor is unpolled"
    );
    assert!(!proxy_handle.diagnostics().route_active);
    assert_eq!(std::fs::read(&authority_path).unwrap(), Vec::<u8>::new());

    let actor_task = tokio::spawn(actor.run());
    let served = tokio::time::timeout(Duration::from_secs(2), &mut serving)
        .await
        .expect("resumed actor should apply delayed disconnect cleanup")
        .unwrap();
    assert_eq!(served, 0);
    drop(serving);
    assert!(!connection.deactivate_x11_route().unwrap());

    handle.shutdown().await.unwrap();
    let _exit = actor_task.await.unwrap();
    drop(proxy);
    let _ = std::fs::remove_dir_all(root);
}
