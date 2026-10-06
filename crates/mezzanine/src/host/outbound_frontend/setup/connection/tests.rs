//! Pinned transport qualification without application authentication or creation.
//!
//! Local Unix setup uses protected profiles; loopback QUIC peers establish only
//! transport identity. No initialize frame, credentials or terminal input is sent.

use super::*;
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::RemoteRoleCeiling;
use secrecy::SecretString;

/// Prepares through the production hello and protected-profile setup owners.
pub(super) async fn prepared(
    admission: &OutboundFrontendAdmission,
    root: &std::path::Path,
    address: iroh::EndpointAddr,
) -> (
    PreparedFrontend,
    Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
) {
    RemoteClientProfileStore::under_config_root(root)
        .save(&RemoteClientProfile {
            name: "pinned".into(),
            server_addr: address,
            role: RemoteRoleCeiling::Observer,
            scope: RemoteClientProfileScope::Host,
            device_credential: SecretString::from("owner-only-proof".to_string()),
        })
        .unwrap();
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
                "handle":frontend.handle(), "profile":"pinned", "initialize": {
                    "client_name":"frontend", "requested_version":3,
                    "requested_role":"observer", "session_intent":"host_only"
                }
            })
            .to_string(),
        ))
        .await
        .unwrap();
    (
        frontend.prepare(Duration::from_secs(2)).await.unwrap(),
        client,
    )
}

/// Independent frontends dial the same pinned peer under one identity, retaining
/// separate connection leases and untouched initialization. Dropping one closes
/// only that connection; the other remains usable without application setup.
#[tokio::test]
async fn outbound_frontend_pinned_connections_retain_independent_ownership() {
    Box::pin(qualify_independent_connections(false)).await;
}

/// Port mapping is an endpoint binding policy, not grounds to reject a protected
/// direct address. Independent leases must still share the same endpoint and
/// preserve sibling usability without acquiring replacement identity ownership.
#[tokio::test]
async fn outbound_frontend_port_mapping_preserves_shared_connections() {
    Box::pin(qualify_independent_connections(true)).await;
}

/// Drives protected setup and loopback byte transfer with the selected endpoint
/// policy. No remote application authentication or session creation is sent.
async fn qualify_independent_connections(port_mapping: bool) {
    let root = std::env::temp_dir().join(format!("mez-pinned-{:032x}", rand::random::<u128>()));
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        port_mapping,
        ..Default::default()
    };
    let endpoint = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 2, Duration::from_secs(2)).unwrap();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .alpns(vec![RuntimeIrohCompressionCodec::None.alpn().to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let (first, first_client) = prepared(&admission, &root, server.addr()).await;
    let (second, second_client) = prepared(&admission, &root, server.addr()).await;
    let connect = async {
        (
            first.connect_pinned().await.unwrap(),
            second.connect_pinned().await.unwrap(),
        )
    };
    let accept = async {
        (
            server.accept().await.unwrap().await.unwrap(),
            server.accept().await.unwrap().await.unwrap(),
        )
    };
    let ((first, second), (remote_first, remote_second)) =
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(connect, accept)
        })
        .await
        .unwrap();
    assert_eq!(remote_first.remote_id(), endpoint.endpoint_id());
    assert_eq!(remote_second.remote_id(), endpoint.endpoint_id());
    assert_eq!(
        second.compression.codec(),
        RuntimeIrohCompressionCodec::None
    );
    assert_eq!(second.prepared.initialize["session_intent"], "host_only");
    assert!(second.prepared.initialize.get("authentication").is_none());
    assert_ne!(
        first.connection.connection().stable_id(),
        second.connection.connection().stable_id()
    );
    drop(first);
    let send = async {
        let (mut tx, mut rx) = second.connection.connection().open_bi().await.unwrap();
        tx.write_all(b"transport-only").await.unwrap();
        tx.finish().unwrap();
        assert_eq!(rx.read_to_end(32).await.unwrap(), b"alive");
    };
    let reply = async {
        let (mut tx, mut rx) = remote_second.accept_bi().await.unwrap();
        assert_eq!(rx.read_to_end(32).await.unwrap(), b"transport-only");
        tx.write_all(b"alive").await.unwrap();
        tx.finish().unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(send, reply) })
        .await
        .unwrap();
    drop(second);
    drop(first_client);
    drop(second_client);
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// A route-less profile rejects before a handshake and releases the consumed
/// frontend slot. No fallback endpoint or guessed network destination is used.
#[tokio::test]
async fn outbound_frontend_pinned_route_rejection_releases_ownership() {
    let root =
        std::env::temp_dir().join(format!("mez-pinned-reject-{:032x}", rand::random::<u128>()));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 1, Duration::from_secs(2)).unwrap();
    let (prepared, client) = prepared(
        &admission,
        &root,
        iroh::EndpointAddr::new(iroh::SecretKey::generate().public()),
    )
    .await;
    let error = prepared.connect_pinned().await.err().unwrap();
    assert_eq!(error.kind(), MezErrorKind::Forbidden);
    assert_eq!(admission.slots.available_permits(), 1);
    drop(client);
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
