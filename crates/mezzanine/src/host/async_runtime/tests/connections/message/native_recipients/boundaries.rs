//! Framed exact-address rejection, size bounds and fresh native registration.
//!
//! Connections retain real lower hello bindings and trusted local membership.
//! These tests do not invent raw native authentication or exercise providers,
//! production listener UID admission, private socket paths, or raw lease cleanup.

use super::*;

/// Creates the same fixture authority either through ordinary lower native
/// registration or the existing fresh runtime identity helper. The explicit
/// project rebind is trusted setup, never an envelope-supplied claim.
fn framed_boundary_fixture(
    runtime_native: bool,
) -> (
    RuntimeSessionService,
    MessageConnection,
    AgentId,
    SenderIdentity,
) {
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
    let project = ProjectScopeId::from_canonical_root_bytes(b"/fixture/framed-raw-boundary");
    service
        .message_service_mut()
        .rebind_agent_project_scope(&sender, Some(project.clone()))
        .unwrap();
    let native = if runtime_native {
        service
            .ensure_runtime_message_identity(
                "agent-%1",
                Some(StableId::new('%', 1)),
                "native-fixture",
                &["agent-harness"],
                10,
            )
            .unwrap()
    } else {
        service
            .message_service_mut()
            .ensure_agent_identity(
                SenderIdentity {
                    agent_id: AgentId::opaque("agent-%1").unwrap(),
                    project_scope: Some(project.clone()),
                    pane_id: Some(StableId::new('%', 1)),
                    window_id: Some(StableId::new('@', 1)),
                    role: Some("native-fixture".into()),
                    capabilities: vec!["agent-harness".into()],
                    objective: None,
                },
                10,
            )
            .unwrap()
    };
    let native = service
        .message_service_mut()
        .rebind_agent_project_scope(&native.agent_id, Some(project))
        .unwrap();
    service
        .message_service_mut()
        .subscribe_from_retained_start(&native.agent_id)
        .unwrap();
    (service, connection, sender, native)
}

/// Builds one complete frame body without interpreting the selected ID.
fn framed_send(sender: &AgentId, id: &str, target: &str) -> serde_json::Value {
    serde_json::json!({
        "protocol": "mmp/1", "id": id, "type": "send", "time": "fixture:20",
        "sender": {"agent_id": sender.as_str()}, "recipient": {"agent_id": target},
        "correlation_id": null, "ttl_ms": null, "content_type": "text/plain; charset=utf-8",
        "payload": "framed boundary evidence",
    })
}

/// Missing, retired, foreign-project and label-like targets produce the same
/// framed unavailable result. A long opaque ID remains inside the frame limit:
/// it is an unavailable address, not a newly invented ID-length rejection.
/// Every expected rejection is inspected only after transport/actor cleanup.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_native_unavailable_addresses_keep_framed_authority() {
    let (mut service, mut connection, sender, native) = framed_boundary_fixture(false);
    let mut retired = native.clone();
    retired.agent_id = AgentId::opaque("agent-retired").unwrap();
    retired.pane_id = None;
    retired.window_id = None;
    service
        .message_service_mut()
        .ensure_agent_identity(retired.clone(), 10)
        .unwrap();
    assert!(
        service
            .message_service_mut()
            .retire_agent_identity(&retired.agent_id)
    );
    let mut foreign = native.clone();
    foreign.agent_id = AgentId::opaque("agent-foreign").unwrap();
    foreign.pane_id = None;
    foreign.window_id = None;
    foreign.project_scope = Some(ProjectScopeId::from_canonical_root_bytes(
        b"/fixture/framed-raw-foreign",
    ));
    service
        .message_service_mut()
        .ensure_agent_identity(foreign, 10)
        .unwrap();
    let before = service.message_service().snapshot_state();
    let targets = [
        "agent-missing".to_string(),
        "agent-retired".to_string(),
        "agent-foreign".to_string(),
        "native-fixture".to_string(),
        format!("agent-{}", "x".repeat(1024)),
    ];
    let count = u64::try_from(targets.len()).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let client = async {
        let mut responses = Vec::new();
        for (index, target) in targets.iter().enumerate() {
            let body = framed_send(&sender, &format!("unavailable-{index}"), target);
            assert!(body.to_string().len() < 4096);
            responses.push(exchange_raw_frame(&mut client_stream, body).await);
        }
        client_stream.shutdown().await.unwrap();
        responses
    };
    let server = async {
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 1).unwrap(),
            |index| 20 + index,
            |served, _| served >= count,
        )
        .await
        .unwrap();
        assert_eq!(served, count);
        handle.shutdown().await.unwrap();
    };
    let (responses, (), exit) = tokio::join!(client, server, actor.run());
    assert_eq!(responses.len(), targets.len());
    for response in &responses {
        assert_eq!(response["type"], "error");
        assert_eq!(response["error"]["code"], "undeliverable");
        assert_eq!(response, &responses[0]);
    }
    assert_eq!(exit.service.message_service().snapshot_state(), before);
    assert!(exit.service.agent_turn_ledger().turns().is_empty());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_peer_message_turn_count(native.agent_id.as_str()),
        0
    );
}

