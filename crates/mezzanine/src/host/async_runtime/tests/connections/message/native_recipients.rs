//! Framed raw discovery-to-native-recipient parsing on an isolated Unix pair.
//!
//! The existing fixture adapter accepts a connection established by lower raw
//! hello and trusted pre-actor project binding. Discovery and send cross real
//! framing and actor ingress. This is not production listener UID admission,
//! wire-authorized project binding, or live native-provider lifecycle coverage.

use super::*;
use crate::protocol::message::{decode_mmp_frame, encode_mmp_body};
use mez_agent::messaging::{ProjectScopeId, Recipient, SenderIdentity, dispatch_mmp_body};
use mez_core::ids::StableId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod boundaries;

/// Exchanges one complete bounded frame without assuming a socket read matches
/// frame boundaries. Only the sender owns this stream, and no fixture mail is
/// addressed back to it, so unsolicited delivery frames are not expected here.
async fn exchange_raw_frame(stream: &mut UnixStream, body: serde_json::Value) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        stream
            .write_all(&encode_mmp_body(&body.to_string()))
            .await
            .unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 512];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "raw connection closed before its response frame");
            bytes.extend_from_slice(&chunk[..read]);
            if let Ok((body, consumed)) = decode_mmp_frame(&bytes, 4096) {
                assert_eq!(
                    consumed,
                    bytes.len(),
                    "fixture expects one response per request"
                );
                return serde_json::from_str(&body).unwrap();
            }
            assert!(bytes.len() <= 8192, "bounded fixture response framing");
        }
    })
    .await
    .expect("isolated raw frame exchange must finish")
}

/// A generated raw sender discovers an independently owned native identity and
/// sends to the exact returned ID through Unix framing and the runtime actor.
/// Both frames are served and the actor is shut down BEFORE the desired ACK is
/// asserted, so the baseline parser refusal remains a causal oracle, not a
/// dangling server or timeout. Native receipt is inspected without inventing a
/// native raw connection; its cursor advances only through explicit acknowledgement.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_discovery_native_recipient_round_trip() {
    let mut service = test_service();
    let mut connection = MessageConnection::default();
    let welcome: serde_json::Value = serde_json::from_str(&dispatch_mmp_body(
        r#"{"protocol":"mmp/1","type":"hello","role":"raw-fixture"}"#,
        service.message_service_mut(),
        &mut connection,
        10,
    ))
    .unwrap();
    assert_eq!(welcome["type"], "welcome");
    let sender = connection.agent_id.clone().unwrap();
    assert!(StableId::parse('a', sender.as_str()).is_some());
    let project = ProjectScopeId::from_canonical_root_bytes(b"/fixture/framed-raw-native");
    service
        .message_service_mut()
        .rebind_agent_project_scope(&sender, Some(project.clone()))
        .unwrap();
    let native = service
        .message_service_mut()
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque("agent-%1").unwrap(),
                project_scope: Some(project),
                pane_id: Some(StableId::new('%', 1)),
                window_id: Some(StableId::new('@', 1)),
                role: Some("native-fixture".into()),
                capabilities: vec!["agent-harness".into()],
                objective: None,
            },
            10,
        )
        .unwrap();
    service
        .message_service_mut()
        .subscribe_from_retained_start(&native.agent_id)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let client = async {
        let discovery = exchange_raw_frame(
            &mut client_stream,
            serde_json::json!({
                "protocol": "mmp/1", "type": "discover", "role": "native-fixture",
            }),
        )
        .await;
        assert_eq!(discovery["type"], "discover_result");
        assert_eq!(discovery["scope"], "project");
        let agents = discovery["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        let discovered = agents[0]["agent_id"].as_str().unwrap();
        assert_eq!(discovered, native.agent_id.as_str());
        let response = exchange_raw_frame(&mut client_stream, serde_json::json!({
            "protocol": "mmp/1", "id": "framed-native-discovered", "type": "send", "time": "fixture:20",
            "sender": {"agent_id": sender.as_str()}, "recipient": {"agent_id": discovered},
            "correlation_id": null, "ttl_ms": null, "content_type": "text/plain; charset=utf-8",
            "payload": "isolated framed native evidence",
        })).await;
        client_stream.shutdown().await.unwrap();
        response
    };
    let server = async {
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 1).unwrap(),
            |index| 20 + index,
            |served, _| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert_eq!(connection.agent_id.as_ref(), Some(&sender));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let (response, (), mut exit) = tokio::join!(client, server, actor.run());
    assert_eq!(
        response["type"], "ack",
        "framed discovered exact ID must address the native recipient: {response}"
    );
    assert_eq!(response["status"], "accepted");
    assert_eq!(response["message_id"], "framed-native-discovered");
    assert_eq!(response["queued_recipients"], 1);
    let batch = exit
        .service
        .message_service()
        .receive_subscribed(&native.agent_id, 30, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(
        exit.service
            .message_service()
            .subscription(&native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    let message = &batch.messages[0];
    assert_eq!(message.envelope.id, "framed-native-discovered");
    assert_eq!(message.envelope.sender.agent_id, sender);
    assert_eq!(
        message.envelope.recipient,
        Recipient::Agent(native.agent_id.clone())
    );
    assert_eq!(message.envelope.payload, "isolated framed native evidence");
    exit.service
        .message_service_mut()
        .advance_subscription(&native.agent_id, message.sequence)
        .unwrap();
    assert!(
        exit.service
            .message_service()
            .receive_subscribed(&native.agent_id, 31, 10)
            .unwrap()
            .messages
            .is_empty()
    );
}
