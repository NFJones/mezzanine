//! Tests for protocol wire framing and visible frame rendering.

use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use super::{
    FrameContext, FrameOverflow, ProtocolFrame, ProtocolFrameCodec, decode_frame, encode_frame,
    render_frame_template,
};

/// Verifies that a content-length frame round-trips through direct wire helpers.
#[test]
fn encodes_and_decodes_content_length_frame() {
    let frame = ProtocolFrame::new("application/vnd.mezzanine.test+json", "{\"ok\":true}");

    let encoded = encode_frame(&frame);
    let (decoded, consumed) = decode_frame(&encoded, 1024).unwrap();

    assert_eq!(decoded, frame);
    assert_eq!(consumed, encoded.len());
}

/// Verifies that malformed frames without a content-length header are rejected.
#[test]
fn rejects_missing_content_length() {
    let input = b"Content-Type: application/json\r\n\r\n{}";

    let error = decode_frame(input, 1024).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies duplicate content-length headers are rejected consistently.
///
/// Streaming and direct frame decoding both depend on the advertised body size.
/// Duplicate length headers would otherwise let the two paths choose different
/// body boundaries, so both same-value and conflicting duplicates must fail
/// before any frame body is accepted.
#[test]
fn rejects_duplicate_content_length_headers() {
    let same_value = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
    let conflicting = b"Content-Length: 2\r\nContent-Length: 3\r\n\r\n{}!";

    let same_error = decode_frame(same_value, 1024).unwrap_err();
    let conflicting_error = decode_frame(conflicting, 1024).unwrap_err();

    assert_eq!(same_error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert_eq!(
        conflicting_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
}

/// Verifies that the configured maximum body size is enforced by direct decode.
#[test]
fn rejects_oversized_body() {
    let input = b"Content-Length: 10\r\n\r\n0123456789";

    let error = decode_frame(input, 4).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Unterminated headers must fail within a finite header budget independently
/// of the body limit, before an Iroh bridge can retain arbitrary peer bytes.
#[test]
fn codec_rejects_unterminated_header_over_budget() {
    let mut codec = ProtocolFrameCodec::new(1024 * 1024).unwrap();
    let mut input = BytesMut::from(vec![b'x'; 8193].as_slice());
    let error = codec.decode(&mut input).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.to_string().contains("header"));
    assert_eq!(input.len(), 8193, "rejection must not consume peer input");
}

/// Verifies that streaming decode leaves partial input untouched and consumes a
/// complete frame only after the remaining bytes arrive.
#[test]
fn codec_decodes_split_frames_without_consuming_partial_input() {
    let frame = ProtocolFrame::new("application/json", r#"{"ok":true}"#);
    let encoded = encode_frame(&frame);
    let split_at = encoded.len() - 3;
    let mut codec = ProtocolFrameCodec::new(1024).unwrap();
    let mut input = BytesMut::from(&encoded[..split_at]);

    assert_eq!(codec.decode(&mut input).unwrap(), None);
    assert_eq!(input.len(), split_at);

    input.extend_from_slice(&encoded[split_at..]);
    assert_eq!(codec.decode(&mut input).unwrap(), Some(frame));
    assert!(input.is_empty());
}

/// Exact-budget headers remain valid across split terminators, but a terminator
/// beyond the budget fails even when the configured body budget is much larger.
/// Large bodies and later buffered frames must not count toward the first header.
#[test]
fn header_budget_preserves_boundaries_fragmentation_and_frame_independence() {
    let limit = super::wire::MAX_PROTOCOL_HEADER_BYTES;
    for header_bytes in [limit - 1, limit, limit + 1] {
        let prefix = "Content-Length: 9000\r\nX-Padding: ";
        let header = format!(
            "{prefix}{}\r\n\r\n",
            "x".repeat(header_bytes - prefix.len() - 4)
        );
        assert_eq!(header.len(), header_bytes);
        let body = "b".repeat(9000);
        let first = format!("{header}{body}");
        let next = encode_frame(&ProtocolFrame::new("application/json", "{}"));
        let mut complete = first.as_bytes().to_vec();
        complete.extend_from_slice(&next);
        let mut codec = ProtocolFrameCodec::new(16384).unwrap();
        let mut input = BytesMut::from(complete.as_slice());
        if header_bytes > limit {
            assert!(
                codec
                    .decode(&mut input)
                    .unwrap_err()
                    .to_string()
                    .contains("header")
            );
            assert!(
                decode_frame(&complete, 16384)
                    .unwrap_err()
                    .to_string()
                    .contains("header")
            );
            continue;
        }
        let (decoded, consumed) = decode_frame(&complete, 16384).unwrap();
        assert_eq!(consumed, first.len());
        assert_eq!(codec.decode(&mut input).unwrap(), Some(decoded));
        assert_eq!(
            codec.decode(&mut input).unwrap(),
            Some(ProtocolFrame::new("application/json", "{}"))
        );
        assert!(input.is_empty());
        for split in [
            header.len() - 3,
            header.len() - 2,
            header.len() - 1,
            header.len(),
            first.len() - 1,
        ] {
            let mut codec = ProtocolFrameCodec::new(16384).unwrap();
            let mut partial = BytesMut::from(&first.as_bytes()[..split]);
            assert_eq!(codec.decode(&mut partial).unwrap(), None);
            assert_eq!(partial.len(), split);
            partial.extend_from_slice(&first.as_bytes()[split..]);
            assert_eq!(codec.decode(&mut partial).unwrap().unwrap().body, body);
            assert!(partial.is_empty());
        }
    }
    let mut codec = ProtocolFrameCodec::new(16384).unwrap();
    let mut pending = BytesMut::new();
    for _ in 0..31 {
        pending.extend_from_slice(&[b'x'; 256]);
        assert_eq!(codec.decode(&mut pending).unwrap(), None);
    }
    pending.extend_from_slice(&[b'x'; 256]);
    assert!(
        codec
            .decode(&mut pending)
            .unwrap_err()
            .to_string()
            .contains("header")
    );
}

/// Verifies streaming decode rejects duplicate content-length headers.
///
/// The incremental codec decides whether to wait for more bytes from the header
/// alone. This regression keeps that decision aligned with full-frame decoding
/// by rejecting duplicate size declarations before consuming input.
#[test]
fn codec_rejects_duplicate_content_length_headers() {
    let mut codec = ProtocolFrameCodec::new(1024).unwrap();
    let mut input = BytesMut::from(&b"Content-Length: 2\r\nContent-Length: 3\r\n\r\n{}!"[..]);

    let error = codec.decode(&mut input).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that streaming encode writes valid frames and rejects bodies over
/// the configured limit.
#[test]
fn codec_encodes_and_rejects_oversized_bodies() {
    let mut codec = ProtocolFrameCodec::new(4).unwrap();
    let mut output = BytesMut::new();

    codec
        .encode(ProtocolFrame::new("application/json", "ok"), &mut output)
        .unwrap();
    assert!(output.starts_with(b"Content-Length: 2\r\n"));

    let error = codec
        .encode(
            ProtocolFrame::new("application/json", "too-long"),
            &mut output,
        )
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that visible frame templates substitute known fields and render
/// missing fields as empty text.
#[test]
fn frame_template_renders_named_fields_and_empty_missing_values() {
    let context = FrameContext::new()
        .with("window.index", "1")
        .with("window.name", "work");

    let rendered = render_frame_template(
        "#{window.index}:#{window.name}:#{pane.id}",
        &context,
        80,
        FrameOverflow::Truncate,
    );

    assert_eq!(rendered, "1:work:");
}

/// Verifies that control characters are stripped from visible frame text.
#[test]
fn frame_template_sanitizes_control_characters() {
    let context = FrameContext::new().with("window.name", "bad\u{1b}[31m");

    let rendered = render_frame_template("#{window.name}", &context, 80, FrameOverflow::Truncate);

    assert_eq!(rendered, "bad [31m");
}

/// Verifies that elision preserves the requested width for long visible frame
/// fields.
#[test]
fn frame_template_elides_to_width() {
    let context = FrameContext::new().with("window.name", "0123456789");

    let rendered = render_frame_template("#{window.name}", &context, 6, FrameOverflow::Elide);

    assert_eq!(rendered, "01234\u{2026}");
}
