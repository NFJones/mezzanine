//! Correlated host-only settlement and exact bounded frame consumption.
//!
//! Synthetic loopback responses qualify owner transport plumbing, not live host
//! authorization or session creation. No credentials are emitted on local IPC.

use super::*;
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};

/// Real host dispatch must accept the endpoint-bound durable device proof and
/// deny its later revocation. Pairing is seeded through the protected test store,
/// not provider work; both attempts remain host-only with no session allocation.
#[tokio::test]
async fn outbound_host_initialize_real_trust_accepts_then_rejects_revocation() {
    use crate::host::iroh::HostIrohRuntime;
    use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
    use std::os::unix::fs::PermissionsExt;

    let root =
        std::env::temp_dir().join(format!("mez-owner-trust-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host_root = root.join("host");
    let client_root = root.join("client");
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        ..Default::default()
    };
    let host = HostIrohRuntime::bind(
        &host_root,
        RuntimeIrohTransportPolicy {
            enabled: true,
            ..policy.clone()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let endpoint = OutboundEndpointOwner::bind(&client_root, &policy)
        .await
        .unwrap();
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 1, Duration::from_secs(2)).unwrap();
    let trust = RemoteTrustStore::under_host_config_root(&host_root).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invitation = trust
        .create_invitation(host.endpoint_id(), RemoteRoleCeiling::Observer, 600, now)
        .unwrap();
    let redemption = trust
        .redeem_invitation(
            &invitation.token,
            host.endpoint_id(),
            &endpoint.endpoint_id().to_string(),
            "owner fixture",
            RequestedRole::Observer,
            now,
        )
        .unwrap();
    RemoteClientProfileStore::under_config_root(&client_root)
        .save(&RemoteClientProfile {
            name: "real-host".into(),
            server_addr: host.endpoint_addr().unwrap(),
            role: RemoteRoleCeiling::Observer,
            scope: RemoteClientProfileScope::Host,
            device_credential: redemption.device_credential.clone(),
        })
        .unwrap();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let serve = host.serve(async move { server_stop.notified().await });
    let client_work = async {
        for revoked in [false, true] {
            if revoked {
                trust
                    .revoke_record(&redemption.record.id, Some("fixture revocation"), now)
                    .unwrap();
            }
            let (server, client) = tokio::net::UnixStream::pair().unwrap();
            let mut client = Framed::new(client, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
            client
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({"protocol":PROTOCOL}).to_string(),
                ))
                .await
                .unwrap();
            let frontend = admission.admit(server).await.unwrap();
            client.next().await.unwrap().unwrap();
            client
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({
                        "handle":frontend.handle(), "profile":"real-host", "initialize": {
                            "client_name":"owner fixture", "requested_version":3,
                            "requested_role":"observer", "session_intent":"host_only"
                        }
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            let prepared = frontend.prepare(Duration::from_secs(2)).await.unwrap();
            let connected = prepared.connect_pinned().await.unwrap();
            let initialized = connected.initialize_host_only().await;
            if revoked {
                assert_eq!(initialized.err().unwrap().kind(), MezErrorKind::Forbidden);
            } else {
                let initialized = initialized.unwrap();
                assert_eq!(
                    initialized.summary,
                    serde_json::json!({"selected_version":3,"granted_role":"observer","host_only":true})
                );
                drop(initialized);
            }
            assert_eq!(admission.slots.available_permits(), 1);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), client.next())
                    .await
                    .is_ok_and(|result| result.is_none()),
                "local frontend receives no remote proof or reply"
            );
        }
        stop.notify_one();
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(serve, client_work)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 2);
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

/// Produces the minimal synthetic host-only response for one pinned identity.
fn response(server: iroh::EndpointId) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":REQUEST_ID,"result":{
        "selected_version":3,"granted_role":"observer",
        "host":{"endpoint_id":server.to_string()},"session":null,"lease":null,
        "client":null,"capabilities":{"features":{"host_only":true}}
    }})
}

