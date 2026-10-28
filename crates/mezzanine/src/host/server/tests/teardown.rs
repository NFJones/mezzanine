//! Accepted host shutdown still completes safe teardown after durability failure.
//!
//! The host and all sessions are created inside one private fixture. Faults are
//! ordinary failures at the actual checkpoint destination, not fabricated native
//! receipts. Post-result observations precede fixture-owned cleanup, allowing a
//! failing regression to release its original resources before assertions panic.

use super::*;
use tokio::io::AsyncReadExt;

/// Installs a real destination-type failure while retaining the fixture's old
/// checkpoints for restoration before cleanup. Returns whether a directory moved.
fn break_checkpoint_destination(root: &Path) -> bool {
    let layouts = root.join("layouts");
    let had_layouts = layouts.exists();
    if had_layouts {
        fs::rename(&layouts, root.join("layouts-before-fault")).unwrap();
    }
    fs::write(layouts, b"owned checkpoint destination fault").unwrap();
    had_layouts
}

/// Restores only the original fixture-owned checkpoint path after observation.
fn restore_checkpoint_destination(root: &Path, had_layouts: bool) {
    fs::remove_file(root.join("layouts")).unwrap();
    if had_layouts {
        fs::rename(root.join("layouts-before-fault"), root.join("layouts")).unwrap();
    }
}

/// Replacing the owned layouts directory with a file makes the real strict
/// checkpoint fail. Accepted graceful shutdown must retain that durability error
/// while still stopping the exact hosted runtime. A second explicit fixture
/// cleanup proves resources remain owned even when testing the historical bug.
#[tokio::test(flavor = "current_thread")]
async fn host_teardown_checkpoint_failure_still_stops_owned_sessions() {
    let root = test_root("checkpoint-failure-teardown");
    let host = HostServer::bind(config(root.clone())).unwrap();
    let session = host
        .create_session(Some("checkpoint-fault".into()), Size::new(80, 24).unwrap())
        .await
        .unwrap();
    let had_layouts = break_checkpoint_destination(&root);
    let outcome =
        tokio::time::timeout(Duration::from_secs(8), host.serve(std::future::ready(()))).await;
    let snapshots = host.router.snapshots().await.unwrap();
    let live_after = snapshots.iter().any(|snapshot| {
        snapshot.session_id == session.session_id
            && !matches!(
                snapshot.state,
                SessionSupervisorState::Stopped | SessionSupervisorState::Failed
            )
    });
    // Restore only this fixture's destination before its exact final cleanup.
    restore_checkpoint_destination(&root, had_layouts);
    let cleanup = host.router.shutdown_all(true, Duration::from_secs(3)).await;
    drop(host);
    let _ = fs::remove_dir_all(root);
    cleanup.unwrap();
    let error = outcome.unwrap().unwrap_err();
    assert!(error.message().contains("checkpoint"), "{error}");
    assert!(
        !live_after,
        "strict checkpoint failure bypassed explicit owned shutdown"
    );
}

/// A held real admission lock cannot keep an accepted host stop waiting beyond
/// its original budget. No runtime is created in this negative case; releasing
/// the gate after return proves a newly granted lock did not supply success.
#[tokio::test(flavor = "current_thread")]
async fn host_teardown_bounds_admission_drain_without_renewing_budget() {
    let root = test_root("blocked-admission-drain");
    let mut settings = config(root.clone());
    settings.shutdown_timeout = Duration::from_millis(20);
    let host = HostServer::bind(settings).unwrap();
    let gate = host.router.hold_creation_for_tests().await;
    let outcome =
        tokio::time::timeout(Duration::from_secs(2), host.serve(std::future::ready(()))).await;
    drop(gate);
    let state = host.router.admission_state();
    drop(host);
    let _ = fs::remove_dir_all(root);
    let error = outcome.unwrap().unwrap_err();
    assert!(
        error.message().contains("admission drain") && error.message().contains("deadline"),
        "{error}"
    );
    assert_ne!(state.as_str(), "serving");
}

/// Failed required checkpoints must publish an error on the original completion
/// stream after actual owned runtime teardown. If that stream is already lost,
/// the durability cause must retain precedence and include publication failure;
/// neither case can produce a healthy completion receipt or replay a stop.
#[tokio::test(flavor = "current_thread")]
async fn host_teardown_failed_checkpoint_preserves_original_completion_verdict() {
    for connected in [true, false] {
        let root = test_root("checkpoint-completion-verdict");
        let host = HostServer::bind(config(root.clone())).unwrap();
        host.create_session(Some("completion-fault".into()), Size::new(80, 24).unwrap())
            .await
            .unwrap();
        let had_layouts = break_checkpoint_destination(&root);
        let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
        let mut peer = if connected {
            Some(peer)
        } else {
            drop(peer);
            None
        };
        let completion = HostShutdownCompletion {
            stream,
            id: json!("original-checkpoint-client"),
            deadline: tokio::time::Instant::now() + Duration::from_secs(3),
        };
        let connections = FuturesUnordered::<
            std::future::Pending<Option<(HostShutdownRequest, Option<HostShutdownCompletion>)>>,
        >::new();
        let outcome = tokio::time::timeout(
            Duration::from_secs(8),
            host.complete_shutdown(
                HostShutdownRequest { force: false },
                Some(completion),
                connections,
                crate::error::TeardownErrors::default(),
            ),
        )
        .await;
        let read = if let Some(peer) = peer.as_mut() {
            let mut bytes = Vec::new();
            let result = peer.read_to_end(&mut bytes).await;
            Some((result, bytes))
        } else {
            None
        };
        let snapshots = host.router.snapshots().await.unwrap();
        let live_after = snapshots.iter().any(|snapshot| {
            !matches!(
                snapshot.state,
                SessionSupervisorState::Stopped | SessionSupervisorState::Failed
            )
        });
        restore_checkpoint_destination(&root, had_layouts);
        let cleanup = host.router.shutdown_all(true, Duration::from_secs(3)).await;
        drop(host);
        let _ = fs::remove_dir_all(root);
        cleanup.unwrap();
        let error = outcome.unwrap().unwrap_err();
        assert!(error.message().contains("checkpoint"), "{error}");
        assert!(!live_after, "completion preceded owned runtime teardown");
        if let Some((result, bytes)) = read {
            result.unwrap();
            let (body, consumed) =
                crate::control::decode_control_frame(&bytes, HOST_CONTROL_MAX_CONTENT_LENGTH)
                    .unwrap();
            assert_eq!(consumed, bytes.len());
            let reply: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(reply["id"], "original-checkpoint-client");
            assert!(reply.get("error").is_some());
            assert!(reply.get("result").is_none());
        } else {
            assert!(
                error.message().contains("shutdown completion reply"),
                "{error}"
            );
        }
    }
}
