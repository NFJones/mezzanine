//! Foreground operation identity checks without terminal or remote effects.
//!
//! Logical mutations retain invocation-local identities rather than relying on
//! process IDs. Exhaustion rejects instead of reusing a completed operation.

use super::*;

/// Entry may emit a mode prefix before stalling. This fake records that entry
/// began, then remains pending until the caller cancels or its budget expires.
struct StalledEntry {
    started: std::sync::Arc<tokio::sync::Notify>,
    restores: usize,
}

impl AsyncAttachedTerminalIo for StalledEntry {
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> crate::host::async_runtime::AsyncTerminalIoFuture<
        'a,
        Vec<crate::host::terminal::AttachedTerminalFdReadiness>,
    > {
        Box::pin(std::future::pending())
    }

    fn read_input<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> crate::host::async_runtime::AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(std::future::pending())
    }

    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        _lines: &'a [String],
        _spans: &'a [Vec<mez_terminal::TerminalStyleSpan>],
        _modes: mez_mux::presentation::AttachedTerminalOutputModes,
    ) -> crate::host::async_runtime::AsyncTerminalIoFuture<'a, usize> {
        Box::pin(std::future::pending())
    }

    fn enter_presentation<'a>(
        &'a mut self,
    ) -> crate::host::async_runtime::AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            self.started.notify_one();
            std::future::pending().await
        })
    }

    fn restore_presentation<'a>(
        &'a mut self,
    ) -> crate::host::async_runtime::AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            self.restores += 1;
            Ok(())
        })
    }
}

