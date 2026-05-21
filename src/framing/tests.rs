//! Tests for protocol wire framing and visible frame rendering.

use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use super::{
    FrameContext, FrameOverflow, ProtocolFrame, ProtocolFrameCodec, decode_frame, encode_frame,
    render_frame_template, render_pending_observer_status,
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

/// Verifies that the configured maximum body size is enforced by direct decode.
#[test]
fn rejects_oversized_body() {
    let input = b"Content-Length: 10\r\n\r\n0123456789";

    let error = decode_frame(input, 4).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
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

    assert_eq!(rendered, "012...");
}

/// Verifies that pending observer labels are rendered in a compact frame-safe
/// status string.
#[test]
fn pending_observer_status_names_requests() {
    let observers = vec![("o1".to_string(), "reader".to_string())];

    let rendered = render_pending_observer_status(&observers, 80);

    assert_eq!(rendered, "pending observers: o1(reader)");
}
