//! Item-aware foreground qualification without a desktop clipboard provider.
//!
//! Synthetic IPC supplies complete or interrupted transfers. A function backend
//! records exact test source only; ownership loss must not expose partial content
//! or replay effects. These fixtures do not prove physical clipboard delivery.

use super::*;
use crate::host::outbound_frontend::clipboard_wire::{ClipboardFrames, ClipboardReceiver};
use std::sync::Mutex;

static COPIED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Records only synthetic fixture content using the existing policy adapter.
fn copy_fixture(content: &str) -> bool {
    COPIED.lock().unwrap().push(content.to_string());
    true
}

/// Complete transfers may enter the explicit local adapter exactly once; a
/// cancelled partial transfer cannot. Foreground cancellation retires its IPC,
/// restores presentation and introduces no view fetch, ACK or input mutation.
#[tokio::test]
async fn outbound_foreground_clipboard_applies_only_completed_effects() {
    for complete in [false, true] {
        COPIED.lock().unwrap().clear();
        let (root, listener, peer, mut client) = inert_client(Vec::new());
        let handle = client.client.handle.clone();
        let summary = client.summary.clone();
        client.clipboard_receiver = Some(ClipboardReceiver::new(handle.clone(), summary.clone()));
        let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        for _ in 0..3 {
            terminal.push_pending_input_read();
        }
        let stop = std::sync::Arc::new(tokio::sync::Notify::new());
        let foreground_stop = stop.clone();
        let source = "synthetic Unicode 雪\r\n\n";
        let foreground = client.run_clipboard_foreground(
            &mut terminal,
            Size::new(80, 24).unwrap(),
            Duration::from_secs(2),
            async move { foreground_stop.notified().await },
            crate::host::terminal::HostClipboard::new(copy_fixture, || None),
        );
        let responder = async {
            let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
            let frame = peer.next().await.unwrap().unwrap();
            let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(
                request,
                serde_json::json!({"operation":"items","handle":handle,"wait_ms":25})
            );
            let mut frames = ClipboardFrames::new(&handle, &summary, 1, source).unwrap();
            peer.send(frames.next().unwrap().unwrap()).await.unwrap();
            peer.send(frames.next().unwrap().unwrap()).await.unwrap();
            if complete {
                peer.send(frames.next().unwrap().unwrap()).await.unwrap();
                // A subsequent items request proves the completed transfer was
                // validated; no view fetch or receipt acknowledgement is needed.
                let frame = peer.next().await.unwrap().unwrap();
                let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
                assert_eq!(request["operation"], "items");
                tokio::time::timeout(Duration::from_secs(2), async {
                    while COPIED.lock().unwrap().is_empty() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            stop.notify_one();
            let next = peer.next().await;
            assert!(
                next.is_none()
                    || matches!(&next, Some(Err(error)) if error.io_kind() == Some(std::io::ErrorKind::ConnectionReset)),
                "cancelled item exchange must not replay (complete={complete}, wire_error={:?})",
                next.as_ref()
                    .and_then(|frame| frame.as_ref().err())
                    .map(|error| (error.kind(), error.io_kind()))
            );
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(foreground, responder)
        })
        .await
        .unwrap();
        result.unwrap();
        assert_eq!(
            COPIED.lock().unwrap().as_slice(),
            if complete {
                vec![source.to_string()]
            } else {
                Vec::new()
            }
        );
        assert_eq!(terminal.written_frames.len(), 1);
        assert_eq!(terminal.presentation_entries, 1);
        assert_eq!(terminal.presentation_restores, 1);
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A clipboard adapter cannot broaden observer or unnegotiated ownership. Both
/// failures precede terminal entry and worker construction, so no copy is queued.
#[tokio::test]
async fn outbound_foreground_clipboard_rejects_unnegotiated_or_observer_owners() {
    for observer in [false, true] {
        let (root, listener, mut peer, mut client) = inert_client(Vec::new());
        if observer {
            client.summary.granted_role = "observer".into();
            client.clipboard_receiver = Some(ClipboardReceiver::new(
                client.client.handle.clone(),
                client.summary.clone(),
            ));
        }
        let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        assert!(
            client
                .run_clipboard_foreground(
                    &mut terminal,
                    Size::new(80, 24).unwrap(),
                    Duration::from_secs(1),
                    std::future::pending(),
                    crate::host::terminal::HostClipboard::disabled(),
                )
                .await
                .is_err()
        );
        assert_eq!(terminal.presentation_entries, 0);
        let mut bytes = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::io::AsyncReadExt::read_to_end(&mut peer, &mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(bytes.is_empty());
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}
