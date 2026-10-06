//! Supervision owns control and stalled dedicated channels without detached tasks.
//!
//! Synthetic pinned initialization and private Unix streams exercise concurrency
//! and cancellation only; no local X server, real credential or CLI activation.

use super::*;
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::RemoteRoleCeiling;
use base64::Engine as _;

/// A silent dedicated peer and a ready peer waiting on remote preface must not
/// prevent exact-session health replies. Cancellation closes both channels and
/// control, removes publication and releases every local/remote admission slot.
/// The remote peer observes no second initialization or health application call.
#[tokio::test]
async fn outbound_x11_supervisor_keeps_control_live_and_cancels_owned_channels() {
    Box::pin(qualify_supervision("matched")).await;
}

/// A smaller dedicated listener must wait for its own slot without retiring
/// the control session or spinning on an immediate capacity error.
#[tokio::test]
async fn outbound_x11_supervisor_preserves_control_with_smaller_listener_pool() {
    Box::pin(qualify_supervision("smaller")).await;
}

/// Reservations held outside the supervisor consume real remote capacity. A
/// pending local peer must not cause whole-session retirement while they remain.
#[tokio::test]
async fn outbound_x11_supervisor_preserves_control_with_external_reservations() {
    Box::pin(qualify_supervision("external")).await;
}