/// The existing configured frame limit rejects an oversized complete request
/// before raw recipient dispatch. This is a framing error, not an agent-ID byte
/// limit. A peer write may observe early close; actor and message state remain
/// available for cleanup and no envelope, receipt or cursor is accepted.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_native_recipient_preserves_frame_size_limit() {
    let (service, mut connection, sender, native) = framed_boundary_fixture(false);
    let before = service.message_service().snapshot_state();
    let mut body = framed_send(&sender, "oversized-frame", native.agent_id.as_str());
    body["payload"] = serde_json::json!("x".repeat(4097));
    assert!(body.to_string().len() > 4096);
    let input = encode_mmp_body(&body.to_string());
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let client = async {
        let written = client_stream.write_all(&input).await;
        let _ = client_stream.shutdown().await;
        written
    };
    let server = async {
        let result = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 1).unwrap(),
            |_| 20,
            |_, _| false,
        )
        .await;
        let _ = server_stream.shutdown().await;
        handle.shutdown().await.unwrap();
        result
    };
    let (_peer_write, result, exit) = tokio::join!(client, server, actor.run());
    let error = result.unwrap_err();
    assert!(
        error.message().contains("exceeds configured limit"),
        "{error}"
    );
    assert_eq!(exit.service.message_service().snapshot_state(), before);
    assert!(exit.service.agent_turn_ledger().turns().is_empty());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
}

/// A fresh native identity created by the existing runtime helper is likewise
/// addressable by its exact discovery ID. There is intentionally no shell
/// session or provider service: acceptance cannot consume peer-turn budget,
/// publish a native receive receipt, or auto-advance the native subscription.
/// This does not test role/capability refresh or native provider execution.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_recipient_composes_with_fresh_runtime_native_registration() {
    let (service, mut connection, sender, native) = framed_boundary_fixture(true);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let client = async {
        let discovery = exchange_raw_frame(
            &mut client_stream,
            serde_json::json!({"protocol": "mmp/1", "type": "discover", "role": "native-fixture"}),
        )
        .await;
        assert_eq!(discovery["scope"], "project");
        let agents = discovery["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        let target = agents[0]["agent_id"].as_str().unwrap();
        assert_eq!(target, native.agent_id.as_str());
        let response = exchange_raw_frame(
            &mut client_stream,
            framed_send(&sender, "runtime-native-address", target),
        )
        .await;
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
        handle.shutdown().await.unwrap();
    };
    let (response, (), exit) = tokio::join!(client, server, actor.run());
    assert_eq!(response["status"], "accepted", "{response}");
    assert_eq!(response["message_id"], "runtime-native-address");
    assert_eq!(response["queued_recipients"], 1);
    let batch = exit
        .service
        .message_service()
        .receive_subscribed(&native.agent_id, 30, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(
        batch.messages[0].envelope.recipient,
        Recipient::Agent(native.agent_id.clone())
    );
    assert_eq!(
        exit.service
            .message_service()
            .subscription(&native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(exit.service.agent_turn_ledger().turns().is_empty());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_peer_message_turn_count(native.agent_id.as_str()),
        0
    );
}
