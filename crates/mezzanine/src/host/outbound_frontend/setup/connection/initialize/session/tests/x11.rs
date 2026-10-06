//! Explicit X11 admission without local credentials or forwarding activation.
//!
//! Synthetic pinned peers qualify exact route evidence and original request
//! ownership. Ordinary transitions must reject offers before application writes;
//! this fixture neither opens a local X server nor delivers route proof on IPC.

use super::*;
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::RemoteRoleCeiling;
use base64::Engine as _;

mod dispatch;
mod frontend;

/// Returns only a synthetic fake-cookie offer, never a real local credential.
fn offer(mode: &str) -> serde_json::Value {
    serde_json::json!({"version":crate::runtime::x11::X11_FORWARDING_VERSION,
        "mode":mode,"auth_protocol":"MIT-MAGIC-COOKIE-1",
        "fake_cookie_base64":base64::engine::general_purpose::STANDARD.encode([17_u8;16]),
        "takeover":false})
}

/// Primary intent and an explicit offer are necessary but not sufficient:
/// correlated returned capability, exact trust mode and route proof are required.
/// Unrequested authority cannot enter the existing redraw-only session summary.
#[test]
fn outbound_session_x11_requires_explicit_primary_and_exact_route() {
    let server = iroh::SecretKey::generate().public();
    let mut input = serde_json::json!({"client_name":"fixture","requested_version":3,
        "requested_role":"primary","session_intent":"attach",
        "session_target":{"session_id":"$1"},"x11_forwarding":offer("untrusted")});
    let params = initialize_params_from_json(&input.to_string()).unwrap();
    validate_x11_mode(&params).unwrap();
    let mut reply = response(server);
    assert!(validate_session_response(&reply.to_string(), server, &params).is_err());
    reply["result"]["capabilities"] = serde_json::json!({"features":{"x11_forwarding":true}});
    reply["result"]["x11_forwarding"] = serde_json::json!({
        "version":crate::runtime::x11::X11_FORWARDING_VERSION,"mode":"untrusted","generation":7,
        "route_token_base64":base64::engine::general_purpose::STANDARD.encode([51_u8;32])});
    let summary = validate_session_response(&reply.to_string(), server, &params).unwrap();
    assert!(summary.get("x11_forwarding").is_none());
    input.as_object_mut().unwrap().remove("x11_forwarding");
    let plain = initialize_params_from_json(&input.to_string()).unwrap();
    assert!(validate_x11_mode(&plain).is_err());
    assert!(validate_session_response(&reply.to_string(), server, &plain).is_err());
    input["requested_role"] = serde_json::json!("observer");
    input["x11_forwarding"] = offer("untrusted");
    let observer = initialize_params_from_json(&input.to_string()).unwrap();
    assert!(validate_x11_mode(&observer).is_err());
    reply["result"]["x11_forwarding"]["mode"] = serde_json::json!("trusted");
    assert!(validate_session_response(&reply.to_string(), server, &params).is_err());
}

/// A separately selected admission sends one original-key initialize and retains
/// exact mode/generation/proof on its connection only. Missing capability or mode
/// drift retires ownership; ordinary initialization rejects before opening a
/// stream. No local IPC reply, relay worker, reconnect or initialize replay occurs.
#[tokio::test]
async fn outbound_session_x11_admission_retains_route_without_forwarding() {
    for case in ["trusted", "untrusted", "missing", "mode-drift", "ordinary"] {
        let root =
            std::env::temp_dir().join(format!("mez-x11-admission-{:032x}", rand::random::<u128>()));
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
                device_credential: secrecy::SecretString::from("synthetic-owner-proof".to_string()),
            })
            .unwrap();
        let (mut prepared, mut local) = create_frontend(&admission, "x11-fixture").await;
        prepared
            .initialize
            .as_object_mut()
            .unwrap()
            .remove("event_stream_version");
        let requested = if case == "trusted" {
            "trusted"
        } else {
            "untrusted"
        };
        prepared.initialize["x11_forwarding"] = offer(requested);
        let (reset_observed, reset_barrier) = tokio::sync::oneshot::channel();
        let peer = async {
            let connection = server.accept().await.unwrap().await.unwrap();
            assert_eq!(connection.remote_id(), endpoint.endpoint_id());
            if case == "ordinary" {
                assert!(
                    connection.accept_bi().await.is_err(),
                    "ordinary path must emit no application request"
                );
                return;
            }
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
            assert_eq!(request["params"]["idempotency_key"], "create-x11-fixture");
            assert_eq!(request["params"]["x11_forwarding"]["mode"], requested);
            let mut reply = response(server.id());
            reply["result"]["capabilities"] =
                serde_json::json!({"features":{"x11_forwarding":case != "missing"}});
            reply["result"]["x11_forwarding"] = serde_json::json!({
                "version":crate::runtime::x11::X11_FORWARDING_VERSION,
                "mode":if case == "mode-drift" { "trusted" } else { requested },"generation":7,
                "route_token_base64":base64::engine::general_purpose::STANDARD.encode([51_u8;32])});
            bridge
                .stream_mut()
                .write_all(&crate::control::encode_control_body(&reply.to_string()))
                .await
                .unwrap();
            if matches!(case, "trusted" | "untrusted") {
                // No manual credit override: production admission must grant
                // credit only after validating the correlated route response.
                let (mut send, recv) = connection.open_bi().await.unwrap();
                let preface = crate::runtime::x11::X11StreamPreface {
                    generation: 7,
                    route_token: crate::runtime::x11::X11RouteToken::new([51; 32]),
                };
                send.write_all(&preface.encode()).await.unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_secs(2), send.stopped())
                        .await
                        .unwrap()
                        .unwrap()
                        .is_some()
                );
                assert!(
                    connection.close_reason().is_none(),
                    "channel disposal must not retire its parent session"
                );
                let _ = reset_observed.send(());
                drop((send, recv));
            }
            assert!(
                read_exact_frame(bridge.stream_mut()).await.is_err(),
                "no initialize replay or relay request expected"
            );
            connection.closed().await;
        };
        let client = async {
            let connected = prepared.connect_pinned().await.unwrap();
            let result = if case == "ordinary" {
                connected.initialize_session().await
            } else {
                connected.initialize_x11_session().await
            };
            if matches!(case, "trusted" | "untrusted") {
                let initialized = result.unwrap();
                let route = initialized.x11_route.as_ref().unwrap();
                assert_eq!(route.mode.as_str(), requested);
                assert_eq!(route.generation, 7);
                assert_eq!(route.route_token.as_bytes(), &[51_u8; 32]);
                assert!(initialized.summary.get("x11_forwarding").is_none());
                let channel = initialized.accept_x11_channel().await.unwrap();
                assert_eq!(
                    initialized.x11_slots.available_permits(),
                    policy.x11.max_connections_per_route - 1
                );
                drop(channel);
                assert_eq!(
                    initialized.x11_slots.available_permits(),
                    policy.x11.max_connections_per_route
                );
                // Keep the parent until the peer observes its channel reset.
                // A control-stream EOF below then fences parent retirement.
                reset_barrier.await.unwrap();
                drop(initialized);
            } else {
                assert!(result.is_err());
            }
            assert!(
                local.next().await.is_none(),
                "route proof must not enter local IPC"
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
}
