//! Bounded exact-owner transfers without clipboard writes or transport work.
use super::*;

/// Supplies inert validated-shape fixture metadata without remote credentials.
fn owner() -> (FrontendHandle, SessionSummary) {
    (FrontendHandle { owner: "fixture".into(), generation: 1 }, serde_json::from_value(serde_json::json!({
        "selected_version":3,"granted_role":"primary","session_id":"$1","lease_id":"lease-one","client_id":"c1"
    })).unwrap())
}

/// Empty, Unicode across chunk boundaries and maximum effects round-trip exactly
/// through finite frames. The lazy producer does not retain encoded frame copies.
#[test]
fn outbound_clipboard_wire_round_trips_bounded_frames() {
    let (handle, session) = owner();
    for content in [
        String::new(),
        format!("{}雪\r\n", "x".repeat(CHUNK_BYTES - 1)),
        "y".repeat(MAX_BYTES),
    ] {
        let mut receiver = ClipboardReceiver::new(handle.clone(), session.clone());
        let mut completed = None;
        let mut frames = 0;
        for frame in ClipboardFrames::new(&handle, &session, 1, &content).unwrap() {
            let frame = frame.unwrap();
            assert!(frame.body.len() <= FRAME_BYTES);
            let value = receiver.apply(&frame).unwrap();
            if value.is_some() {
                assert!(completed.is_none());
                completed = value;
            }
            frames += 1;
        }
        assert_eq!(frames, content.len().div_ceil(CHUNK_BYTES).max(1) + 2);
        assert_eq!(completed.as_deref(), Some(content.as_str()));
        assert!(receiver.pending.is_none());
    }
    assert!(ClipboardFrames::new(&handle, &session, 0, "").is_err());
    assert!(ClipboardFrames::new(&handle, &session, 1, &"x".repeat(MAX_BYTES + 1)).is_err());
    assert!(std::mem::size_of::<ClipboardFrames<'_>>() < 128);
}

/// Foreign handles/session facts, wrong sequence and incomplete commits clear
/// partial content and poison the exchange. Error text never includes content.
#[test]
fn outbound_clipboard_wire_rejects_foreign_or_malformed_transfers() {
    let (handle, session) = owner();
    let frames = ClipboardFrames::new(&handle, &session, 1, "private-content")
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/client_id", serde_json::json!("c2")),
        ("/transfer", serde_json::json!(0)),
        ("/chunks", serde_json::json!(100)),
    ] {
        let mut receiver = ClipboardReceiver::new(handle.clone(), session.clone());
        let mut body: serde_json::Value = serde_json::from_str(&frames[0].body).unwrap();
        *body.pointer_mut(pointer).unwrap() = value;
        let error = receiver
            .apply(&ProtocolFrame::new(CONTENT_TYPE, body.to_string()))
            .unwrap_err();
        assert!(!error.message().contains("private-content"));
        assert!(receiver.failed && receiver.pending.is_none());
        assert!(receiver.apply(&frames[0]).is_err());
    }
    let mut receiver = ClipboardReceiver::new(handle.clone(), session.clone());
    receiver.apply(&frames[0]).unwrap();
    assert!(receiver.apply(&frames[2]).is_err());
    assert!(receiver.pending.is_none());
    for (pointer, value) in [
        ("/index", serde_json::json!(1)),
        ("/transfer", serde_json::json!(2)),
        (
            "/data_base64",
            serde_json::json!("!invalid-private-content"),
        ),
        ("/data_base64", serde_json::json!("eA==")),
        (
            "/data_base64",
            serde_json::json!("x".repeat(CHUNK_BYTES.div_ceil(3) * 4 + 1)),
        ),
    ] {
        let mut receiver = ClipboardReceiver::new(handle.clone(), session.clone());
        receiver.apply(&frames[0]).unwrap();
        let mut body: serde_json::Value = serde_json::from_str(&frames[1].body).unwrap();
        *body.pointer_mut(pointer).unwrap() = value;
        let error = receiver
            .apply(&ProtocolFrame::new(CONTENT_TYPE, body.to_string()))
            .unwrap_err();
        assert!(!error.message().contains("private-content"));
        assert!(receiver.failed && receiver.pending.is_none());
    }
    let mut receiver = ClipboardReceiver::new(handle.clone(), session.clone());
    let mut extra: serde_json::Value = serde_json::from_str(&frames[0].body).unwrap();
    extra["credential"] = serde_json::json!("private-content");
    assert!(
        receiver
            .apply(&ProtocolFrame::new(CONTENT_TYPE, extra.to_string()))
            .is_err()
    );
    assert!(receiver.pending.is_none());
    let mut receiver = ClipboardReceiver::new(handle.clone(), session.clone());
    assert!(
        receiver
            .apply(&ProtocolFrame::new(
                "application/json",
                frames[0].body.clone()
            ))
            .is_err()
    );
    let mut receiver = ClipboardReceiver::new(handle.clone(), session.clone());
    receiver.apply(&frames[0]).unwrap();
    let mut invalid: serde_json::Value = serde_json::from_str(&frames[1].body).unwrap();
    invalid["data_base64"] = serde_json::json!(
        base64::engine::general_purpose::STANDARD.encode(vec![0xff; "private-content".len()])
    );
    receiver
        .apply(&ProtocolFrame::new(CONTENT_TYPE, invalid.to_string()))
        .unwrap();
    assert!(
        receiver.apply(&frames[2]).is_err(),
        "only committed UTF-8 may be exposed"
    );
    assert!(receiver.pending.is_none());
    let mut receiver = ClipboardReceiver::new(handle, session);
    for frame in &frames {
        receiver.apply(frame).unwrap();
    }
    assert!(
        receiver.apply(&frames[0]).is_err(),
        "completed occurrence cannot replay"
    );
}
