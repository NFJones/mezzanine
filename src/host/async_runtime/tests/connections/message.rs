//! Async-runtime tests owned by message behavior.

use super::super::*;

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

/// Verifies async message connection flushes fanout after response write.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_flushes_fanout_after_response_write() {
    use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
    use mez_agent::messaging::{Envelope, Recipient};
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
