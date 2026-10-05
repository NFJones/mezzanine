//! Owned event framing across codecs, fragmentation and cancelled read waiters.
//!
//! Duplex fixtures carry only synthetic events; no negotiation or remote session
//! authority is established. Codec history and pending bytes remain reader-owned.

use super::*;
use crate::runtime::{IrohFrameCompressionMode, IrohStreamEncoder};
use tokio::io::AsyncWriteExt;

mod clipboard;

/// Actual QUIC setup is deadline-bound, rejects an incorrect stream preface,
/// and disposes only that stream. A later valid stream on the retained connection
/// remains usable; no endpoint shutdown, detached worker or reconnect is needed.
#[tokio::test]
async fn outbound_events_quic_setup_is_bounded_and_stream_scoped() {
    use iroh::endpoint::presets::Minimal;
    use std::time::Duration;
    let server = iroh::Endpoint::builder(Minimal)
        .secret_key(iroh::SecretKey::generate())
        .alpns(vec![crate::runtime::MEZZANINE_IROH_ALPN.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let client = iroh::Endpoint::builder(Minimal)
        .secret_key(iroh::SecretKey::generate())
        .transport_config(
            iroh::endpoint::QuicTransportConfig::builder()
                .max_concurrent_uni_streams(iroh::endpoint::VarInt::from_u32(4))
                .build(),
        )
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let accept = async { server.accept().await.unwrap().await.unwrap() };
    let (connection, peer) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            client.connect(server.addr(), crate::runtime::MEZZANINE_IROH_ALPN),
            accept
        )
    })
    .await
    .unwrap();
    let connection = connection.unwrap();
    let policy =
        IrohCompressionPolicy::new(RuntimeIrohCompressionCodec::None, 1, 3, DECODED_LIMIT).unwrap();
    let error = OutboundEventReader::accept(&connection, policy, Duration::from_millis(100))
        .await
        .err()
        .unwrap();
    assert!(error.message().contains("timed out"));
    assert!(connection.close_reason().is_none());
    for valid in [false, true] {
        let write = async {
            let mut send = peer.open_uni().await.unwrap();
            if valid {
                send.write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE)
                    .await
                    .unwrap();
                send.write_all(&frame("message", 11)).await.unwrap();
            } else {
                send.write_all(b"mezzanine/events/2\n").await.unwrap();
            }
            send.finish().unwrap();
            send
        };
        let (reader, _send) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                OutboundEventReader::accept(&connection, policy, Duration::from_secs(2)),
                write
            )
        })
        .await
        .unwrap();
        if valid {
            let mut reader = reader.unwrap();
            assert_eq!(
                reader.next().await.unwrap(),
                Some((AttachRenderAction::View, Some(11)))
            );
            assert!(reader.next().await.unwrap().is_none());
        } else {
            assert!(
                reader
                    .err()
                    .unwrap()
                    .message()
                    .contains("preface unsupported")
            );
        }
        assert!(connection.close_reason().is_none());
    }
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"fixture complete");
    client.close().await;
    server.close().await;
}

/// Encodes one synthetic notification without terminal content or credentials.
fn frame(event: &str, id: u64) -> Vec<u8> {
    crate::control::encode_control_body(
        &serde_json::json!({
            "jsonrpc":"2.0","method":format!("event/{event}"),
            "params":{"event_type":event,"event_id":id}
        })
        .to_string(),
    )
}

