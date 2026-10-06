//! Real QUIC stream admission without local X credentials or frontend forwarding.
//!
//! Disposable peers test route proof, bounded permits, cancellation and independent
//! sibling connections. Application bytes remain unread until authenticated; no
//! desktop, provider, or session initialization work is performed by this fixture.

use super::*;
use crate::host::outbound_endpoint::OutboundConnectionLease;
use crate::runtime::RuntimeIrohTransportPolicy;

/// Owns one endpoint and two independently connected loopback peers for isolation.
pub(super) async fn fixture() -> (
    std::path::PathBuf,
    OutboundEndpointOwner,
    iroh::Endpoint,
    OutboundConnectionLease,
    iroh::endpoint::Connection,
    OutboundConnectionLease,
    iroh::endpoint::Connection,
) {
    let root =
        std::env::temp_dir().join(format!("mez-x11-channel-{:032x}", rand::random::<u128>()));
    let owner = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![b"mez-x11-channel-test".to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let (lease, peer) = tokio::join!(
        owner.connect(server.addr(), b"mez-x11-channel-test"),
        async { server.accept().await.unwrap().await.unwrap() }
    );
    let (sibling, sibling_peer) = tokio::join!(
        owner.connect(server.addr(), b"mez-x11-channel-test"),
        async { server.accept().await.unwrap().await.unwrap() }
    );
    let lease = lease.unwrap();
    let sibling = sibling.unwrap();
    // Production grants finite stream credit only after route validation. This
    // transport-only fixture supplies the same one-channel gate explicitly.
    lease
        .connection()
        .set_max_concurrent_bi_streams(iroh::endpoint::VarInt::from_u32(1));
    sibling
        .connection()
        .set_max_concurrent_bi_streams(iroh::endpoint::VarInt::from_u32(1));
    (root, owner, server, lease, peer, sibling, sibling_peer)
}

/// Supplies exact synthetic route evidence without any real local credential.
pub(super) fn route() -> X11ForwardingResult {
    X11ForwardingResult {
        version: crate::runtime::x11::X11_FORWARDING_VERSION,
        mode: crate::runtime::x11::X11ForwardingMode::Untrusted,
        generation: 7,
        route_token: crate::runtime::x11::X11RouteToken::new([51; 32]),
    }
}

/// Exact admission consumes only the fixed preface, retains endpoint lifetime and
/// capacity, and leaves subsequent bytes untouched. Stream disposal releases its
/// permit and cannot close the independently connected sibling.
#[tokio::test]
async fn outbound_x11_channel_authenticates_without_consuming_setup() {
    let (root, owner, server, lease, peer, sibling, sibling_peer) = fixture().await;
    let route = route();
    let slots = Arc::new(Semaphore::new(1));
    let open = async {
        let (mut send, recv) = peer.open_bi().await.unwrap();
        let preface = X11StreamPreface {
            generation: route.generation,
            route_token: route.route_token.clone(),
        };
        send.write_all(&[preface.encode().as_slice(), b"setup-tail"].concat())
            .await
            .unwrap();
        (send, recv)
    };
    let (channel, (send, recv)) = tokio::join!(
        accept_channel(
            &owner,
            lease.connection(),
            &route,
            slots.clone(),
            Duration::from_secs(2)
        ),
        open
    );
    let mut channel = channel.unwrap();
    assert_eq!(slots.available_permits(), 0);
    assert!(owner.clone().begin_shutdown().is_err());
    let error = accept_channel(
        &owner,
        lease.connection(),
        &route,
        slots.clone(),
        Duration::from_secs(2),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind(), MezErrorKind::RateLimited);
    let mut tail = [0; 10];
    channel.recv.read_exact(&mut tail).await.unwrap();
    assert_eq!(&tail, b"setup-tail");
    drop(channel);
    assert_eq!(slots.available_permits(), 1);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), send.stopped())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    assert!(lease.connection().close_reason().is_none());
    let (mut sibling_send, sibling_recv) = sibling_peer.open_bi().await.unwrap();
    sibling_send.write_all(b"live").await.unwrap();
    let (client_send, mut client_recv) = sibling.connection().accept_bi().await.unwrap();
    let mut bytes = [0; 4];
    client_recv.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"live");
    drop((
        send,
        recv,
        sibling_send,
        sibling_recv,
        client_send,
        client_recv,
    ));
    drop((lease, peer, sibling, sibling_peer));
    owner
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Wrong generation, wrong proof and malformed prefaces reject with payload-free
/// diagnostics and reset the accepted stream. Each rejection releases its permit
/// while leaving the connection reusable for a separately authenticated stream.
#[tokio::test]
async fn outbound_x11_channel_rejects_foreign_and_malformed_prefaces() {
    let (root, owner, server, lease, peer, sibling, sibling_peer) = fixture().await;
    let route = route();
    let slots = Arc::new(Semaphore::new(1));
    for case in ["generation", "token", "magic"] {
        let mut preface = X11StreamPreface {
            generation: if case == "generation" { 8 } else { 7 },
            route_token: crate::runtime::x11::X11RouteToken::new(if case == "token" {
                [52; 32]
            } else {
                [51; 32]
            }),
        }
        .encode();
        if case == "magic" {
            preface[0] = 0;
        }
        let open = async {
            let (mut send, recv) = peer.open_bi().await.unwrap();
            send.write_all(&preface).await.unwrap();
            (send, recv)
        };
        let (result, (send, recv)) = tokio::join!(
            accept_channel(
                &owner,
                lease.connection(),
                &route,
                slots.clone(),
                Duration::from_secs(2)
            ),
            open
        );
        let error = result.err().unwrap();
        assert_eq!(error.kind(), MezErrorKind::Forbidden);
        assert_eq!(slots.available_permits(), 1);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), send.stopped())
                .await
                .unwrap()
                .unwrap()
                .is_some()
        );
        assert!(lease.connection().close_reason().is_none());
        assert!(sibling.connection().close_reason().is_none());
        drop((send, recv));
    }
    drop((lease, peer, sibling, sibling_peer));
    owner
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Cancelling a pending transport accept releases capacity. A timeout or waiter
/// drop after accepting a partial preface resets that stream rather than returning
/// a reusable partial channel. No wait cancellation reconnects or replays bytes.
#[tokio::test]
async fn outbound_x11_channel_cancellation_and_timeout_release_ownership() {
    let (root, owner, server, lease, peer, sibling, sibling_peer) = fixture().await;
    let route = route();
    let slots = Arc::new(Semaphore::new(1));
    {
        let mut pending = Box::pin(accept_channel(
            &owner,
            lease.connection(),
            &route,
            slots.clone(),
            Duration::from_secs(2),
        ));
        assert!(matches!(
            futures_util::poll!(&mut pending),
            std::task::Poll::Pending
        ));
        assert_eq!(slots.available_permits(), 0);
    }
    assert_eq!(slots.available_permits(), 1);
    for cancel in [false, true] {
        let (mut send, recv) = peer.open_bi().await.unwrap();
        send.write_all(b"MZX").await.unwrap();
        let mut accepting = Box::pin(accept_channel(
            &owner,
            lease.connection(),
            &route,
            slots.clone(),
            Duration::from_millis(100),
        ));
        if cancel {
            tokio::select! {
                result = &mut accepting => panic!("partial preface unexpectedly settled: {}", result.is_ok()),
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
            drop(accepting);
        } else {
            assert!(accepting.await.is_err());
        }
        assert_eq!(slots.available_permits(), 1);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), send.stopped())
                .await
                .unwrap()
                .unwrap()
                .is_some()
        );
        assert!(sibling.connection().close_reason().is_none());
        drop((send, recv));
    }
    drop((lease, peer, sibling, sibling_peer));
    owner
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}
