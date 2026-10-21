//! Async-runtime tests owned by message behavior.

use super::super::*;
use tokio::net::{UnixListener, UnixStream};

mod native_recipients;

/// Verifies async message connection dispatches hello.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_dispatches_hello() {
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
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
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
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
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-message-listener-{}-{}.sock",
        std::process::id(),
        "stateful"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
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
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-message-listener-{}-{}.sock",
        std::process::id(),
        "concurrent"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
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

/// A one-connection limit bounds simultaneous message work rather than the
/// listener's lifetime, so a completed client immediately frees capacity for
/// a second sequential client.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_reuses_capacity_after_disconnect() {
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path =
        std::env::temp_dir().join(format!("mez-message-cap-{}-reuse.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);

    let clients = async {
        for _ in 0..2 {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            stream.write_all(&hello).await.unwrap();
            let mut output = vec![0; 4096];
            let read =
                tokio::time::timeout(std::time::Duration::from_secs(1), stream.read(&mut output))
                    .await
                    .expect("sequential client should receive listener capacity")
                    .unwrap();
            output.truncate(read);
            let (body, _) = decode_mmp_frame(&output, 4096).unwrap();
            assert!(body.contains(r#""type":"welcome""#), "{body}");
            stream.shutdown().await.unwrap();
        }
    };
    let server = async {
        let served = serve_async_runtime_message_listener_concurrent(
            &listener,
            &handle,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            10,
            1,
            |served, _| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        handle.shutdown().await.unwrap();
    };

    let ((), (), _exit) = tokio::join!(clients, server, actor.run());
    let _ = std::fs::remove_file(&path);
}

/// Malformed peers must be reaped without killing the listener or actor. A
/// one-slot listener must accept a replacement that completes hello and
/// heartbeat, while the original native pane process remains alive. Testing
/// invalid headers, UTF-8 bodies, content type, size, truncation and clean EOF
/// protects connection-local rejection without resynchronizing a bad stream.
/// Valid-framed malformed JSON and unsupported protocol bodies instead receive
/// structured errors and leave that replacement connection usable for hello.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_reaps_failed_connection_tasks_during_accept() {
    use crate::protocol::message::encode_mmp_body;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-message-listener-{}-reap.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service();
    service.start_initial_pane_process(None).unwrap();
    let original_pid = service.pane_processes().primary_pid("%1").unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let rejected: [&[u8]; 6] = [
            b"Content-Length: invalid\r\n\r\n",
            b"Content-Length: 1\r\n\r\n\xff",
            b"Content-Length: 2\r\nContent-Type: text/plain\r\n\r\n{}",
            b"Content-Length: 4097\r\n\r\n",
            b"Content-Length: 20\r\n\r\n{",
            b"",
        ];
        for input in rejected {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            stream.write_all(input).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut output = [0; 1];
            let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut output))
                .await
                .expect("rejected peer must close without another accept");
            assert!(
                matches!(read, Ok(0) | Err(_)),
                "unexpected peer response: {read:?}"
            );

            let mut replacement = UnixStream::connect(&path).await.unwrap();
            for (body, expected_code) in [
                ("{", "invalid_envelope"),
                (
                    r#"{"protocol":"mmp/2","type":"hello"}"#,
                    "unsupported_protocol",
                ),
            ] {
                replacement.write_all(&encode_mmp_body(body)).await.unwrap();
                let body = read_listener_response(&mut replacement).await;
                let error: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(error["type"], "error");
                assert_eq!(error["error"]["code"], expected_code);
                assert_eq!(error["error"]["retryable"], false);
            }
            for (body, expected) in [
                (
                    r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
                    "welcome",
                ),
                (
                    r#"{"protocol":"mmp/1","type":"heartbeat","id":"still-alive"}"#,
                    "ack",
                ),
            ] {
                replacement.write_all(&encode_mmp_body(body)).await.unwrap();
                let body = read_listener_response(&mut replacement).await;
                assert!(body.contains(&format!(r#""type":"{expected}""#)), "{body}");
            }
            replacement.shutdown().await.unwrap();
            assert!(
                handle.metrics().await.is_ok(),
                "same actor must remain responsive"
            );
            assert!(
                std::process::Command::new("kill")
                    .args(["-0", &original_pid.to_string()])
                    .status()
                    .unwrap()
                    .success(),
                "original pane must survive"
            );
        }
    };
    let server = async {
        let served = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            serve_async_runtime_message_listener_concurrent(
                &listener,
                &handle,
                AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
                10,
                1,
                |accepted, _| accepted >= 12,
            ),
        )
        .await
        .expect("listener must drain without hanging")
        .expect("ordinary peer errors must not fail the listener");
        assert_eq!(served, 12);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());
    assert_eq!(
        exit.service.pane_processes().primary_pid("%1"),
        Some(original_pid)
    );
    assert!(exit.service.message_service().discover_agents().is_empty());
    assert!(
        exit.service
            .message_service()
            .snapshot_state()
            .subscriptions
            .is_empty()
    );
    let _ = std::fs::remove_file(&path);
}

/// A peer that shuts its read side before hello response must release its
/// registration and the one active slot. Repeated hello on its replacement
/// retires only the previous raw identity. Stale duplicate cleanup must leave
/// the replacement subscription and a pane-backed subscription untouched. A
/// final malformed peer forces the listener's final drain error path as well.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_contains_response_failure_and_final_drain() {
    use crate::protocol::message::encode_mmp_body;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!("mez-mmp-drain-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service();
    let pane_identity = service.message_service_mut().register_agent(
        Some(mez_core::ids::PaneId::new('%', 1)),
        None,
        "pane",
        Vec::new(),
    );
    service
        .message_service_mut()
        .subscribe(&pane_identity.agent_id)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"raw"}"#);
    let clients = async {
        let mut broken = UnixStream::connect(&path).await.unwrap();
        broken.write_all(&hello).await.unwrap();
        broken
            .into_std()
            .unwrap()
            .shutdown(std::net::Shutdown::Both)
            .unwrap();

        let mut replacement = UnixStream::connect(&path).await.unwrap();
        let mut ids = Vec::new();
        for _ in 0..2 {
            replacement.write_all(&hello).await.unwrap();
            let body = read_replacement_welcome(&mut replacement).await;
            let welcome: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(welcome["type"], "welcome");
            ids.push(AgentId::opaque(welcome["identity"]["agent_id"].as_str().unwrap()).unwrap());
        }
        assert_ne!(ids[0], ids[1]);
        handle
            .retire_message_connection(ids[0].clone())
            .await
            .unwrap();
        handle
            .retire_message_connection(pane_identity.agent_id.clone())
            .await
            .unwrap();
        replacement
            .write_all(&encode_mmp_body(
                r#"{"protocol":"mmp/1","type":"heartbeat","id":"replacement"}"#,
            ))
            .await
            .unwrap();
        let body = read_listener_response(&mut replacement).await;
        assert!(body.contains(r#""type":"ack""#), "{body}");
        replacement
            .write_all(b"Content-Length: invalid\r\n\r\n")
            .await
            .unwrap();
        let mut output = [0; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), replacement.read(&mut output))
            .await
            .unwrap();
        assert!(matches!(read, Ok(0) | Err(_)));
        let mut final_peer = UnixStream::connect(&path).await.unwrap();
        final_peer
            .write_all(b"Content-Length: 4097\r\n\r\n")
            .await
            .unwrap();
    };
    let server = async {
        let accepted = tokio::time::timeout(
            Duration::from_secs(5),
            serve_async_runtime_message_listener_concurrent(
                &listener,
                &handle,
                AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
                10,
                1,
                |accepted, _| accepted >= 3,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(accepted, 3);
        handle.shutdown().await.unwrap();
    };
    let ((), (), exit) = tokio::join!(clients, server, actor.run());
    assert_eq!(
        exit.service.message_service().discover_agents(),
        vec![pane_identity.clone()]
    );
    assert!(
        exit.service
            .message_service()
            .subscription(&pane_identity.agent_id)
            .is_some()
    );
    assert_eq!(
        exit.service
            .message_service()
            .snapshot_state()
            .subscriptions
            .len(),
        1
    );
    let _ = std::fs::remove_file(&path);
}

/// Unexpected actor loss is not an ordinary malformed-peer result. Active
/// accept must report it, rather than allowing the session
/// supervisor to mistake a dead actor for a successfully isolated connection.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_actor_unavailable_remains_fatal() {
    use tokio::net::UnixListener;
    let path = std::env::temp_dir().join(format!("mez-mmp-dead-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    drop(actor);
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        serve_async_runtime_message_listener_concurrent(
            &listener,
            &handle,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            10,
            1,
            |_, _| false,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        error.message().contains("actor lifecycle unavailable"),
        "{error}"
    );
    let _ = std::fs::remove_file(&path);
}

/// Reads one bounded response for strict framed listener controls. These
/// controls issue no request until the previous response completes; preserving
/// fragmented socket reads avoids making rejection depend on write packet size.
async fn read_listener_response(stream: &mut UnixStream) -> String {
    use crate::protocol::message::decode_mmp_frame;
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut output = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "replacement closed before welcome");
            output.extend_from_slice(&chunk[..read]);
            assert!(output.len() <= 4096);
            if let Ok((body, consumed)) = decode_mmp_frame(&output, 4096) {
                assert_eq!(consumed, output.len());
                return body;
            }
        }
    })
    .await
    .unwrap()
}

/// Requires actual welcome on a separate replacement, not just arbitrary bytes.
async fn read_replacement_welcome(stream: &mut UnixStream) -> String {
    let body = read_listener_response(stream).await;
    assert!(body.contains(r#""type":"welcome""#), "{body}");
    body
}

/// Valid raw objective updates must not turn disconnect or repeated hello into
/// critical actor/listener failure. Two sent versions and a third unsent current
/// objective retain exact canonical mail/receipts through production cleanup;
/// a new explicit socket then proves one-slot capacity and native pane survival.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_preserves_sent_objective_versions_on_retirement() {
    use crate::protocol::message::encode_mmp_body;
    use tokio::io::AsyncWriteExt;
    for repeat_hello in [false, true] {
        let path = std::env::temp_dir().join(format!(
            "mez-mmp-objectives-{}-{repeat_hello}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let mut service = test_service();
        service.start_initial_pane_process(None).unwrap();
        let original_pid = service.pane_processes().primary_pid("%1").unwrap();
        let target = service
            .message_service_mut()
            .register_agent(None, None, "target", Vec::new());
        let target_cursor = service
            .message_service_mut()
            .subscribe(&target.agent_id)
            .unwrap();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
            .build()
            .unwrap();
        let clients = async {
            let mut peer = UnixStream::connect(&path).await.unwrap();
            peer.write_all(&encode_mmp_body(
                r#"{"protocol":"mmp/1","type":"hello","role":"raw","objective":"A"}"#,
            ))
            .await
            .unwrap();
            let welcome: serde_json::Value =
                serde_json::from_str(&read_replacement_welcome(&mut peer).await).unwrap();
            let id = welcome["identity"]["agent_id"].as_str().unwrap();
            for (index, objective) in ["A", "B", "C unsent"].into_iter().enumerate() {
                if index > 0 {
                    peer.write_all(&encode_mmp_body(&format!(
                        r#"{{"protocol":"mmp/1","type":"presence","objective":"{objective}"}}"#
                    )))
                    .await
                    .unwrap();
                    assert!(
                        read_listener_response(&mut peer)
                            .await
                            .contains(r#""type":"ack""#)
                    );
                }
                if index < 2 {
                    let body = format!(
                        r#"{{"protocol":"mmp/1","type":"send","id":"objective-{index}","time":"message:test","sender":{{"agent_id":"{id}","role":"raw"}},"recipient":{{"agent_id":"{}"}},"scope":"session","correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"fixed retained mail"}}"#,
                        target.agent_id
                    );
                    peer.write_all(&encode_mmp_body(&body)).await.unwrap();
                    assert!(
                        read_listener_response(&mut peer)
                            .await
                            .contains(r#""status":"accepted""#)
                    );
                }
            }
            if repeat_hello {
                peer.write_all(&encode_mmp_body(
                    r#"{"protocol":"mmp/1","type":"hello","role":"next-raw"}"#,
                ))
                .await
                .unwrap();
                read_replacement_welcome(&mut peer).await;
            }
            peer.shutdown().await.unwrap();
            let mut replacement = UnixStream::connect(&path).await.unwrap();
            replacement
                .write_all(&encode_mmp_body(
                    r#"{"protocol":"mmp/1","type":"hello","role":"replacement"}"#,
                ))
                .await
                .unwrap();
            read_replacement_welcome(&mut replacement).await;
            assert!(handle.metrics().await.is_ok());
            assert!(
                std::process::Command::new("kill")
                    .args(["-0", &original_pid.to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            replacement.shutdown().await.unwrap();
        };
        let server = async {
            assert_eq!(
                serve_async_runtime_message_listener_concurrent(
                    &listener,
                    &handle,
                    AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
                    10,
                    1,
                    |accepted, _| accepted >= 2
                )
                .await
                .unwrap(),
                2
            );
            handle.shutdown().await.unwrap();
        };
        let ((), (), exit) = tokio::join!(clients, server, actor.run());
        let after = exit.service.message_service().snapshot_state();
        assert_eq!(
            exit.service.pane_processes().primary_pid("%1"),
            Some(original_pid)
        );
        assert_eq!(
            exit.service
                .message_service()
                .subscription(&target.agent_id),
            Some(&target_cursor)
        );
        assert_eq!(after.retained_messages.len(), 2);
        assert_eq!(after.accepted_messages.len(), 2);
        let versions = after.retired_delivery_floors[0]
            .fresh_sender_versions
            .as_ref()
            .unwrap();
        assert_eq!(
            versions
                .iter()
                .map(|version| version.objective.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("A"), Some("B")]
        );
        assert_eq!(
            after
                .retained_messages
                .iter()
                .map(|retained| retained.envelope.sender.objective.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("A"), Some("B")]
        );
        assert_eq!(
            mez_agent::messaging::MessageService::from_snapshot_state(&after)
                .unwrap()
                .snapshot_state(),
            after
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// Invalid framing immediately followed by a valid effectful hello must close
/// without scanning forward, executing hello or leaving a registration. A
/// separate explicit connection then proves the same one-slot listener survives.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_does_not_scan_past_invalid_framing() {
    use crate::protocol::message::encode_mmp_body;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let path = std::env::temp_dir().join(format!("mez-mmp-no-scan-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let service = test_service();
    let mut pristine = mez_agent::messaging::MessageService::from_snapshot_state(
        &service.message_service().snapshot_state(),
    )
    .unwrap();
    let expected = pristine
        .register_agent(None, None, "replacement", Vec::new())
        .agent_id;
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let clients = async {
        let mut bad = UnixStream::connect(&path).await.unwrap();
        let mut input = b"Content-Length: invalid\r\n\r\n".to_vec();
        input.extend(encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"must-not-run"}"#,
        ));
        bad.write_all(&input).await.unwrap();
        let mut output = [0; 4096];
        let read = tokio::time::timeout(Duration::from_secs(2), bad.read(&mut output))
            .await
            .unwrap();
        assert!(
            matches!(read, Ok(0) | Err(_)),
            "bad stream was scanned/replayed"
        );
        let mut replacement = UnixStream::connect(&path).await.unwrap();
        replacement
            .write_all(&encode_mmp_body(
                r#"{"protocol":"mmp/1","type":"hello","role":"replacement"}"#,
            ))
            .await
            .unwrap();
        let welcome: serde_json::Value =
            serde_json::from_str(&read_replacement_welcome(&mut replacement).await).unwrap();
        assert_eq!(
            welcome["identity"]["agent_id"],
            expected.as_str(),
            "bad stream must not allocate even a subsequently retired hello identity"
        );
        replacement.shutdown().await.unwrap();
    };
    let server = async {
        assert_eq!(
            serve_async_runtime_message_listener_concurrent(
                &listener,
                &handle,
                AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
                10,
                1,
                |accepted, _| accepted >= 2
            )
            .await
            .unwrap(),
            2
        );
        handle.shutdown().await.unwrap();
    };
    let ((), (), exit) = tokio::join!(clients, server, actor.run());
    assert!(exit.service.message_service().discover_agents().is_empty());
    assert!(
        exit.service
            .message_service()
            .snapshot_state()
            .subscriptions
            .is_empty()
    );
    let _ = std::fs::remove_file(&path);
}

/// Actor loss after an actually accepted task must remain critical in final
/// drain. The accept predicate signals before the actor is dropped, proving
/// that this control exercises the final join rather than idle active accept.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_final_drain_actor_loss_is_critical() {
    use crate::protocol::message::encode_mmp_body;
    use tokio::io::AsyncWriteExt;
    let path =
        std::env::temp_dir().join(format!("mez-mmp-final-actor-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (accepted, acceptance) = tokio::sync::oneshot::channel();
    let mut accepted = Some(accepted);
    let server = serve_async_runtime_message_listener_concurrent(
        &listener,
        &handle,
        AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
        10,
        1,
        |count, _| {
            if count == 1 {
                accepted.take().unwrap().send(()).unwrap();
                true
            } else {
                false
            }
        },
    );
    let client = async {
        let mut peer = UnixStream::connect(&path).await.unwrap();
        peer.write_all(&encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello"}"#))
            .await
            .unwrap();
        acceptance.await.unwrap();
        drop(actor);
        peer
    };
    let (result, _peer) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(server, client)
    })
    .await
    .unwrap();
    assert_eq!(
        result.unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidState
    );
    let _ = std::fs::remove_file(&path);
}

/// A real listener fanout write failure must release its raw lease and active
/// slot without acknowledging shared mail or stopping the same actor. A sibling
/// cursor and retained acceptance record survive while a replacement says hello.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_fanout_failure_reuses_exact_capacity() {
    use crate::protocol::message::encode_mmp_body;
    use tokio::io::AsyncWriteExt;
    let path =
        std::env::temp_dir().join(format!("mez-mmp-fanout-reuse-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", Vec::new());
    let sibling = service
        .message_service_mut()
        .register_agent(None, None, "sibling", Vec::new());
    let sibling_cursor = service
        .message_service_mut()
        .subscribe(&sibling.agent_id)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let clients = async {
        let mut peer = UnixStream::connect(&path).await.unwrap();
        peer.write_all(&encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"broken-fanout"}"#,
        ))
        .await
        .unwrap();
        read_replacement_welcome(&mut peer).await;
        let peer = peer.into_std().unwrap();
        peer.shutdown(std::net::Shutdown::Read).unwrap();
        let body = format!(
            r#"{{"protocol":"mmp/1","type":"send","id":"listener-fanout","time":"message:test","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{{"session":true}},"scope":"session","correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"keep sibling mail"}}"#,
            sender.agent_id
        );
        let result = handle
            .handle_message_input(
                encode_mmp_body(&body),
                4096,
                MessageConnection {
                    agent_id: Some(sender.agent_id.clone()),
                    delivery_cursor: None,
                },
                11,
            )
            .await
            .unwrap();
        assert!(
            String::from_utf8(result.output)
                .unwrap()
                .contains(r#""status":"accepted""#)
        );
        let mut replacement = UnixStream::connect(&path).await.unwrap();
        replacement
            .write_all(&encode_mmp_body(
                r#"{"protocol":"mmp/1","type":"hello","role":"replacement"}"#,
            ))
            .await
            .unwrap();
        read_replacement_welcome(&mut replacement).await;
        assert!(handle.metrics().await.is_ok());
        replacement.shutdown().await.unwrap();
        drop(peer);
    };
    let server = async {
        assert_eq!(
            serve_async_runtime_message_listener_concurrent(
                &listener,
                &handle,
                AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
                10,
                1,
                |accepted, _| accepted >= 2
            )
            .await
            .unwrap(),
            2
        );
        handle.shutdown().await.unwrap();
    };
    let ((), (), exit) = tokio::join!(clients, server, actor.run());
    assert_eq!(
        exit.service.message_service().discover_agents(),
        vec![sender, sibling.clone()]
    );
    assert_eq!(
        exit.service
            .message_service()
            .subscription(&sibling.agent_id),
        Some(&sibling_cursor)
    );
    assert_eq!(
        exit.service
            .message_service()
            .receive_subscribed(&sibling.agent_id, 12, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert_eq!(
        exit.service
            .message_service()
            .snapshot_state()
            .accepted_messages
            .len(),
        1
    );
    let _ = std::fs::remove_file(&path);
}

/// Cancellation of the listener aborts its socket tasks, but the socket-local
/// lease must retire its raw hello identity/subscription while the same actor
/// and independently owned pane registration remain available. A second owner
/// whose receiver is already closed exercises actor-side cancellation before
/// hello reply consumption, not just cleanup after a normal disconnect.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_cancellation_retires_exact_raw_lease() {
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::AsyncWriteExt;
    use tokio::net::{UnixListener, UnixStream};
    let path = std::env::temp_dir().join(format!("mez-mmp-cancel-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service();
    let pane = service.message_service_mut().register_agent(
        Some(mez_core::ids::PaneId::new('%', 1)),
        None,
        "pane",
        Vec::new(),
    );
    service
        .message_service_mut()
        .subscribe(&pane.agent_id)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let work = async {
        let listener_handle = handle.clone();
        let task = tokio::spawn(async move {
            serve_async_runtime_message_listener_concurrent(
                &listener,
                &listener_handle,
                AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
                10,
                1,
                |_, _| false,
            )
            .await
        });
        let mut peer = UnixStream::connect(&path).await.unwrap();
        let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"cancelled"}"#);
        peer.write_all(&hello).await.unwrap();
        read_replacement_welcome(&mut peer).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        let (owner, identity) = tokio::sync::watch::channel(None);
        drop(identity);
        handle
            .handle_message_input_with_owner(
                hello,
                4096,
                MessageConnection::default(),
                11,
                Some(owner),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let result = handle
                    .handle_message_input(
                        encode_mmp_body(
                            r#"{"protocol":"mmp/1","type":"discover","scope":"session"}"#,
                        ),
                        4096,
                        MessageConnection {
                            agent_id: Some(pane.agent_id.clone()),
                            delivery_cursor: None,
                        },
                        12,
                    )
                    .await
                    .unwrap();
                let (body, _) = decode_mmp_frame(&result.output, 4096).unwrap();
                if !body.contains("cancelled") {
                    assert!(body.contains(r#""role":"pane""#));
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled socket lease must release its subscription");
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(work, actor.run());
    assert_eq!(
        exit.service.message_service().discover_agents(),
        vec![pane.clone()]
    );
    assert_eq!(
        exit.service
            .message_service()
            .snapshot_state()
            .subscriptions
            .len(),
        1
    );
    let _ = std::fs::remove_file(&path);
}

/// Retained mail must not pin every fresh raw hello identity after disconnect.
/// Churn with a stationary queue exercises the production owner handoff and
/// retirement requests; both shared envelopes and acceptance receipts remain,
/// while identities that never sent retained mail require no durable floors.
#[tokio::test(flavor = "current_thread")]
async fn async_message_raw_lease_churn_bounds_retained_provenance() {
    use crate::protocol::message::encode_mmp_body;
    use mez_agent::messaging::{Envelope, MessageScope, MessageService, Recipient};
    let mut service = test_service();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", Vec::new());
    service
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "stationary-mail".to_string(),
                message_type: "send".to_string(),
                time: "message:test".to_string(),
                sender: sender.clone(),
                recipient: Recipient::Session,
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain".to_string(),
                payload: "keep shared mail".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            10,
        )
        .unwrap();
    let before = service.message_service().snapshot_state();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let work = async {
        let (owner, identity) = tokio::sync::watch::channel(None);
        for index in 0..64 {
            let result = handle.handle_message_input_with_owner(
                encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"churn","capabilities":["raw"],"objective":"bounded lease"}"#),
                4096, MessageConnection::default(), 11 + index, Some(owner.clone()),
            ).await.unwrap();
            assert_eq!(*identity.borrow(), result.connection.agent_id);
            handle
                .retire_message_connection(result.connection.agent_id.unwrap())
                .await
                .unwrap();
        }
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(work, actor.run());
    let after = exit.service.message_service().snapshot_state();
    assert_eq!(after.retained_messages, before.retained_messages);
    assert_eq!(after.accepted_messages, before.accepted_messages);
    assert_eq!(after.registered_agents, before.registered_agents);
    assert_eq!(after.subscriptions, before.subscriptions);
    assert!(
        after.retired_delivery_floors.is_empty(),
        "raw churn left {} floors",
        after.retired_delivery_floors.len()
    );
    MessageService::from_snapshot_state(&after).unwrap();
}

/// Actor shutdown owns final raw cleanup even before a cancelled socket's
/// detached Drop request arrives. Keeping the receiver alive until shutdown
/// deterministically exercises that ordering without polling discovery first.
/// An independently owned pane subscription must remain in the returned service.
#[tokio::test(flavor = "current_thread")]
async fn async_message_actor_shutdown_fences_live_raw_lease() {
    use crate::protocol::message::encode_mmp_body;
    let mut service = test_service();
    let pane = service.message_service_mut().register_agent(
        Some(mez_core::ids::PaneId::new('%', 1)),
        None,
        "pane",
        Vec::new(),
    );
    service
        .message_service_mut()
        .subscribe(&pane.agent_id)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let work = async {
        let (owner, identity) = tokio::sync::watch::channel(None);
        handle
            .handle_message_input_with_owner(
                encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"shutdown-race"}"#),
                4096,
                MessageConnection::default(),
                10,
                Some(owner),
            )
            .await
            .unwrap();
        assert!(identity.borrow().is_some());
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(work, actor.run());
    assert_eq!(
        exit.service.message_service().discover_agents(),
        vec![pane.clone()]
    );
    assert!(
        exit.service
            .message_service()
            .subscription(&pane.agent_id)
            .is_some()
    );
    assert_eq!(
        exit.service
            .message_service()
            .snapshot_state()
            .subscriptions
            .len(),
        1
    );
}

/// A broken fanout socket must not acknowledge a selected delivery, replay a
/// write, or consume another recipient's retained mail. A genuine missing
/// subscription then proves the identical fanout boundary remains fatal when
/// the failure originates in actor/shared-state selection rather than I/O.
#[tokio::test(flavor = "current_thread")]
async fn async_message_fanout_failure_preserves_cursor_and_shared_mail() {
    use mez_agent::messaging::{Envelope, MessageScope, Recipient};
    use tokio::net::UnixStream;
    let mut service = test_service();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", Vec::new());
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", Vec::new());
    let sibling = service
        .message_service_mut()
        .register_agent(None, None, "sibling", Vec::new());
    let cursor = service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    let sibling_cursor = service
        .message_service_mut()
        .subscribe(&sibling.agent_id)
        .unwrap();
    let missing =
        service
            .message_service_mut()
            .register_agent(None, None, "unsubscribed", Vec::new());
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "broken-fanout".to_string(),
        message_type: "send".to_string(),
        time: "message:test".to_string(),
        sender: sender.clone(),
        recipient: Recipient::Session,
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".to_string(),
        payload: "retained for siblings".to_string(),
        extension_fields: Vec::new(),
    };
    service
        .message_service_mut()
        .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let (client, mut stream) = UnixStream::pair().unwrap();
    client
        .into_std()
        .unwrap()
        .shutdown(std::net::Shutdown::Read)
        .unwrap();
    let work = async {
        let mut connection = MessageConnection {
            agent_id: Some(target.agent_id.clone()),
            delivery_cursor: Some(cursor.clone()),
        };
        let error = serve_async_runtime_message_connection_loop(
            &mut stream,
            &handle,
            &mut connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |_| 11,
            |_, _| false,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
        connection.agent_id = Some(missing.agent_id.clone());
        let error = serve_async_runtime_message_connection_loop(
            &mut stream,
            &handle,
            &mut connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |_| 11,
            |_, _| false,
        )
        .await
        .unwrap_err();
        assert!(
            error.message().contains("no delivery subscription"),
            "{error}"
        );
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(work, actor.run());
    assert_eq!(
        exit.service
            .message_service()
            .subscription(&target.agent_id),
        Some(&cursor)
    );
    assert_eq!(
        exit.service
            .message_service()
            .subscription(&sibling.agent_id),
        Some(&sibling_cursor)
    );
    assert_eq!(
        exit.service
            .message_service()
            .receive_subscribed(&sibling.agent_id, 12, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
}

/// Verifies async message connection flushes fanout after response write.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_flushes_fanout_after_response_write() {
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use mez_agent::messaging::{Envelope, MessageScope, Recipient};
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
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();
    let mut connection = MessageConnection {
        agent_id: Some(target.agent_id.clone()),
        delivery_cursor: service
            .message_service()
            .subscription(&target.agent_id)
            .cloned(),
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
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
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
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
            r#"{{"protocol":"mmp/1","type":"send","id":"m-idle","time":"message:client-idle","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{},"scope":"session","correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"idle hello"}}"#,
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
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