/// Exercises independent local/remote capacity while preserving the same
/// control, channel, cancellation and publication assertions in every case.
async fn qualify_supervision(case: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::env::temp_dir().join(format!("mez-xsuper-{:032x}", rand::random::<u128>()));
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        x11: crate::runtime::RuntimeIrohX11Policy {
            max_connections_per_route: 2,
            setup_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        ..Default::default()
    };
    let endpoint = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 1, Duration::from_secs(2)).unwrap();
    let peer = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![RuntimeIrohCompressionCodec::None.alpn().to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    RemoteClientProfileStore::under_config_root(&root)
        .save(&RemoteClientProfile {
            name: "fixture".into(),
            server_addr: peer.addr(),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: secrecy::SecretString::from("synthetic-proof".to_string()),
        })
        .unwrap();
    let (server, local) = tokio::net::UnixStream::pair().unwrap();
    let mut local = Framed::new(local, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
    local
        .send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({"protocol":PROTOCOL}).to_string(),
        ))
        .await
        .unwrap();
    let frontend = admission.admit(server).await.unwrap();
    local.next().await.unwrap().unwrap();
    let handle = frontend.handle().clone();
    local.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
        "handle":handle,"profile":"fixture","initialize":{
            "client_name":"fixture","requested_version":3,"requested_role":"primary",
            "session_intent":"attach","session_target":{"session_id":"$1"},
            "x11_forwarding":{"version":2,"mode":"untrusted","auth_protocol":"MIT-MAGIC-COOKIE-1",
                "fake_cookie_base64":base64::engine::general_purpose::STANDARD.encode([17_u8;16]),"takeover":false},
            "client":{"name":"fixture","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}
        }
    }).to_string())).await.unwrap();
    let prepared = frontend.prepare(Duration::from_secs(2)).await.unwrap();
    let remote = async {
        let connection = peer.accept().await.unwrap().await.unwrap();
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
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request).unwrap()["method"],
            "control/initialize"
        );
        let body = serde_json::json!({"jsonrpc":"2.0","id":REQUEST_ID,"result":{
            "selected_version":3,"granted_role":"primary","host":{"endpoint_id":peer.id().to_string()},
            "session":{"id":"$1"},"client":{"id":"c1"},
            "lease":{"lease_id":"lease-one","session_id":"$1","state":"active"},
            "capabilities":{"features":{"x11_forwarding":true}},
            "x11_forwarding":{"version":2,"mode":"untrusted","generation":7,
                "route_token_base64":base64::engine::general_purpose::STANDARD.encode([51_u8;32])}
        }}).to_string();
        bridge
            .stream_mut()
            .write_all(&crate::control::encode_control_body(&body))
            .await
            .unwrap();
        assert!(
            read_exact_frame(bridge.stream_mut()).await.is_err(),
            "control must not replay initialization"
        );
        connection.closed().await;
    };
    let owner = async {
        let initialized = prepared
            .connect_pinned()
            .await
            .unwrap()
            .initialize_x11_session()
            .await
            .unwrap();
        let summary = initialized.summary.clone();
        let slots = initialized.x11_slots.clone();
        let occurrences = initialized.x11_occurrence.clone();
        let source = initialized.x11_relay_source().unwrap();
        let mut held = Vec::new();
        if case == "external" {
            held.push(source.reserve().unwrap());
            held.push(source.reserve().unwrap());
        }
        let baseline = occurrences.load(std::sync::atomic::Ordering::Relaxed);
        let listener =
            X11FrontendListener::bind(endpoint.clone(), if case == "smaller" { 1 } else { 2 })
                .unwrap();
        let path = listener.socket_path().unwrap().to_path_buf();
        let stop = Arc::new(tokio::sync::Notify::new());
        let stopped = stop.clone();
        let supervise =
            initialized.supervise_x11(listener, async move { stopped.notified().await });
        let clients = async {
            local
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({"operation":"x11-discovery","handle":handle}).to_string(),
                ))
                .await
                .unwrap();
            let discovery = local.next().await.unwrap().unwrap();
            assert_eq!(discovery.content_type, CONTENT_TYPE);
            let discovery: serde_json::Value = serde_json::from_str(&discovery.body).unwrap();
            assert_eq!(
                discovery,
                serde_json::json!({"handle":handle,"session":summary,
                "version":1,"socket_name":path.file_name().unwrap().to_str().unwrap()})
            );
            assert_eq!(
                occurrences.load(std::sync::atomic::Ordering::Relaxed),
                baseline,
                "discovery must allocate no occurrence"
            );
            assert_eq!(
                slots.available_permits(),
                if case == "external" { 0 } else { 2 },
                "discovery must consume no remote permit"
            );
            let mut silent = tokio::net::UnixStream::connect(&path).await.unwrap();
            if case == "external" {
                local
                    .send(ProtocolFrame::new(
                        CONTENT_TYPE,
                        serde_json::json!({"operation":"health","handle":handle}).to_string(),
                    ))
                    .await
                    .unwrap();
                let reply = local.next().await.unwrap().unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&reply.body).unwrap()["connected"],
                    true
                );
                assert_eq!(
                    occurrences.load(std::sync::atomic::Ordering::Relaxed),
                    baseline
                );
                drop(held);
            }
            // The shared checked allocator fences actual acceptance, not a sleep.
            tokio::time::timeout(Duration::from_secs(2), async {
                while occurrences.load(std::sync::atomic::Ordering::Relaxed) != baseline + 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let ready = tokio::net::UnixStream::connect(&path).await.unwrap();
            let mut ready = Framed::new(ready, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
            if case == "smaller" {
                local
                    .send(ProtocolFrame::new(
                        CONTENT_TYPE,
                        serde_json::json!({"operation":"health","handle":handle}).to_string(),
                    ))
                    .await
                    .unwrap();
                let reply = local.next().await.unwrap().unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&reply.body).unwrap()["connected"],
                    true
                );
                assert_eq!(
                    occurrences.load(std::sync::atomic::Ordering::Relaxed),
                    baseline + 1
                );
                silent.shutdown().await.unwrap();
            }
            ready
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({
                        "protocol":"mez-outbound-x11/2","handle":handle,"session":summary
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            let reply = ready.next().await.unwrap().unwrap();
            let reply: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
            assert_eq!(reply["ready"], true);
            assert_eq!(reply["protocol"], "mez-outbound-x11/2");
            assert_eq!(reply["occurrence"], baseline + 2);
            assert_eq!(
                slots.available_permits(),
                if case == "smaller" { 1 } else { 0 }
            );
            for _ in 0..2 {
                local
                    .send(ProtocolFrame::new(
                        CONTENT_TYPE,
                        serde_json::json!({"operation":"health","handle":handle}).to_string(),
                    ))
                    .await
                    .unwrap();
                let response = local.next().await.unwrap().unwrap();
                let response: serde_json::Value = serde_json::from_str(&response.body).unwrap();
                assert_eq!(response["handle"], serde_json::to_value(&handle).unwrap());
                assert_eq!(response["session"], summary);
                assert_eq!(response["connected"], true);
            }
            stop.notify_one();
            assert!(local.next().await.is_none());
            assert!(ready.next().await.is_none());
            let mut bytes = Vec::new();
            silent.read_to_end(&mut bytes).await.unwrap();
            assert!(bytes.is_empty());
        };
        let (result, ()) = tokio::join!(Box::pin(supervise), Box::pin(clients));
        result.unwrap();
        assert!(!path.exists());
        assert_eq!(slots.available_permits(), 2);
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(Box::pin(remote), Box::pin(owner));
    })
    .await
    .unwrap();
    assert_eq!(admission.slots.available_permits(), 1);
    drop(local);
    drop(admission);
    endpoint
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    peer.close().await;
    std::fs::remove_dir_all(root).unwrap();
}
