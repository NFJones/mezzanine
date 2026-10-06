//! Session-owned dedicated handoff and relay, without listener or desktop effects.
//!
//! Synthetic pinned initialization grants one exact route. The session derives
//! its cookie/codec and allocates occurrences; a dedicated Unix pair carries the
//! closed handshake and raw bytes. No caller can substitute route proof or local
//! X credentials, and the ordinary control IPC remains free of relay payloads.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Constructs a fixed valid synthetic fake-cookie setup, not a real X credential.
fn setup() -> Vec<u8> {
    let mut bytes = vec![0; 48];
    bytes[0] = b'l';
    bytes[2..4].copy_from_slice(&11_u16.to_le_bytes());
    bytes[6..8].copy_from_slice(&18_u16.to_le_bytes());
    bytes[8..10].copy_from_slice(&16_u16.to_le_bytes());
    bytes[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
    bytes[32..48].fill(17);
    bytes
}

/// The retained session must bind its own handle/summary and advance occurrences
/// across a rejected handshake and a successful relay. Exact fake-cookie setup,
/// ping/pong and both FINs survive on dedicated IPC, while the original control
/// stream receives neither route proof nor X11 bytes. Exhaustion and detached
/// owners reject without accepting another remote stream or reusing an occurrence.
#[tokio::test]
async fn outbound_x11_frontend_uses_retained_session_and_nonreused_occurrences() {
    let root =
        std::env::temp_dir().join(format!("mez-x11-composed-{:032x}", rand::random::<u128>()));
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        x11: crate::runtime::RuntimeIrohX11Policy {
            setup_timeout: Duration::from_millis(100),
            ..Default::default()
        },
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
            device_credential: secrecy::SecretString::from("synthetic-owner-proof".to_string()),
        })
        .unwrap();
    let (mut prepared, mut local) = create_frontend(&admission, "composed-fixture").await;
    prepared
        .initialize
        .as_object_mut()
        .unwrap()
        .remove("event_stream_version");
    prepared.initialize["x11_forwarding"] = offer("untrusted");
    let (completed, completion) = tokio::sync::oneshot::channel();
    let remote = async {
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
        assert_eq!(
            request["params"]["idempotency_key"],
            "create-composed-fixture"
        );
        let mut reply = response(server.id());
        reply["result"]["capabilities"] = serde_json::json!({"features":{"x11_forwarding":true}});
        reply["result"]["x11_forwarding"] = serde_json::json!({
            "version":crate::runtime::x11::X11_FORWARDING_VERSION,"mode":"untrusted","generation":7,
            "route_token_base64":base64::engine::general_purpose::STANDARD.encode([51_u8;32])});
        bridge
            .stream_mut()
            .write_all(&crate::control::encode_control_body(&reply.to_string()))
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(
            &crate::runtime::x11::X11StreamPreface {
                generation: 7,
                route_token: crate::runtime::x11::X11RouteToken::new([51; 32]),
            }
            .encode(),
        )
        .await
        .unwrap();
        send.write_all(&setup()).await.unwrap();
        send.write_all(b"ping").await.unwrap();
        send.finish().unwrap();
        let mut bytes = Vec::new();
        recv.read_to_end(1024)
            .await
            .map(|value| bytes = value)
            .unwrap();
        assert_eq!(bytes, b"pong");
        assert!(connection.close_reason().is_none());
        completed.send(()).unwrap();
        assert!(
            read_exact_frame(bridge.stream_mut()).await.is_err(),
            "no control replay or X11 payload expected"
        );
        connection.closed().await;
    };
    let client =
        async {
            let mut initialized = prepared
                .connect_pinned()
                .await
                .unwrap()
                .initialize_x11_session()
                .await
                .unwrap();
            let handle = initialized.connected.prepared.frontend.handle().clone();
            let summary = initialized.summary.clone();
            let (bad, peer) = tokio::net::UnixStream::pair().unwrap();
            let mut peer = Framed::new(peer, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
            peer.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
            "protocol":"mez-outbound-x11/1","handle":handle,"session":summary,"occurrence":2,
        }).to_string())).await.unwrap();
            assert!(Box::pin(initialized.relay_x11_frontend(bad)).await.is_err());
            assert_eq!(initialized.x11_occurrence, 1);
            assert!(peer.next().await.is_none());
            let (dedicated, peer) = tokio::net::UnixStream::pair().unwrap();
            let frontend = async {
                let mut peer = Framed::new(peer, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
                peer.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
                "protocol":"mez-outbound-x11/1","handle":handle,"session":summary,"occurrence":2,
            }).to_string())).await.unwrap();
                let ready = peer.next().await.unwrap().unwrap();
                let ready: serde_json::Value = serde_json::from_str(&ready.body).unwrap();
                assert_eq!(
                    ready,
                    serde_json::json!({"protocol":"mez-outbound-x11/1","handle":handle,
                "session":summary,"occurrence":2,"ready":true})
                );
                let parts = peer.into_parts();
                assert!(parts.write_buf.is_empty());
                let (read, mut write) = tokio::io::split(parts.io);
                // Readiness and setup can be coalesced. Preserve every byte
                // decoded ahead of the ready frame before reading the socket.
                let mut raw = std::io::Cursor::new(parts.read_buf).chain(read);
                let mut header = [0; 48];
                raw.read_exact(&mut header).await.unwrap();
                assert_eq!(header.as_slice(), setup());
                let mut bytes = Vec::new();
                raw.read_to_end(&mut bytes).await.unwrap();
                assert_eq!(bytes, b"ping");
                write.write_all(b"pong").await.unwrap();
                write.shutdown().await.unwrap();
            };
            let (result, ()) = tokio::join!(
                Box::pin(initialized.relay_x11_frontend(dedicated)),
                Box::pin(frontend)
            );
            result.unwrap();
            assert_eq!(initialized.x11_occurrence, 2);
            assert_eq!(
                initialized.x11_slots.available_permits(),
                policy.x11.max_connections_per_route
            );
            completion.await.unwrap();
            initialized.x11_occurrence = u64::MAX;
            let (stream, _peer) = tokio::net::UnixStream::pair().unwrap();
            assert_eq!(
                initialized
                    .relay_x11_frontend(stream)
                    .await
                    .unwrap_err()
                    .kind(),
                MezErrorKind::Conflict
            );
            assert_eq!(initialized.x11_occurrence, u64::MAX);
            initialized.detached = true;
            let (stream, _peer) = tokio::net::UnixStream::pair().unwrap();
            assert_eq!(
                initialized
                    .relay_x11_frontend(stream)
                    .await
                    .unwrap_err()
                    .kind(),
                MezErrorKind::Conflict
            );
            drop(initialized);
            assert!(local.next().await.is_none());
        };
    tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(Box::pin(remote), Box::pin(client));
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
