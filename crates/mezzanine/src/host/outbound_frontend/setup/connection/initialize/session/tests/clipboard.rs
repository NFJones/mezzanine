//! Explicit clipboard admission is separate from redraw-only supervision.
//!
//! Synthetic pinned peers qualify capability rejection and retained item ownership,
//! not host clipboard effects, live provider work or ordinary CLI activation.

use super::*;
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::RemoteRoleCeiling;

/// Version two requires explicit primary intent and true returned capability.
/// Missing, false or malformed capability cannot authorize a clipboard reader;
/// the existing supervisor transition remains restricted to absent/v1 events.
#[test]
fn outbound_session_clipboard_requires_primary_and_capability() {
    let server = iroh::SecretKey::generate().public();
    let params = initialize_params_from_json(
        &serde_json::json!({
            "client_name":"fixture","requested_version":3,"requested_role":"primary",
            "session_intent":"attach","session_target":{"session_id":"$1"},
            "event_stream_version":2
        })
        .to_string(),
    )
    .unwrap();
    validate_event_mode(&params, true).unwrap();
    assert!(validate_event_mode(&params, false).is_err());
    let mut original = response(server);
    assert!(validate_session_response(&original.to_string(), server, &params).is_err());
    original["result"]["capabilities"]["features"]["client_clipboard_write"] =
        serde_json::json!(true);
    assert!(validate_session_response(&original.to_string(), server, &params).is_ok());
    for capability in [
        serde_json::json!(false),
        serde_json::Value::Null,
        serde_json::json!("true"),
        serde_json::json!(1),
    ] {
        let mut invalid = original.clone();
        invalid["result"]["capabilities"]["features"]["client_clipboard_write"] = capability;
        assert!(validate_session_response(&invalid.to_string(), server, &params).is_err());
    }
    for (role, version) in [("observer", 2), ("primary", 1), ("primary", 3)] {
        let params = initialize_params_from_json(
            &serde_json::json!({
                "client_name":"fixture","requested_version":3,"requested_role":role,
                "session_intent":"attach","session_target":{"session_id":"$1"},
                "event_stream_version":version
            })
            .to_string(),
        )
        .unwrap();
        assert!(validate_event_mode(&params, true).is_err());
    }
}

