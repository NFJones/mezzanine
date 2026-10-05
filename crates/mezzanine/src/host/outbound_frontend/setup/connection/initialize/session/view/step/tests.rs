//! Closed input request and runtime acknowledgement qualification.
//!
//! Validation occurs before remote mutation. Accepted input count is not proof
//! of process delivery; unknown response fields and credentials are not projected.

use super::*;

/// A synthetic pinned peer reads the exact mutation before the local waiter is
/// cancelled. No acknowledgement is sent: disposal must close that connection,
/// release frontend capacity, and never emit a second mutation. This qualifies
/// the transport failure window, not execution or billing by a real provider.
#[tokio::test]
async fn outbound_input_cancel_after_peer_read_never_replays_mutation() {
    use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
    use crate::security::remote::RemoteRoleCeiling;
    let root =
        std::env::temp_dir().join(format!("mez-input-cancel-{:032x}", rand::random::<u128>()));
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        ..Default::default()
    };
    let endpoint = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 1, Duration::from_secs(2)).unwrap();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .alpns(vec![RuntimeIrohCompressionCodec::None.alpn().to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    RemoteClientProfileStore::under_config_root(&root)
        .save(&RemoteClientProfile {
            name: "fixture".into(),
            server_addr: server.addr(),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: secrecy::SecretString::from("synthetic-proof".to_string()),
        })
        .unwrap();
    let (local_server, local_client) = tokio::net::UnixStream::pair().unwrap();
    let mut local = Framed::new(local_client, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
    local
        .send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({"protocol":PROTOCOL}).to_string(),
        ))
        .await
        .unwrap();
    let frontend = admission.admit(local_server).await.unwrap();
    local.next().await.unwrap().unwrap();
    let handle = frontend.handle().clone();
    local.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
        "handle":handle,"profile":"fixture","initialize":{
            "client_name":"fixture","requested_version":3,"requested_role":"primary",
            "session_intent":"create","idempotency_key":"fixture-create",
            "client":{"name":"fixture","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}
        }
    }).to_string())).await.unwrap();
    let prepared = frontend.prepare(Duration::from_secs(2)).await.unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let peer = async {
        let connection = server.accept().await.unwrap().await.unwrap();
        let (send, recv) = connection.accept_bi().await.unwrap();
        let compression = IrohCompressionPolicy::new(
            RuntimeIrohCompressionCodec::None,
            512,
            3,
            BODY_LIMIT + 1024,
        )
        .unwrap();
        let mut bridge = IrohCompressionBridge::spawn(recv, send, compression, BODY_LIMIT).unwrap();
        let init = read_exact_frame(bridge.stream_mut()).await.unwrap();
        let init: serde_json::Value = serde_json::from_str(&init).unwrap();
        assert_eq!(init["params"]["authentication"]["token"], "synthetic-proof");
        bridge.stream_mut().write_all(&crate::control::encode_control_body(&serde_json::json!({
            "jsonrpc":"2.0","id":init["id"],"result":{
                "selected_version":3,"granted_role":"primary","host":{"endpoint_id":server.id().to_string()},
                "session":{"id":"$1"},"client":{"id":"c1"},
                "lease":{"lease_id":"lease-fixture","session_id":"$1","state":"active"},"x11_forwarding":null
            }
        }).to_string())).await.unwrap();
        let step = read_exact_frame(bridge.stream_mut()).await.unwrap();
        let step: serde_json::Value = serde_json::from_str(&step).unwrap();
        assert_eq!(step["method"], "terminal/step");
        assert_eq!(
            step["params"]["idempotency_key"],
            "exact-original-input-key"
        );
        assert_eq!(step["params"]["input_bytes"], serde_json::json!([97, 98]));
        assert_eq!(step["params"]["render"], false);
        accepted_tx.send(()).unwrap();
        // Closing without a response is intentionally ambiguous. A second
        // complete request would prove replay and must fail this assertion.
        assert!(read_exact_frame(bridge.stream_mut()).await.is_err());
        connection.closed().await;
    };
    let client = async {
        let session = prepared
            .connect_pinned()
            .await
            .unwrap()
            .initialize_session()
            .await
            .unwrap();
        local
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({
                    "operation":"step","handle":handle,"columns":80,"rows":24,
                    "idempotency_key":"exact-original-input-key","input_bytes":[97,98]
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let mut delivery = Box::pin(session.deliver_view());
        tokio::select! {
            result = &mut delivery => panic!("unacknowledged input unexpectedly settled: {}", result.is_ok()),
            result = accepted_rx => result.unwrap(),
        }
        drop(delivery);
        assert!(local.next().await.is_none());
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(peer, client)
    })
    .await
    .unwrap();
    drop(local);
    drop(admission);
    endpoint
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Exact handles and primary settlement are required before bounded input is
/// forwarded. Unknown envelope fields cannot select another method or session.
#[test]
fn outbound_input_request_requires_exact_primary_and_budget() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let body = serde_json::json!({"operation":"step","handle":handle,
        "columns":80,"rows":24,"idempotency_key":"exact-input","input_bytes":[97,98]});
    let request: StepRequest = serde_json::from_value(body.clone()).unwrap();
    validate_request(
        &request,
        &handle,
        &serde_json::json!({"granted_role":"primary"}),
    )
    .unwrap();
    assert!(
        validate_request(
            &request,
            &handle,
            &serde_json::json!({"granted_role":"observer"})
        )
        .is_err()
    );
    for (pointer, value) in [
        ("/operation", serde_json::json!("initialize")),
        ("/handle/generation", serde_json::json!(2)),
        ("/columns", serde_json::json!(0)),
        ("/rows", serde_json::json!(4097)),
        ("/idempotency_key", serde_json::json!("")),
        ("/input_bytes", serde_json::json!(vec![0_u8; 513])),
    ] {
        let mut changed = body.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let request: StepRequest = serde_json::from_value(changed).unwrap();
        assert!(
            validate_request(
                &request,
                &handle,
                &serde_json::json!({"granted_role":"primary"})
            )
            .is_err()
        );
    }
    let mut foreign = body;
    foreign["session_id"] = serde_json::json!("$2");
    assert!(serde_json::from_value::<StepRequest>(foreign).is_err());
}

/// Correlation and exact runtime input acceptance must precede a content-free
/// acknowledgement. Lifecycle flags are preserved rather than invented from
/// input size, and forwarded-byte or credential metadata never crosses IPC.
#[test]
fn outbound_input_acknowledgement_is_exact_and_content_free() {
    let original = serde_json::json!({"jsonrpc":"2.0","id":STEP_ID,"result":{
        "input_bytes":2,"client_detached":false,"session_terminated":false,
        "application":{"forwarded_bytes":0},"device_credential":"not-projected"}});
    assert_eq!(
        project_acknowledgement(&original.to_string(), 2).unwrap(),
        serde_json::json!({"input_bytes":2,"client_detached":false,"session_terminated":false})
    );
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/input_bytes", serde_json::json!(1)),
        ("/result/client_detached", serde_json::Value::Null),
        ("/result/session_terminated", serde_json::json!("false")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let error = project_acknowledgement(&changed.to_string(), 2).unwrap_err();
        assert!(!error.message().contains("not-projected"));
    }
}