/// Wrong correlation, authority, scope or returned proof must fail without
/// leaking payloads into diagnostics. Unknown metadata is not projected.
#[test]
fn outbound_host_initialize_settlement_is_correlated_and_allowlisted() {
    let server = iroh::SecretKey::generate().public();
    let original = response(server);
    let mut extra = original.clone();
    extra["result"]["untrusted"] = serde_json::json!("do-not-export");
    assert_eq!(
        validate_host_response(&extra.to_string(), server).unwrap(),
        serde_json::json!({"selected_version":3,"granted_role":"observer","host_only":true})
    );
    for (pointer, value) in [
        ("/id", serde_json::json!("different")),
        ("/result/selected_version", serde_json::json!(2)),
        ("/result/granted_role", serde_json::json!("primary")),
        (
            "/result/host/endpoint_id",
            serde_json::json!(iroh::SecretKey::generate().public().to_string()),
        ),
        ("/result/session", serde_json::json!({"id":"$1"})),
        ("/result/lease", serde_json::json!({"lease_id":"lease-one"})),
        ("/result/client", serde_json::json!({"id":"c1"})),
        (
            "/result/capabilities/features/host_only",
            serde_json::json!(false),
        ),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            validate_host_response(&changed.to_string(), server).is_err(),
            "{pointer}"
        );
    }
    let mut proof = original;
    proof["result"]["device_credential"] = serde_json::json!("private-proof");
    let error = validate_host_response(&proof.to_string(), server).unwrap_err();
    assert!(!error.message().contains("private-proof"));
}

/// Reading one frame leaves a coalesced second frame untouched. Oversized body,
/// header and duplicate Content-Length reject before unbounded consumption.
#[tokio::test]
async fn outbound_host_initialize_frame_reads_are_exact_and_bounded() {
    let (mut reader, mut writer) = tokio::io::duplex(32768);
    let first = encode_frame(&ProtocolFrame::new(CONTROL_CONTENT_TYPE, "first"));
    let second = encode_frame(&ProtocolFrame::new(CONTROL_CONTENT_TYPE, "second"));
    writer.write_all(&[first, second].concat()).await.unwrap();
    assert_eq!(read_exact_frame(&mut reader).await.unwrap(), "first");
    assert_eq!(read_exact_frame(&mut reader).await.unwrap(), "second");
    for bytes in [
        format!("Content-Length: {}\r\n\r\n", BODY_LIMIT + 1).into_bytes(),
        b"Content-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_vec(),
        vec![b'x'; 8192],
    ] {
        let (mut reader, mut writer) = tokio::io::duplex(32768);
        writer.write_all(&bytes).await.unwrap();
        assert!(read_exact_frame(&mut reader).await.is_err());
    }
}

/// The complete consumed hello/setup/connect/initialize path sends one private
/// proof only to the pinned synthetic peer and retains the subsequent frame.
/// The local frontend receives no raw initialize response or device proof.
#[tokio::test]
async fn outbound_host_initialize_keeps_proof_owner_side_and_trailing_frame() {
    let root = std::env::temp_dir().join(format!("mez-host-init-{:032x}", rand::random::<u128>()));
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
    let (prepared, mut local) =
        super::super::tests::prepared(&admission, &root, server.addr()).await;
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
        let request = read_exact_frame(bridge.stream_mut()).await.unwrap();
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["id"], REQUEST_ID);
        assert_eq!(request["params"]["session_intent"], "host_only");
        assert_eq!(
            request["params"]["authentication"]["token"],
            "owner-only-proof"
        );
        bridge
            .stream_mut()
            .write_all(
                &[
                    encode_frame(&ProtocolFrame::new(
                        CONTROL_CONTENT_TYPE,
                        response(server.id()).to_string(),
                    )),
                    encode_frame(&ProtocolFrame::new(CONTROL_CONTENT_TYPE, "trailing")),
                ]
                .concat(),
            )
            .await
            .unwrap();
        (connection, bridge)
    };
    let initialize = async {
        prepared
            .connect_pinned()
            .await
            .unwrap()
            .initialize_host_only()
            .await
            .unwrap()
    };
    let (mut initialized, (peer_connection, peer_bridge)) =
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(initialize, peer)
        })
        .await
        .unwrap();
    assert_eq!(
        initialized.summary,
        serde_json::json!({"selected_version":3,"granted_role":"observer","host_only":true})
    );
    assert_eq!(
        read_exact_frame(initialized.bridge.stream_mut())
            .await
            .unwrap(),
        "trailing"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), local.next())
            .await
            .is_err()
    );
    assert!(endpoint.clone().begin_shutdown().is_err());
    drop(initialized);
    drop(local);
    drop(peer_bridge);
    drop(peer_connection);
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}
