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
    };
    (root, listener, peer, client)
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