/// Every configured codec must preserve consecutive event identity and clean
/// EOF. Stateful variants use one encoder for both records, matching production
/// direction-local history rather than independently encoding each event.
#[tokio::test]
async fn outbound_events_preserve_codec_history_and_clean_eof() {
    for codec in [
        RuntimeIrohCompressionCodec::None,
        RuntimeIrohCompressionCodec::Zstd,
        RuntimeIrohCompressionCodec::Lz4,
        RuntimeIrohCompressionCodec::ZstdStream,
        RuntimeIrohCompressionCodec::Lz4Stream,
    ] {
        let policy = IrohCompressionPolicy::new(codec, 1, 3, DECODED_LIMIT).unwrap();
        let mut encoder = policy
            .is_streaming()
            .then(|| IrohStreamEncoder::new(policy).unwrap());
        let mut bytes = crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE.to_vec();
        for wire in [frame("pane_changed", 7), frame("config_changed", 8)] {
            let encoded = match encoder.as_mut() {
                Some(encoder) => encoder
                    .encode_frame(&wire, IrohFrameCompressionMode::Eligible)
                    .unwrap(),
                None => policy
                    .encode_frame(&wire, IrohFrameCompressionMode::Eligible)
                    .unwrap(),
            };
            bytes.extend_from_slice(encoded.as_bytes());
        }
        let (stream, mut peer) = tokio::io::duplex(32768);
        peer.write_all(&bytes).await.unwrap();
        peer.shutdown().await.unwrap();
        let mut reader = OutboundEventReader::from_stream(stream, policy)
            .await
            .unwrap();
        assert_eq!(
            reader.next().await.unwrap(),
            Some((AttachRenderAction::View, Some(7)))
        );
        assert_eq!(
            reader.next().await.unwrap(),
            Some((AttachRenderAction::ImmediateView, Some(8)))
        );
        assert_eq!(reader.next().await.unwrap(), None);
        assert_eq!(reader.next().await.unwrap(), None);
    }
}

/// Cancelling a pending read after a prefix is buffered must not discard bytes
/// or reset decoder history. Completing the record returns its exact identity
/// once, without requiring a new connection or replay from the producer.
#[tokio::test]
async fn outbound_events_cancelled_read_retains_partial_frame() {
    let policy =
        IrohCompressionPolicy::new(RuntimeIrohCompressionCodec::None, 1, 3, DECODED_LIMIT).unwrap();
    let (stream, mut peer) = tokio::io::duplex(32768);
    peer.write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE)
        .await
        .unwrap();
    let mut reader = OutboundEventReader::from_stream(stream, policy)
        .await
        .unwrap();
    let wire = frame("message", 9);
    peer.write_all(&wire[..12]).await.unwrap();
    let mut waiting = Box::pin(reader.next());
    assert!(matches!(
        futures_util::poll!(&mut waiting),
        std::task::Poll::Pending
    ));
    drop(waiting);
    assert_eq!(reader.pending, wire[..12]);
    peer.write_all(&wire[12..]).await.unwrap();
    peer.shutdown().await.unwrap();
    assert_eq!(
        reader.next().await.unwrap(),
        Some((AttachRenderAction::View, Some(9)))
    );
    assert!(reader.next().await.unwrap().is_none());
}

/// Invalid framing and truncated EOF permanently poison the owner. Another call
/// cannot reuse consumed compression history or expose untrusted error content.
#[tokio::test]
async fn outbound_events_invalid_frames_poison_owned_reader() {
    for bytes in [
        b"Content-Length: 1048577\r\n\r\n".to_vec(),
        b"Content-Length: 2\r\n\r\nx".to_vec(),
        crate::protocol::framing::encode_frame(&crate::protocol::framing::ProtocolFrame::new(
            "application/json",
            "private-proof",
        )),
        crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":1,"result":{"token":"private-proof"}}"#,
        ),
    ] {
        let policy =
            IrohCompressionPolicy::new(RuntimeIrohCompressionCodec::None, 1, 3, DECODED_LIMIT)
                .unwrap();
        let (stream, mut peer) = tokio::io::duplex(32768);
        peer.write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE)
            .await
            .unwrap();
        peer.write_all(&bytes).await.unwrap();
        peer.shutdown().await.unwrap();
        let mut reader = OutboundEventReader::from_stream(stream, policy)
            .await
            .unwrap();
        let error = reader.next().await.unwrap_err();
        assert!(!error.message().contains("private-proof"));
        assert!(reader.failed);
        assert_eq!(
            reader.next().await.unwrap_err().message(),
            "outbound event reader unavailable"
        );
    }
}
