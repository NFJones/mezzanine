//! Explicitly gated clipboard decoding without host clipboard or session work.
//!
//! Synthetic frames exercise shared assembly and direction-local codec state.
//! Sensitive effects use identity-only records, matching the producer contract.

use super::*;

/// Encodes one synthetic clipboard notification under the control MIME.
fn effect(method: &str, params: serde_json::Value) -> Vec<u8> {
    crate::control::encode_control_body(
        &serde_json::json!({
            "jsonrpc":"2.0","method":format!("client/clipboard.{method}"),"params":params
        })
        .to_string(),
    )
}

/// Explicit v2 construction accepts complete UTF-8 effects exactly once across
/// all codecs. Ordinary events retain their classification after the transfer;
/// partial effects remain neutral and cannot masquerade as redraw identities.
#[tokio::test]
async fn outbound_events_clipboard_preserves_codec_history_and_gate() {
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
        let mut bytes = crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE.to_vec();
        for wire in [
            effect(
                "begin",
                serde_json::json!({"sequence":7,"total_bytes":3,"chunks":1}),
            ),
            effect(
                "chunk",
                serde_json::json!({"sequence":7,"index":0,"data_base64":"6Zuq"}),
            ),
            effect("commit", serde_json::json!({"sequence":7})),
            frame("message", 8),
        ] {
            let encoded = match encoder.as_mut() {
                Some(encoder) => encoder
                    .encode_frame(&wire, IrohFrameCompressionMode::IdentityOnly)
                    .unwrap(),
                None => policy
                    .encode_frame(&wire, IrohFrameCompressionMode::IdentityOnly)
                    .unwrap(),
            };
            bytes.extend_from_slice(encoded.as_bytes());
        }
        let (stream, mut peer) = tokio::io::duplex(32768);
        peer.write_all(&bytes).await.unwrap();
        peer.shutdown().await.unwrap();
        let mut reader = OutboundEventReader::from_stream_version(stream, policy, true)
            .await
            .unwrap();
        assert!(
            reader.next().await.is_err(),
            "v2 cannot use a content-discarding consumer"
        );
        for _ in 0..2 {
            assert!(matches!(
                reader.next_item().await.unwrap(),
                Some(OutboundEventItem::Redraw(AttachRenderAction::None, None))
            ));
        }
        assert!(
            matches!(reader.next_item().await.unwrap(), Some(OutboundEventItem::Clipboard(content)) if content == "雪")
        );
        assert!(matches!(
            reader.next_item().await.unwrap(),
            Some(OutboundEventItem::Redraw(AttachRenderAction::View, Some(8)))
        ));
        assert!(reader.next_item().await.unwrap().is_none());
    }
    let policy =
        IrohCompressionPolicy::new(RuntimeIrohCompressionCodec::None, 1, 3, DECODED_LIMIT).unwrap();
    let (stream, mut peer) = tokio::io::duplex(32768);
    peer.write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE)
        .await
        .unwrap();
    peer.write_all(&effect(
        "begin",
        serde_json::json!({"sequence":1,"total_bytes":3,"chunks":1}),
    ))
    .await
    .unwrap();
    let mut reader = OutboundEventReader::from_stream(stream, policy)
        .await
        .unwrap();
    assert!(
        reader.next_item().await.is_err(),
        "v1 cannot grant clipboard authority from a frame"
    );
    assert!(reader.failed);
}

/// A cancelled item read retains framing and the same partial transfer. Idle
/// expiry clears sensitive partial state while leaving the stream alive; a late
/// commit is neutral and cannot recover expired content or invent transport EOF.
#[tokio::test(start_paused = true)]
async fn outbound_events_clipboard_cancel_and_idle_expiry_preserve_stream() {
    let policy =
        IrohCompressionPolicy::new(RuntimeIrohCompressionCodec::None, 1, 3, DECODED_LIMIT).unwrap();
    let (stream, mut peer) = tokio::io::duplex(32768);
    peer.write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE)
        .await
        .unwrap();
    let mut reader = OutboundEventReader::from_stream_version(stream, policy, true)
        .await
        .unwrap();
    peer.write_all(&effect(
        "begin",
        serde_json::json!({"sequence":1,"total_bytes":3,"chunks":1}),
    ))
    .await
    .unwrap();
    assert!(matches!(
        reader.next_item().await.unwrap(),
        Some(OutboundEventItem::Redraw(AttachRenderAction::None, None))
    ));
    let chunk = effect(
        "chunk",
        serde_json::json!({"sequence":1,"index":0,"data_base64":"6Zuq"}),
    );
    peer.write_all(&chunk[..12]).await.unwrap();
    let mut waiting = Box::pin(reader.next_item());
    assert!(matches!(
        futures_util::poll!(&mut waiting),
        std::task::Poll::Pending
    ));
    drop(waiting);
    assert_eq!(reader.pending, chunk[..12]);
    peer.write_all(&chunk[12..]).await.unwrap();
    assert!(matches!(
        reader.next_item().await.unwrap(),
        Some(OutboundEventItem::Redraw(AttachRenderAction::None, None))
    ));
    let mut waiting = Box::pin(reader.next_item());
    assert!(matches!(
        futures_util::poll!(&mut waiting),
        std::task::Poll::Pending
    ));
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert!(matches!(
        futures_util::poll!(&mut waiting),
        std::task::Poll::Pending
    ));
    drop(waiting);
    assert!(
        reader
            .clipboard
            .as_ref()
            .unwrap()
            .expiration_deadline()
            .is_none()
    );
    assert!(!reader.failed && !reader.ended);
    peer.write_all(&effect("commit", serde_json::json!({"sequence":1})))
        .await
        .unwrap();
    peer.write_all(&frame("message", 2)).await.unwrap();
    assert!(matches!(
        reader.next_item().await.unwrap(),
        Some(OutboundEventItem::Redraw(AttachRenderAction::None, None))
    ));
    assert!(matches!(
        reader.next_item().await.unwrap(),
        Some(OutboundEventItem::Redraw(AttachRenderAction::View, Some(2)))
    ));
    peer.shutdown().await.unwrap();
    assert!(reader.next_item().await.unwrap().is_none());
}