/// A stalled entry must be cancellable and deadline-bound, retire the local
/// session, and attempt cleanup. Neither entry failure window can send an ACK.
#[tokio::test]
async fn outbound_foreground_stalled_entry_retires_before_restoration() {
    for cancel in [true, false] {
        let (root, listener, mut peer, client) = inert_client(vec![7]);
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut terminal = StalledEntry {
            started: started.clone(),
            restores: 0,
        };
        let cancellation = async move {
            started.notified().await;
            if !cancel {
                std::future::pending::<()>().await;
            }
        };
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            client.run_snapshot_foreground(
                &mut terminal,
                Size::new(80, 24).unwrap(),
                Duration::from_millis(100),
                cancellation,
            ),
        )
        .await
        .expect("entry must not bypass cancellation or deadline");
        assert_eq!(result.is_ok(), cancel);
        assert_eq!(terminal.restores, 1);
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

/// Builds only inert client ownership over a disposable local socket. No setup,
/// remote authentication or session work is reconstructed by this fixture.
fn inert_client(
    receipts: Vec<u64>,
) -> (
    std::path::PathBuf,
    std::os::unix::net::UnixListener,
    tokio::net::UnixStream,
    OutboundSessionClient,
) {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("mez-foreground-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("outbound.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
    let client = OutboundSessionClient {
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
        modes: mez_mux::presentation::AttachedTerminalOutputModes::default(),
        receipts,
        lines: vec!["exact snapshot".into()],
        events_negotiated: false,
        render_rate_limit_fps: None,
        view_identity: None,
        event_cutoff: None,
        snapshot_size: (80, 24),
        committed_view: None,
    };
    (root, listener, peer, client)
}

/// Idle animation deadlines must trigger a fresh snapshot even without redraw
/// events and while ordinary pacing is closed. This exercises retained IPC
/// ownership, not physical visibility or the full production attach scheduler.
#[tokio::test]
async fn outbound_foreground_idle_animation_bypasses_ordinary_pacing() {
    let (root, listener, peer, mut client) = inert_client(Vec::new());
    client.events_negotiated = true;
    client.render_rate_limit_fps = Some(2);
    client.modes.animation_refresh_interval_ms = 100;
    let handle = client.client.handle.clone();
    let summary = serde_json::to_value(&client.summary).unwrap();
    let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
    for _ in 0..3 {
        terminal.push_pending_input_read();
    }
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let foreground_stop = stop.clone();
    let foreground = client.run_snapshot_foreground(
        &mut terminal,
        Size::new(80, 24).unwrap(),
        Duration::from_secs(1),
        async move { foreground_stop.notified().await },
    );
    let responder = async {
        let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
        let frame = peer.next().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert_eq!(request["operation"], "events");
        tokio::time::sleep(Duration::from_millis(110)).await;
        peer.send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({
                "handle":handle,"session":summary,"action":"none","event_id":null
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let frame = peer.next().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert!(
            request.get("operation").is_none(),
            "idle animation must capture a fresh view"
        );
        assert_eq!(request["handle"], serde_json::to_value(&handle).unwrap());
        stop.notify_one();
        assert!(peer.next().await.is_none());
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(foreground, responder)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(terminal.written_frames.len(), 1);
    assert_eq!(terminal.presentation_restores, 1);
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// Ordinary redraw events retain one pending latest-state fetch until the
/// advertised interval elapses. Idle replies cannot erase that pending request,
/// and no snapshot may be captured before the observed cadence deadline.
#[tokio::test]
async fn outbound_foreground_ordinary_redraw_waits_for_server_cadence() {
    let (root, listener, peer, mut client) = inert_client(Vec::new());
    client.events_negotiated = true;
    client.render_rate_limit_fps = Some(2);
    let handle = client.client.handle.clone();
    let summary = serde_json::to_value(&client.summary).unwrap();
    let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
    for _ in 0..4 {
        terminal.push_pending_input_read();
    }
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let foreground_stop = stop.clone();
    let foreground = client.run_snapshot_foreground(
        &mut terminal,
        Size::new(80, 24).unwrap(),
        Duration::from_secs(1),
        async move { foreground_stop.notified().await },
    );
    let responder = async {
        let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
        let started = tokio::time::Instant::now();
        for (action, delay) in [("view", 0), ("none", 10), ("none", 510)] {
            let frame = peer.next().await.unwrap().unwrap();
            let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(
                request["operation"], "events",
                "ordinary snapshot must wait for cadence"
            );
            tokio::time::sleep(Duration::from_millis(delay)).await;
            peer.send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({
                    "handle":handle,"session":summary,"action":action,"event_id":7
                })
                .to_string(),
            ))
            .await
            .unwrap();
        }
        let frame = peer.next().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert!(
            request.get("operation").is_none(),
            "pending redraw survives idle replies"
        );
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert_eq!(request["columns"], 80);
        stop.notify_one();
        assert!(peer.next().await.is_none());
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(foreground, responder)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(terminal.written_frames.len(), 1);
    assert_eq!(terminal.presentation_restores, 1);
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// A primary geometry change must be admitted as an empty terminal step before
/// fetching its next view. A view request alone cannot resize authoritative
/// primary state. The mutation retains its exact key and does not forward input.
#[tokio::test]
async fn outbound_foreground_primary_resize_precedes_snapshot() {
    let (root, listener, peer, mut client) = inert_client(Vec::new());
    client.events_negotiated = true;
    let handle = client.client.handle.clone();
    let summary = serde_json::to_value(&client.summary).unwrap();
    let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
    for _ in 0..3 {
        terminal.push_pending_input_read();
    }
    terminal.push_terminal_size(Some(Size::new(100, 30).unwrap()));
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let foreground_stop = stop.clone();
    let foreground = client.run_snapshot_foreground(
        &mut terminal,
        Size::new(80, 24).unwrap(),
        Duration::from_secs(1),
        async move { foreground_stop.notified().await },
    );
    let responder = async {
        let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
        let poll = peer.next().await.unwrap().unwrap();
        let poll: serde_json::Value = serde_json::from_str(&poll.body).unwrap();
        assert_eq!(poll["operation"], "events");
        peer.send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({
                "handle":handle,"session":summary,"action":"none","event_id":null
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let mutation = peer.next().await.unwrap().unwrap();
        let mutation: serde_json::Value = serde_json::from_str(&mutation.body).unwrap();
        assert_eq!(
            mutation["operation"], "step",
            "resize must be admitted before view capture"
        );
        assert_eq!(mutation["handle"], serde_json::to_value(&handle).unwrap());
        assert_eq!(mutation["columns"], 100);
        assert_eq!(mutation["rows"], 30);
        assert_eq!(mutation["input_bytes"], serde_json::json!([]));
        assert!(!mutation["idempotency_key"].as_str().unwrap().is_empty());
        peer.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
            "handle":handle,"session":summary,"idempotency_key":mutation["idempotency_key"],
            "acknowledgement":{"input_bytes":0,"client_detached":false,"session_terminated":false}
        }).to_string())).await.unwrap();
        let view = peer.next().await.unwrap().unwrap();
        let view: serde_json::Value = serde_json::from_str(&view.body).unwrap();
        assert!(view.get("operation").is_none());
        assert_eq!(view["columns"], 100);
        assert_eq!(view["rows"], 30);
        stop.notify_one();
        assert!(peer.next().await.is_none());
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(foreground, responder)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(terminal.written_frames.len(), 1);
    assert_eq!(terminal.presentation_restores, 1);
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// Repeated negotiated idle replies must keep the initial committed frame rather
/// than redraw or fetch snapshots on each poll. Cancellation retires the same
/// stream and restores presentation without replaying setup or terminal input.
#[tokio::test]
async fn outbound_foreground_idle_events_keep_one_committed_frame() {
    let (root, listener, peer, mut client) = inert_client(Vec::new());
    client.events_negotiated = true;
    let handle = client.client.handle.clone();
    let summary = serde_json::to_value(&client.summary).unwrap();
    let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
    for _ in 0..4 {
        terminal.push_pending_input_read();
    }
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let foreground_stop = stop.clone();
    let foreground = client.run_snapshot_foreground(
        &mut terminal,
        Size::new(80, 24).unwrap(),
        Duration::from_secs(1),
        async move { foreground_stop.notified().await },
    );
    let responder = async {
        let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
        for _ in 0..3 {
            let frame = peer.next().await.unwrap().unwrap();
            assert_eq!(frame.content_type, CONTENT_TYPE);
            let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(
                request["operation"], "events",
                "idle replies must not fetch snapshots"
            );
            assert_eq!(request["handle"], serde_json::to_value(&handle).unwrap());
            peer.send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({
                    "handle":handle,"session":summary,"action":"none","event_id":null
                })
                .to_string(),
            ))
            .await
            .unwrap();
        }
        // Observe the next poll before cancelling: already-buffered request
        // bytes are permitted, but no reply or subsequent request is needed
        // to retire the consumed owner.
        let frame = peer.next().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert_eq!(request["operation"], "events");
        assert_eq!(request["handle"], serde_json::to_value(&handle).unwrap());
        stop.notify_one();
        assert!(
            peer.next().await.is_none(),
            "cancellation must retire the exact stream without another reply"
        );
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(foreground, responder)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(terminal.written_frames.len(), 1);
    assert_eq!(terminal.written_frames[0].lines, ["exact snapshot"]);
    assert_eq!(terminal.presentation_entries, 1);
    assert_eq!(terminal.presentation_restores, 1);
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// Input-first waits must settle the original event request before returning its
/// client. Event-first waits must leave queued terminal bytes unread. Neither
/// ordering may replay setup, submit input remotely, or lose stream ownership.
#[tokio::test]
async fn outbound_foreground_negotiated_wait_preserves_both_orderings() {
    for input_first in [true, false] {
        let (root, listener, peer, mut client) = inert_client(Vec::new());
        client.events_negotiated = true;
        let handle = client.client.handle.clone();
        let summary = serde_json::to_value(&client.summary).unwrap();
        let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        if !input_first {
            terminal.push_pending_input_read();
        }
        terminal.push_input(b"unread".to_vec());
        let responder = async {
            let mut peer = Framed::new(peer, ProtocolFrameCodec::new(SNAPSHOT_LIMIT).unwrap());
            let request = peer.next().await.unwrap().unwrap();
            assert_eq!(request.content_type, CONTENT_TYPE);
            let request: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(request["operation"], "events");
            assert_eq!(request["handle"], serde_json::to_value(&handle).unwrap());
            assert_eq!(request["wait_ms"], 25);
            peer.send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({
                    "handle":handle,"session":summary,"action":"none","event_id":null
                })
                .to_string(),
            ))
            .await
            .unwrap();
            peer
        };
        let ((client, input, action), mut peer) =
            tokio::time::timeout(Duration::from_secs(2), async {
                let (waited, peer) = tokio::join!(
                    wait::negotiated(client, &mut terminal, Duration::from_secs(1)),
                    responder
                );
                (waited.unwrap(), peer)
            })
            .await
            .unwrap();
        assert_eq!(action, AttachRenderAction::None);
        if input_first {
            assert_eq!(input, Some(b"unread".to_vec()));
        } else {
            assert_eq!(input, None);
            assert_eq!(terminal.read_input(512).await.unwrap(), b"unread");
        }
        assert_eq!(client.client.handle, handle);
        assert_eq!(serde_json::to_value(client.summary()).unwrap(), summary);
        drop(client);
        assert!(
            peer.next().await.is_none(),
            "no extra request or replay may follow settlement"
        );
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Cancellation before entry and failure of exact output commitment both restore
/// presentation once and retire the local connection. The failed writer cannot
/// report receipt commitment, so no remote acknowledgement may be emitted.
#[tokio::test]
async fn outbound_foreground_restores_on_cancellation_and_commit_failure() {
    for cancelled in [true, false] {
        let (root, listener, mut peer, client) = inert_client(vec![7]);
        let mut terminal = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
        let cancellation = async move {
            if !cancelled {
                std::future::pending::<()>().await;
            }
        };
        let result = client
            .run_snapshot_foreground(
                &mut terminal,
                Size::new(80, 24).unwrap(),
                Duration::from_secs(2),
                cancellation,
            )
            .await;
        assert_eq!(result.is_ok(), cancelled);
        assert_eq!(terminal.presentation_entries, usize::from(!cancelled));
        assert_eq!(terminal.presentation_restores, 1);
        assert_eq!(terminal.written_frames.len(), usize::from(!cancelled));
        let mut bytes = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read_to_end(&mut peer, &mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(bytes.is_empty(), "uncommitted output cannot emit an ACK");
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Keys distinguish consecutive logical operations and separate invocations.
/// The final sequence value remains valid, but exhaustion must not wrap or
/// modify the retained watermark and accidentally replay an older mutation.
#[test]
fn outbound_foreground_keys_are_distinct_and_exhaustion_is_transactional() {
    let mut sequence = 0;
    let first = next_key(1, &mut sequence).unwrap();
    let second = next_key(1, &mut sequence).unwrap();
    assert_ne!(first, second);
    assert_eq!(sequence, 2);
    assert_ne!(first, next_key(2, &mut 0).unwrap());
    sequence = u64::MAX - 1;
    let final_key = next_key(1, &mut sequence).unwrap();
    assert!(final_key.ends_with(&u64::MAX.to_string()));
    assert!(next_key(1, &mut sequence).is_err());
    assert_eq!(sequence, u64::MAX);
}