/// The consumed owner sends one original-key authenticated initialize, then
/// accepts the exact v2 preface only after validated capability. Typed consumption
/// yields one complete effect, without copying proof or content into local IPC.
/// Denied capability retires the connection rather than replaying initialization.
#[tokio::test]
async fn outbound_session_clipboard_admission_retains_exact_connection() {
    for (capable, deliver) in [(false, false), (true, false), (true, true)] {
        let root = std::env::temp_dir().join(format!(
            "mez-clipboard-admission-{:032x}",
            rand::random::<u128>()
        ));
        let policy = RuntimeIrohTransportPolicy {
            compression_codecs: vec![RuntimeIrohCompressionCodec::None],
            ..Default::default()
        };
        let endpoint = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
        let admission =
            OutboundFrontendAdmission::new(endpoint.clone(), 1, Duration::from_secs(2)).unwrap();
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![RuntimeIrohCompressionCodec::None.alpn().to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .clear_address_lookup()
            .bind()
            .await
            .unwrap();
        RemoteClientProfileStore::under_config_root(&root)
            .save(&RemoteClientProfile {
                name: "creator".into(),
                server_addr: server.addr(),
                role: RemoteRoleCeiling::Primary,
                scope: RemoteClientProfileScope::Host,
                device_credential: secrecy::SecretString::from("synthetic-proof".to_string()),
            })
            .unwrap();
        let (mut prepared, mut local) = create_frontend(&admission, "clipboard-fixture").await;
        prepared.initialize["event_stream_version"] = serde_json::json!(2);
        let peer = async {
            let connection = server.accept().await.unwrap().await.unwrap();
            assert_eq!(connection.remote_id(), endpoint.endpoint_id());
            let (send, recv) = connection.accept_bi().await.unwrap();
            let compression = IrohCompressionPolicy::new(
                RuntimeIrohCompressionCodec::None,
                512,
                3,
                BODY_LIMIT + 1024,
            )
            .unwrap();
            let mut bridge =
                IrohCompressionBridge::spawn(recv, send, compression, BODY_LIMIT).unwrap();
            let request = read_exact_frame(bridge.stream_mut()).await.unwrap();
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "control/initialize");
            assert_eq!(request["params"]["event_stream_version"], 2);
            assert_eq!(
                request["params"]["idempotency_key"],
                "create-clipboard-fixture"
            );
            assert_eq!(
                request["params"]["authentication"]["token"],
                "synthetic-proof"
            );
            let mut reply = response(server.id());
            reply["result"]["capabilities"]["features"]["client_clipboard_write"] =
                serde_json::json!(capable);
            bridge
                .stream_mut()
                .write_all(&crate::control::encode_control_body(&reply.to_string()))
                .await
                .unwrap();
            let mut events = if capable {
                let mut events = connection.open_uni().await.unwrap();
                events
                    .write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE)
                    .await
                    .unwrap();
                for (method, params) in [
                    (
                        "begin",
                        serde_json::json!({"sequence":1,"total_bytes":3,"chunks":1}),
                    ),
                    (
                        "chunk",
                        serde_json::json!({"sequence":1,"index":0,"data_base64":"6Zuq"}),
                    ),
                    ("commit", serde_json::json!({"sequence":1})),
                ] {
                    events.write_all(&crate::control::encode_control_body(&serde_json::json!({
                        "jsonrpc":"2.0","method":format!("client/clipboard.{method}"),"params":params
                    }).to_string())).await.unwrap();
                }
                Some(events)
            } else {
                None
            };
            assert!(
                read_exact_frame(bridge.stream_mut()).await.is_err(),
                "initialization must not replay"
            );
            connection.closed().await;
            drop(events.take());
        };
        let client = async {
            let connected = prepared.connect_pinned().await.unwrap();
            let initialized = connected.initialize_clipboard_session().await;
            if capable {
                let mut initialized = initialized.unwrap();
                assert_eq!(initialized.summary["client_id"], "c1");
                if deliver {
                    let handle = initialized.connected.prepared.frontend.handle().clone();
                    let expected_summary = initialized.summary.clone();
                    let summary = serde_json::from_value(initialized.summary.clone()).unwrap();
                    let mut receiver =
                        crate::host::outbound_frontend::clipboard_wire::ClipboardReceiver::new(
                            handle.clone(),
                            summary,
                        );
                    *local.codec_mut() = ProtocolFrameCodec::new(BODY_LIMIT).unwrap();
                    for index in 0..3 {
                        local
                            .send(ProtocolFrame::new(
                                CONTENT_TYPE,
                                serde_json::json!({
                                    "operation":"items","handle":handle,"wait_ms":250
                                })
                                .to_string(),
                            ))
                            .await
                            .unwrap();
                        let response = async {
                            if index < 2 {
                                let frame = local.next().await.unwrap().unwrap();
                                let value: serde_json::Value =
                                    serde_json::from_str(&frame.body).unwrap();
                                assert_eq!(value["kind"], "redraw");
                                assert_eq!(value["action"], "none");
                                assert_eq!(value["handle"], serde_json::to_value(&handle).unwrap());
                                assert_eq!(value["session"], expected_summary);
                                None
                            } else {
                                let mut content = None;
                                for _ in 0..3 {
                                    let frame = local.next().await.unwrap().unwrap();
                                    assert!(frame.body.len() <= BODY_LIMIT);
                                    let item = receiver.apply(&frame).unwrap();
                                    if item.is_some() {
                                        assert!(content.is_none());
                                        content = item;
                                    }
                                }
                                content
                            }
                        };
                        let (delivered, content) =
                            tokio::join!(initialized.deliver_view(), response);
                        initialized = delivered.unwrap();
                        assert_eq!(
                            content.as_deref(),
                            if index == 2 { Some("雪") } else { None }
                        );
                    }
                    assert_eq!(initialized.clipboard_transfer, 1);
                    drop(initialized);
                    assert!(local.next().await.is_none());
                    return;
                }
                assert!(
                    initialized.next_event().await.is_err(),
                    "redraw-only consumer must not discard content"
                );
                for _ in 0..2 {
                    assert!(matches!(
                        initialized.next_event_item().await.unwrap(),
                        Some(
                            crate::host::outbound_frontend::events::OutboundEventItem::Redraw(_, _)
                        )
                    ));
                }
                assert!(matches!(initialized.next_event_item().await.unwrap(),
                    Some(crate::host::outbound_frontend::events::OutboundEventItem::Clipboard(content)) if content == "雪"));
                drop(initialized);
            } else {
                assert_eq!(initialized.err().unwrap().kind(), MezErrorKind::Forbidden);
            }
            assert!(
                local.next().await.is_none(),
                "local IPC must receive neither device proof nor clipboard effect"
            );
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(peer, client)
        })
        .await
        .unwrap();
        assert_eq!(admission.slots.available_permits(), 1);
        drop(local);
        drop(admission);
        endpoint.begin_shutdown().unwrap().finish().await.unwrap();
        server.close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
