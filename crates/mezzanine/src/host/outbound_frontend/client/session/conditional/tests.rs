//! Conditional client reuse requires separately committed snapshot ownership.
use super::*;

/// Changed geometry cannot reuse an older physical base. A full replacement
/// preserves validated snapshot fields but must be committed again before any
/// later conditional request can use its identity. No write or ACK is implied
/// by receiving replacement rows.
#[tokio::test]
async fn outbound_conditional_geometry_replacement_requires_new_commit() {
    let (root, listener, peer, owner) = fixture();
    let epoch = owner.cursor_blink_epoch;
    let handle = owner.client.handle.clone();
    let summary = owner.summary.clone();
    let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
    let (owner, _) = owner
        .present(&mut terminal, "initial", Duration::from_secs(1))
        .await
        .unwrap();
    let responder = async {
        let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
        let frame = peer.next().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert!(request.get("if_view_identity").is_none());
        assert_eq!(request["columns"], 81);
        peer.send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({
                "handle":handle,"session":summary,"lines":["replacement 雪"],
                "line_style_spans":[[]],"cursor":{"row":0,"column":0,"visible":false},
                "output_modes":{},"presentation_ids":[],"render_rate_limit_fps":10,
                "view_identity":"b".repeat(64),"event_cutoff":3
            })
            .to_string(),
        ))
        .await
        .unwrap();
        peer
    };
    let (result, mut peer) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            owner.conditional_snapshot(81, 24, Duration::from_secs(1)),
            responder
        )
    })
    .await
    .unwrap();
    let (owner, modified) = result.unwrap();
    assert!(modified);
    assert_eq!(owner.lines, ["replacement 雪"]);
    assert_eq!(owner.snapshot_size, (81, 24));
    assert_eq!(owner.cursor_blink_epoch, epoch);
    assert!(committed_base(&owner, 81, 24).is_none());
    assert_eq!(terminal.written_frames.len(), 1);
    let (owner, _) = owner
        .present(&mut terminal, "replacement", Duration::from_secs(1))
        .await
        .unwrap();
    let identity = "b".repeat(64);
    assert_eq!(committed_base(&owner, 81, 24), Some(identity.as_str()));
    assert_eq!(terminal.written_frames.len(), 2);
    drop(owner);
    assert!(peer.next().await.is_none());
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// Supplies an inert retained owner, not an authenticated remote session. The
/// disposable discovery directory supports production local writer validation.
fn fixture() -> (
    std::path::PathBuf,
    std::os::unix::net::UnixListener,
    tokio::net::UnixStream,
    OutboundSessionClient,
) {
    use std::os::unix::fs::PermissionsExt;
    let root =
        std::env::temp_dir().join(format!("mez-conditional-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(root.join("outbound.sock")).unwrap();
    std::fs::set_permissions(
        root.join("outbound.sock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
    let owner = OutboundSessionClient {
        client: OutboundFrontendClient {
            stream: Framed::new(stream, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap()),
            handle: FrontendHandle {
                owner: "fixture".into(),
                generation: 1,
            },
            discovery: Discovery::capture(&root).unwrap(),
        },
        summary: SessionSummary {
            selected_version: 3,
            granted_role: "primary".into(),
            session_id: "$1".into(),
            lease_id: "lease-one".into(),
            client_id: "c1".into(),
        },
        styles: vec![vec![]],
        modes: Default::default(),
        receipts: Vec::new(),
        lines: vec!["exact snapshot".into()],
        events_negotiated: false,
        render_rate_limit_fps: None,
        view_identity: Some("a".repeat(64)),
        event_cutoff: Some(1),
        snapshot_size: (80, 24),
        committed_view: None,
        iroh_status_slot: None,
        painted_health: None,
        cursor_blink_epoch: std::time::Instant::now(),
        painted_cursor: None,
        clipboard_receiver: None,
    };
    (root, listener, peer, owner)
}

/// Received revision evidence is insufficient for reuse. Complete output
/// enables only its exact identity/geometry, and explicit invalidation removes
/// that privilege. Wrong ownership and mixed unchanged/content replies reject.
/// An unchanged wire reply must preserve the committed rows and styles without
/// another terminal write. Invalidating that base sends an unconditional request;
/// an unsolicited unchanged response then retires the stream rather than reusing
/// stale physical output or replaying the request.
#[tokio::test]
async fn outbound_conditional_wire_preserves_base_and_rejects_invalidated_reuse() {
    let (root, listener, peer, owner) = fixture();
    let handle = owner.client.handle.clone();
    let summary = owner.summary.clone();
    let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
    let (owner, _) = owner
        .present(&mut terminal, "commit", Duration::from_secs(1))
        .await
        .unwrap();
    let lines = owner.lines.clone();
    let styles = owner.styles.clone();
    let identity = "a".repeat(64);
    let responder = async {
        let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
        let frame = peer.next().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert_eq!(request["handle"], serde_json::to_value(&handle).unwrap());
        assert_eq!(request["if_view_identity"], identity);
        peer.send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({
                "handle":handle,"session":summary,"columns":80,"rows":24,
                "not_modified":true,"view_identity":identity,"event_cutoff":2,
                "render_rate_limit_fps":30
            })
            .to_string(),
        ))
        .await
        .unwrap();
        peer
    };
    let (result, mut peer) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            owner.conditional_snapshot(80, 24, Duration::from_secs(1)),
            responder
        )
    })
    .await
    .unwrap();
    let (mut owner, modified) = result.unwrap();
    assert!(!modified);
    assert_eq!(owner.lines, lines);
    assert_eq!(owner.styles, styles);
    assert_eq!(
        owner.revision_evidence(),
        (Some(identity.as_str()), Some(2))
    );
    assert_eq!(owner.render_rate_limit_fps, Some(30));
    assert_eq!(terminal.written_frames.len(), 1);
    owner.invalidate_committed_view();
    let responder = async {
        let frame = peer.next().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert!(request.get("if_view_identity").is_none());
        peer.send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({
                "handle":handle,"session":summary,"columns":80,"rows":24,
                "not_modified":true,"view_identity":identity,"event_cutoff":3,
                "render_rate_limit_fps":30
            })
            .to_string(),
        ))
        .await
        .unwrap();
        assert!(
            peer.next().await.is_none(),
            "invalid reuse must retire without replay"
        );
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            owner.conditional_snapshot(80, 24, Duration::from_secs(1)),
            responder
        )
    })
    .await
    .unwrap();
    assert!(result.is_err());
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// Received revision evidence is insufficient for reuse. Complete output
/// enables only its exact identity/geometry, and explicit invalidation removes
/// that privilege. Wrong ownership and mixed unchanged/content replies reject.
#[tokio::test]
async fn outbound_conditional_client_requires_committed_output() {
    let (root, listener, peer, owner) = fixture();
    assert!(committed_base(&owner, 80, 24).is_none());
    let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
    let (mut owner, _) = owner
        .present(&mut terminal, "commit", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(terminal.written_frames.len(), 1);
    let identity = "a".repeat(64);
    assert_eq!(committed_base(&owner, 80, 24), Some(identity.as_str()));
    assert!(committed_base(&owner, 81, 24).is_none());
    let original = serde_json::json!({"handle":owner.client.handle,"session":owner.summary,
        "columns":80,"rows":24,"not_modified":true,"view_identity":identity,
        "event_cutoff":2,"render_rate_limit_fps":30});
    let reply: Unchanged = serde_json::from_value(original.clone()).unwrap();
    validate_unchanged(&reply, &owner, 80, 24, Some(&identity)).unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/client_id", serde_json::json!("c2")),
        ("/columns", serde_json::json!(81)),
        ("/not_modified", serde_json::json!(false)),
        ("/view_identity", serde_json::json!("b".repeat(64))),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let reply: Unchanged = serde_json::from_value(invalid).unwrap();
        assert!(validate_unchanged(&reply, &owner, 80, 24, Some(&identity)).is_err());
    }
    let mut mixed = original;
    mixed["lines"] = serde_json::json!(["replacement"]);
    assert!(serde_json::from_value::<Unchanged>(mixed).is_err());
    owner.invalidate_committed_view();
    assert!(validate_unchanged(&reply, &owner, 80, 24, Some(&identity)).is_err());
    drop(owner);
    drop(peer);
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}
