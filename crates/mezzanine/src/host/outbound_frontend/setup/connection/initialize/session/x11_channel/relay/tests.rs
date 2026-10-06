//! Relay uses synthetic frontend bytes, not a desktop X server or real cookie.
use super::*;

/// Invalid fake credentials and mixed setup/application records must expose no
/// frontend bytes. Partial setup timeout and cancellation after validated setup
/// must reset the exact stream, release capacity, and preserve sibling transport.
/// Cancellation uses completed frontend setup as a barrier, not a scheduling sleep.
#[tokio::test]
async fn outbound_x11_relay_failures_and_cancellation_reset_only_owned_stream() {
    use crate::runtime::RuntimeIrohCompressionCodec as Codec;
    for case in [
        "cookie",
        "compressed-cookie",
        "trailing",
        "partial",
        "cancel",
    ] {
        let (root, owner, server, lease, peer, sibling, sibling_peer) =
            super::super::tests::fixture().await;
        let route = super::super::tests::route();
        let slots = Arc::new(Semaphore::new(1));
        let codec = if matches!(case, "compressed-cookie" | "trailing") {
            Codec::ZstdStream
        } else {
            Codec::None
        };
        let compression = IrohCompressionPolicy::new(codec, 1, 3, 1024 * 1024).unwrap();
        let open = async {
            let (mut send, recv) = peer.open_bi().await.unwrap();
            send.write_all(
                &X11StreamPreface {
                    generation: route.generation,
                    route_token: route.route_token.clone(),
                }
                .encode(),
            )
            .await
            .unwrap();
            if case == "partial" {
                send.write_all(b"l\0\x0b").await.unwrap();
            } else {
                let mut bytes = setup(if matches!(case, "cookie" | "compressed-cookie") {
                    18
                } else {
                    17
                });
                if case == "trailing" {
                    bytes.extend_from_slice(b"not-setup");
                }
                X11IrohEncoder::new(compression)
                    .unwrap()
                    .write_setup(&mut send, &bytes, None)
                    .await
                    .unwrap();
            }
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
            open,
        );
        let channel = channel.unwrap();
        let (relay_side, mut frontend) = tokio::io::duplex(4096);
        let cookie = X11Cookie::new([17; 16]);
        if case == "cancel" {
            let mut relay =
                Box::pin(channel.relay(relay_side, compression, &cookie, Duration::from_secs(2)));
            let mut bytes = [0; 48];
            tokio::select! {
                result = &mut relay => panic!("relay settled before cancellation barrier: {}", result.is_ok()),
                result = frontend.read_exact(&mut bytes) => { result.unwrap(); },
            }
            assert_eq!(bytes.as_slice(), setup(17));
            assert_eq!(slots.available_permits(), 0);
            drop(relay);
        } else {
            let error = Box::pin(channel.relay(
                relay_side,
                compression,
                &cookie,
                Duration::from_millis(100),
            ))
            .await
            .unwrap_err();
            assert_eq!(
                error.kind(),
                if case == "partial" {
                    MezErrorKind::InvalidState
                } else {
                    MezErrorKind::Forbidden
                }
            );
            assert!(!error.message().contains("not-setup"));
        }
        let mut exposed = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), frontend.read_to_end(&mut exposed))
            .await
            .unwrap()
            .unwrap();
        assert!(
            exposed.is_empty(),
            "no rejected setup or cancelled tail may be exposed"
        );
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
        drop((send, recv, lease, peer, sibling, sibling_peer));
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
}

/// Constructs one exact little-endian MIT cookie setup with fixed fixture bytes.
fn setup(cookie: u8) -> Vec<u8> {
    let mut bytes = vec![0; 48];
    bytes[0] = b'l';
    bytes[2..4].copy_from_slice(&11_u16.to_le_bytes());
    bytes[6..8].copy_from_slice(&18_u16.to_le_bytes());
    bytes[8..10].copy_from_slice(&16_u16.to_le_bytes());
    bytes[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
    bytes[32..48].fill(cookie);
    bytes
}

/// All negotiated codecs retain exact setup and subsequent bidirectional data.
/// Setup fake proof remains unchanged for attaching-client substitution. Both
/// half-closes complete and release channel capacity without closing the parent
/// or independently connected sibling; the fixture sends no real credentials.
#[tokio::test]
async fn outbound_x11_relay_preserves_setup_and_half_closes_across_codecs() {
    use crate::runtime::RuntimeIrohCompressionCodec as Codec;
    for codec in [
        Codec::None,
        Codec::Zstd,
        Codec::Lz4,
        Codec::ZstdStream,
        Codec::Lz4Stream,
    ] {
        let (root, owner, server, lease, peer, sibling, sibling_peer) =
            super::super::tests::fixture().await;
        let route = super::super::tests::route();
        let slots = Arc::new(Semaphore::new(1));
        let compression = IrohCompressionPolicy::new(codec, 1, 3, 1024 * 1024).unwrap();
        let (relay_side, mut frontend) = tokio::io::duplex(4096);
        let remote = async {
            let (mut send, mut recv) = peer.open_bi().await.unwrap();
            send.write_all(
                &X11StreamPreface {
                    generation: route.generation,
                    route_token: route.route_token.clone(),
                }
                .encode(),
            )
            .await
            .unwrap();
            let mut encoder = X11IrohEncoder::new(compression).unwrap();
            encoder
                .write_setup(&mut send, &setup(17), None)
                .await
                .unwrap();
            encoder
                .relay(&mut b"ping".as_slice(), &mut send, None)
                .await
                .unwrap();
            send.finish().unwrap();
            let mut decoder = X11IrohDecoder::new(compression).unwrap();
            let (mut sink, mut read) = tokio::io::duplex(4096);
            let copy = async {
                let result = decoder.relay(&mut recv, &mut sink, None).await;
                drop(sink);
                result
            };
            let collect = async {
                let mut bytes = Vec::new();
                read.read_to_end(&mut bytes).await.unwrap();
                bytes
            };
            let (result, bytes) = tokio::join!(copy, collect);
            result.unwrap();
            assert_eq!(bytes, b"pong");
        };
        let relay = async {
            let channel = accept_channel(
                &owner,
                lease.connection(),
                &route,
                slots.clone(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            channel
                .relay(
                    relay_side,
                    compression,
                    &X11Cookie::new([17; 16]),
                    Duration::from_secs(2),
                )
                .await
                .unwrap();
        };
        let client = async {
            let mut header = [0; 48];
            frontend.read_exact(&mut header).await.unwrap();
            assert_eq!(header.as_slice(), setup(17));
            let mut payload = Vec::new();
            frontend.read_to_end(&mut payload).await.unwrap();
            assert_eq!(payload, b"ping");
            frontend.write_all(b"pong").await.unwrap();
            frontend.shutdown().await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(Box::pin(remote), Box::pin(relay), Box::pin(client));
        })
        .await
        .unwrap();
        assert_eq!(slots.available_permits(), 1);
        assert!(sibling.connection().close_reason().is_none());
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
}
