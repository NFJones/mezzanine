//! Explicit broker dispatch through production local discovery and channel opening.
//!
//! A synthetic pinned peer supplies correlated session/route and terminal-view
//! replies. The real supervising listener owns publication and channel cleanup;
//! the attaching-client adapter substitutes only the frozen local fixture cookie.
//! No physical X server, provider work or ordinary CLI activation is involved.

use super::*;
use crate::host::outbound_frontend::{OutboundFrontendListener, client::OutboundFrontendClient};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Builds a fixed little-endian setup containing only a synthetic fake cookie.
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

/// Authored X11 intent must traverse actual hello/setup dispatch, exact pinned
/// initialization, initial snapshot, closed discovery and the production opener.
/// Only the local X peer receives the substituted credential. The control owner
/// stays usable after channel FIN, then its retirement removes dedicated socket
/// publication and releases identity ownership without replaying initialization.
#[tokio::test]
async fn outbound_x11_dispatch_discovers_and_opens_owned_local_relay() {
    let root = std::env::temp_dir().join(format!("mez-xdispatch-{:032x}", rand::random::<u128>()));
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        ..Default::default()
    };
    let endpoint = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
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
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 2, policy.setup_timeout).unwrap();
    let stop = Arc::new(tokio::sync::Notify::new());
    let stopped = stop.clone();
    let (done, complete) = tokio::sync::oneshot::channel();
    let remote = async {
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
        let mut bridge = IrohCompressionBridge::spawn(recv, send, compression, BODY_LIMIT).unwrap();
        let request = read_exact_frame(bridge.stream_mut()).await.unwrap();
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["params"]["idempotency_key"], "original-x11-create");
        assert_eq!(request["params"]["x11_forwarding"], offer("untrusted"));
        let mut reply = response(server.id());
        reply["result"]["capabilities"] = serde_json::json!({"features":{"x11_forwarding":true}});
        reply["result"]["x11_forwarding"] = serde_json::json!({"version":2,"mode":"untrusted","generation":7,
            "route_token_base64":base64::engine::general_purpose::STANDARD.encode([51_u8;32])});
        bridge
            .stream_mut()
            .write_all(&crate::control::encode_control_body(&reply.to_string()))
            .await
            .unwrap();
        let view = read_exact_frame(bridge.stream_mut()).await.unwrap();
        let view: serde_json::Value = serde_json::from_str(&view).unwrap();
        assert_eq!(view["method"], "terminal/view");
        let view_reply = serde_json::json!({"jsonrpc":"2.0","id":view["id"],"result":{
            "presentation_ids":[],"view":{"role":"primary","client_size":{"columns":80,"rows":24},
                "lines":["retained"],"line_style_spans":[[]],"cursor":{"row":0,"column":0,"visible":false},"output_modes":{}}
        }});
        bridge
            .stream_mut()
            .write_all(&crate::control::encode_control_body(
                &view_reply.to_string(),
            ))
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
        send.write_all(&[setup(), b"ping".to_vec()].concat())
            .await
            .unwrap();
        send.finish().unwrap();
        assert_eq!(recv.read_to_end(1024).await.unwrap(), b"pong");
        assert!(connection.close_reason().is_none());
        done.send(()).unwrap();
        assert!(
            read_exact_frame(bridge.stream_mut()).await.is_err(),
            "no remote discovery, health or replay request expected"
        );
        connection.closed().await;
    };
    let clients = async {
        let client = OutboundFrontendClient::connect(&root, policy.setup_timeout)
            .await
            .unwrap();
        let (session, lines) = client.start_session("creator", serde_json::json!({
            "client_name":"dispatch-fixture","requested_version":3,"requested_role":"primary",
            "session_intent":"create","idempotency_key":"original-x11-create","detach_primary_on_disconnect":true,
            "x11_forwarding":offer("untrusted"),
            "client":{"name":"dispatch-fixture","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}
        }), 80, 24, policy.setup_timeout).await.unwrap();
        assert_eq!(lines, ["retained"]);
        let (session, name) = session.discover_x11(policy.setup_timeout).await.unwrap();
        let name = name.unwrap();
        let opener = session.x11_channel_opener(&name, 1).unwrap();
        let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = local_listener.local_addr().unwrap().port();
        let display = crate::cli::x11::resolve_local_x11_display(&format!(
            "127.0.0.1:{}",
            port.checked_sub(6000).unwrap()
        ))
        .unwrap();
        let forwarder = crate::cli::x11::X11ClientForwarder::new_for_test(
            display,
            crate::runtime::x11::X11Cookie::new([17; 16]),
            crate::runtime::x11::X11Cookie::new([52; 16]),
        );
        let channel = opener.open(policy.x11.setup_timeout).await.unwrap();
        assert_eq!(channel.occurrence(), 1);
        let local = async {
            let (mut socket, _) = local_listener.accept().await.unwrap();
            let mut header = [0; 48];
            socket.read_exact(&mut header).await.unwrap();
            let mut expected = setup();
            expected[32..48].fill(52);
            assert_eq!(header.as_slice(), expected);
            let mut bytes = Vec::new();
            socket.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, b"ping");
            socket.write_all(b"pong").await.unwrap();
            socket.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(
            Box::pin(forwarder.relay_broker_stream(channel, policy.x11.setup_timeout)),
            Box::pin(local)
        );
        result.unwrap();
        complete.await.unwrap();
        let (session, connected, _) = session
            .sample_transport_health(policy.setup_timeout)
            .await
            .unwrap();
        assert!(
            connected,
            "channel completion must preserve control ownership"
        );
        drop((session, opener));
        // Fence dedicated publication cleanup before outer cancellation. A
        // sibling hello alone cannot prove retirement in a two-slot listener.
        tokio::time::timeout(policy.setup_timeout, async {
            while root.join(&name).exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("control retirement must remove dedicated publication");
        // Readiness remains usable after the original pipeline retires.
        let ready = OutboundFrontendClient::connect(&root, policy.setup_timeout)
            .await
            .unwrap();
        drop(ready);
        stop.notify_one();
        name
    };
    let (served, (), name) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(
            Box::pin(listener.serve(async move { stopped.notified().await })),
            Box::pin(remote),
            Box::pin(clients)
        )
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 2);
    assert!(!root.join(name).exists());
    drop(listener);
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
