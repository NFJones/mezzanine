//! Consumed item exchange tests use disposable IPC and no host clipboard.
//!
//! Exact transfer completion returns one owned source. Interrupted or replayed
//! transfers retire the same stream rather than exposing partial content.

use super::*;
use crate::host::outbound_frontend::clipboard_wire::{ClipboardFrames, ClipboardReceiver};

/// Provides a retained inert owner with private discovery and bounded framing.
/// No production capability is issued; synthetic peer replies test consumption.
fn fixture() -> (
    std::path::PathBuf,
    std::os::unix::net::UnixListener,
    tokio::net::UnixStream,
    OutboundSessionClient,
) {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("mez-items-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(root.join("outbound.sock")).unwrap();
    std::fs::set_permissions(
        root.join("outbound.sock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let summary = SessionSummary {
        selected_version: 3,
        granted_role: "primary".into(),
        session_id: "$1".into(),
        lease_id: "lease-one".into(),
        client_id: "c1".into(),
    };
    let receiver = ClipboardReceiver::new(handle.clone(), summary.clone());
    let owner = OutboundSessionClient {
        client: OutboundFrontendClient {
            stream: Framed::new(stream, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap()),
            handle,
            discovery: Discovery::capture(&root).unwrap(),
        },
        summary,
        styles: vec![vec![]],
        modes: Default::default(),
        receipts: Vec::new(),
        lines: vec!["retained frame".into()],
        events_negotiated: false,
        render_rate_limit_fps: None,
        view_identity: None,
        event_cutoff: None,
        snapshot_size: (80, 24),
        committed_view: None,
        iroh_status_slot: None,
        painted_health: None,
        cursor_blink_epoch: std::time::Instant::now(),
        painted_cursor: None,
        clipboard_receiver: Some(receiver),
    };
    (root, listener, peer, owner)
}

/// Multiple occurrences preserve exact content across chunk boundaries and keep
/// the decoder watermark. Replaying a committed occurrence fails and closes the
/// connection with no extra poll or effect request.
#[tokio::test]
async fn outbound_client_items_complete_then_reject_replayed_occurrence() {
    let (root, listener, peer, mut owner) = fixture();
    let handle = owner.client.handle.clone();
    let summary = owner.summary.clone();
    let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
    let source = format!("{}雪\r\n", "x".repeat(256 * 1024 - 1));
    let mut replay = false;
    for transfer in [1, 2, 2] {
        let response = async {
            let frame = peer.next().await.unwrap().unwrap();
            let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(
                request,
                serde_json::json!({"operation":"items","handle":handle,"wait_ms":25})
            );
            let mut frames = ClipboardFrames::new(&handle, &summary, transfer, &source).unwrap();
            peer.send(frames.next().unwrap().unwrap()).await.unwrap();
            if transfer == 2 && replay {
                return;
            }
            for frame in frames {
                peer.send(frame.unwrap()).await.unwrap();
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(owner.poll_items(25, Duration::from_secs(1)), response)
        })
        .await
        .unwrap();
        if replay {
            assert!(result.is_err());
            assert!(peer.next().await.is_none());
            break;
        }
        let (returned, item) = result.unwrap();
        owner = returned;
        assert!(matches!(item, FrontendItem::Clipboard(content) if content == source));
        assert_eq!(owner.lines, ["retained frame"]);
        replay = transfer == 2;
    }
    drop(peer);
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// A withheld commit times out the total exchange and disposes partial content
/// and its stream without another request. No incomplete source is returned.
#[tokio::test]
async fn outbound_client_items_timeout_discards_partial_transfer_without_replay() {
    let (root, listener, peer, owner) = fixture();
    let handle = owner.client.handle.clone();
    let summary = owner.summary.clone();
    let responder = async {
        let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
        peer.next().await.unwrap().unwrap();
        let mut frames = ClipboardFrames::new(&handle, &summary, 1, "private-content").unwrap();
        peer.send(frames.next().unwrap().unwrap()).await.unwrap();
        peer.send(frames.next().unwrap().unwrap()).await.unwrap();
        assert!(
            peer.next().await.is_none(),
            "failed exchange must not replay"
        );
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(owner.poll_items(1, Duration::from_millis(100)), responder)
    })
    .await
    .unwrap();
    assert!(result.is_err());
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}
