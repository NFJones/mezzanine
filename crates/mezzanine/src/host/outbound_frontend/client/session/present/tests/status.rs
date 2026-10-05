//! Presenter composition qualifies local output, not physical terminal visibility.
//!
//! Synthetic authenticated IPC supplies only coarse health observations. The
//! receipt-aware partial writer establishes an explicit complete-tail barrier;
//! health failure and output failure must retire without receipt acknowledgement.

use super::*;
use mez_terminal::{GraphicRendition, TerminalColor};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Builds one inert retained frame with a validated status slot and receipt.
/// Disposable discovery evidence supports the production client exchange without
/// creating remote authority or reading user configuration.
fn fixture() -> (
    std::path::PathBuf,
    std::os::unix::net::UnixListener,
    tokio::net::UnixStream,
    OutboundSessionClient,
) {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!(
        "mez-present-status-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(root.join("outbound.sock")).unwrap();
    std::fs::set_permissions(
        root.join("outbound.sock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
    let colored = |index| GraphicRendition {
        background: Some(TerminalColor::Indexed(index)),
        ..Default::default()
    };
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
        lines: vec!["雪a    tail".into()],
        styles: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 11,
            rendition: GraphicRendition {
                bold: true,
                ..Default::default()
            },
        }]],
        modes: Default::default(),
        receipts: vec![7],
        events_negotiated: false,
        render_rate_limit_fps: None,
        view_identity: Some("a".repeat(64)),
        event_cutoff: Some(1),
        snapshot_size: (80, 24),
        committed_view: None,
        iroh_status_slot: Some(crate::host::terminal::TerminalIrohStatusSlot {
            row: 0,
            column: 3,
            width: 4,
            good: colored(2),
            degraded: colored(3),
            poor: colored(1),
            unknown: colored(8),
        }),
    };
    (root, listener, peer, owner)
}

/// Exact health facts affect only the returned local decoration. ACK follows the
/// original frame's complete partial tail, while retained server rows/styles stay
/// intact. Disconnection, foreign health ownership, and failed output never ACK.
#[tokio::test]
async fn outbound_present_status_uses_exact_health_and_commits_before_ack() {
    for mode in ["success", "unknown", "down", "foreign", "output-failure"] {
        let (root, listener, peer, owner) = fixture();
        let handle = owner.client.handle.clone();
        let summary = owner.summary.clone();
        let lines = owner.lines.clone();
        let styles = owner.styles.clone();
        let completion = Arc::new(AtomicBool::new(false));
        let mut terminal = PartialWriter {
            completion: Some(completion.clone()),
            fail_flush: mode == "output-failure",
            ..Default::default()
        };
        let responder = async {
            let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
            let frame = peer.next().await.unwrap().unwrap();
            assert_eq!(frame.content_type, CONTENT_TYPE);
            let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(
                request,
                serde_json::json!({"operation":"health","handle":handle})
            );
            assert!(
                !completion.load(Ordering::SeqCst),
                "health exchange must precede output"
            );
            let mut reply = serde_json::json!({"handle":handle,"session":summary,
                "connected":mode != "down", "quality":if matches!(mode, "unknown" | "down") { "unknown" } else { "degraded" }});
            if mode == "foreign" {
                reply["handle"]["generation"] = serde_json::json!(2);
            }
            peer.send(ProtocolFrame::new(CONTENT_TYPE, reply.to_string()))
                .await
                .unwrap();
            if matches!(mode, "success" | "unknown") {
                let frame = peer.next().await.unwrap().unwrap();
                let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
                assert_eq!(request["operation"], "acknowledge");
                assert_eq!(request["handle"], serde_json::to_value(&handle).unwrap());
                assert_eq!(request["idempotency_key"], "exact-output");
                assert_eq!(request["presentation_ids"], serde_json::json!([7]));
                assert!(
                    completion.load(Ordering::SeqCst),
                    "ACK requires complete output"
                );
                peer.send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({
                        "handle":handle,"session":summary,"idempotency_key":"exact-output",
                        "presentation_ids":[7],"acknowledged":true
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
                peer
            } else {
                assert!(
                    peer.next().await.is_none(),
                    "failed health/output must retire without ACK"
                );
                peer
            }
        };
        let (result, peer) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                owner.present(&mut terminal, "exact-output", Duration::from_secs(1)),
                responder
            )
        })
        .await
        .unwrap();
        if matches!(mode, "success" | "unknown") {
            let (owner, acknowledged) = result.unwrap();
            assert!(acknowledged);
            assert_eq!(owner.lines, lines);
            assert_eq!(owner.styles, styles);
            assert!(owner.receipts.is_empty());
            assert!(owner.committed_view.is_some());
            assert_eq!(
                (terminal.frames, terminal.flushes, terminal.pending),
                (1, 2, 0)
            );
            assert_eq!(terminal.lines, ["雪a up tail"]);
            let expected = if mode == "unknown" { 8 } else { 3 };
            assert_eq!(
                terminal.styles[0]
                    .iter()
                    .find(|span| span.start == 3)
                    .unwrap()
                    .rendition
                    .background,
                Some(TerminalColor::Indexed(expected))
            );
            drop(owner);
        } else {
            assert!(result.is_err());
            assert!(!completion.load(Ordering::SeqCst));
            assert_eq!(terminal.frames, usize::from(mode == "output-failure"));
        }
        drop(peer);
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}
