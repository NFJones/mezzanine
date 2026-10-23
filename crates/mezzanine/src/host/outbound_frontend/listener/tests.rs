//! Real local-socket publication and cleanup tests, without remote authority.
//!
//! The endpoint caller retains graceful teardown. Listener tests exercise
//! authenticated admission, stalled sibling independence, and held-parent cleanup.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use crate::runtime::RuntimeIrohTransportPolicy;
use futures_util::{SinkExt, StreamExt};
use std::os::unix::fs::PermissionsExt;
use tokio_util::codec::Framed;

/// Cleanup must require both device and inode equality, even when the other
/// component still matches a real socket. Synthetic stale evidence isolates
/// each guard without depending on multiple mounted devices or inode reuse.
#[test]
fn outbound_frontend_cleanup_preserves_device_or_inode_mismatch() {
    use std::os::unix::fs::MetadataExt;
    for change_device in [true, false] {
        let root =
            std::env::temp_dir().join(format!("mez-clean-id-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join(SOCKET_NAME);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let publication = SocketPublication {
            directory: std::fs::File::open(&root).unwrap(),
            path: path.clone(),
            device: metadata.dev() ^ u64::from(change_device),
            inode: metadata.ino() ^ u64::from(!change_device),
        };
        drop(publication);
        let current = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (metadata.dev(), metadata.ino())
        );
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// The real supervisor must advance a later hello while an earlier peer is
/// silent. Cancellation disposes both owned pipelines and releases capacity;
/// listener disposal and graceful endpoint shutdown remain caller-owned.
#[tokio::test]
async fn outbound_frontend_supervisor_cancels_owned_stalled_pipelines() {
    let root = std::env::temp_dir().join(format!("mez-supervisor-{:032x}", rand::random::<u128>()));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 2, std::time::Duration::from_secs(2))
            .unwrap();
    let path = listener.socket_path().unwrap().to_path_buf();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let serve = listener.serve(async move { server_stop.notified().await });
    let client_work = async {
        let _silent = tokio::net::UnixStream::connect(&path).await.unwrap();
        let client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let mut client = Framed::new(
            client,
            ProtocolFrameCodec::new(super::super::HELLO_LIMIT).unwrap(),
        );
        client
            .send(ProtocolFrame::new(
                super::super::CONTENT_TYPE,
                serde_json::json!({"protocol":super::super::PROTOCOL}).to_string(),
            ))
            .await
            .unwrap();
        let response = client.next().await.unwrap().unwrap();
        assert!(response.body.contains("generation"));
        stop.notify_one();
        assert!(client.next().await.is_none());
    };
    let (served, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(serve, client_work)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 2);
    assert_eq!(listener.admission.slots.available_permits(), 2);
    drop(listener);
    assert!(!path.exists());
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Publication must preserve live and non-socket entries. A rejected second
/// listener does not release or replace the endpoint owner or its first socket.
#[tokio::test]
async fn outbound_frontend_listener_conflicts_preserve_existing_entries() {
    let root = std::env::temp_dir().join(format!(
        "mez-listener-conflict-{:032x}",
        rand::random::<u128>()
    ));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 2, std::time::Duration::from_secs(2))
            .unwrap();
    let path = listener.socket_path().unwrap().to_path_buf();
    assert!(
        OutboundFrontendListener::bind(endpoint.clone(), 2, std::time::Duration::from_secs(2))
            .is_err()
    );
    assert!(path.exists());
    drop(listener);
    std::fs::write(&path, b"authored entry").unwrap();
    assert!(
        OutboundFrontendListener::bind(endpoint.clone(), 2, std::time::Duration::from_secs(2))
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"authored entry");
    std::fs::remove_file(&path).unwrap();
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// A silent first peer must not serialize acceptance or admission of a later
/// peer. Cancelling its handshake releases only its capacity, and listener drop
/// removes its socket without releasing the separately retained endpoint.
#[tokio::test]
async fn outbound_frontend_listener_admits_while_sibling_hello_stalls() {
    let root = std::env::temp_dir().join(format!("mez-listener-{:032x}", rand::random::<u128>()));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 2, std::time::Duration::from_secs(2))
            .unwrap();
    let path = listener.socket_path().unwrap().to_path_buf();
    assert_eq!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let _silent = tokio::net::UnixStream::connect(&path).await.unwrap();
    let accepted = listener.accept().await.unwrap();
    let mut stalled = Box::pin(listener.admit(accepted));
    assert!(matches!(
        futures_util::poll!(&mut stalled),
        std::task::Poll::Pending
    ));
    let client = tokio::net::UnixStream::connect(&path).await.unwrap();
    let accepted = listener.accept().await.unwrap();
    let client = async {
        let mut client = Framed::new(
            client,
            ProtocolFrameCodec::new(super::super::HELLO_LIMIT).unwrap(),
        );
        client
            .send(ProtocolFrame::new(
                super::super::CONTENT_TYPE,
                serde_json::json!({"protocol":super::super::PROTOCOL}).to_string(),
            ))
            .await
            .unwrap();
        let response = client.next().await.unwrap().unwrap();
        assert!(response.body.contains("generation"));
        client
    };
    let (frontend, client) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(listener.admit(accepted), client)
    })
    .await
    .unwrap();
    let frontend = frontend.unwrap();
    assert!(endpoint.clone().begin_shutdown().is_err());
    drop(stalled);
    drop(frontend);
    drop(client);
    drop(listener);
    assert!(!path.exists());
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Cleanup through the held parent must remove only its own socket even when
/// the root moves. A new socket at the same name remains untouched by an older
/// publication's disposal; no stale owner may unlink a replacement entry.
#[tokio::test]
async fn outbound_frontend_listener_cleanup_preserves_replaced_socket() {
    for replace in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "mez-listener-clean-{:032x}",
            rand::random::<u128>()
        ));
        let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
            .await
            .unwrap();
        let listener =
            OutboundFrontendListener::bind(endpoint.clone(), 2, std::time::Duration::from_secs(2))
                .unwrap();
        let path = listener.socket_path().unwrap().to_path_buf();
        let moved = root.with_extension("moved");
        std::fs::rename(&root, &moved).unwrap();
        assert!(listener.socket_path().is_err());
        let actual = moved.join(SOCKET_NAME);
        let replacement = if replace {
            std::fs::remove_file(&actual).unwrap();
            Some(std::os::unix::net::UnixListener::bind(&actual).unwrap())
        } else {
            None
        };
        drop(listener);
        assert_eq!(actual.exists(), replace);
        assert!(!path.exists());
        drop(replacement);
        if replace {
            std::fs::remove_file(&actual).unwrap();
        }
        std::fs::rename(&moved, &root).unwrap();
        endpoint.begin_shutdown().unwrap().finish().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
/// A failed request reports exactly one bounded stage/class line while an
/// expected pre-setup disconnect stays silent and a live sibling keeps working.
///
/// The malformed setup body carries a payload sentinel; the diagnostic must
/// never echo it. Supervision retires only the failed pipeline and does not
/// retry, replay input or disturb the sibling connection.
#[tokio::test]
async fn outbound_frontend_reports_bounded_pipeline_failures_without_payloads() {
    let _ = super::pipeline_diagnostics::take_reported_failures();
    let root = std::env::temp_dir().join(format!(
        "mez-listener-diagnostics-{:032x}",
        rand::random::<u128>()
    ));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 4, std::time::Duration::from_secs(5))
            .unwrap();
    let path = listener.socket_path().unwrap().to_path_buf();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let serve = listener.serve(async move { server_stop.notified().await });
    let client_work = async {
        let hello = || {
            ProtocolFrame::new(
                super::super::CONTENT_TYPE,
                serde_json::json!({"protocol": super::super::PROTOCOL}).to_string(),
            )
        };
        // 1. A malformed setup request carrying a payload sentinel retires only
        //    this pipeline and is reported once at the profile stage.
        let failed = tokio::net::UnixStream::connect(&path).await.unwrap();
        let mut failed = Framed::new(
            failed,
            ProtocolFrameCodec::new(super::super::HELLO_LIMIT).unwrap(),
        );
        failed.send(hello()).await.unwrap();
        assert!(
            failed
                .next()
                .await
                .unwrap()
                .unwrap()
                .body
                .contains("generation")
        );
        failed
            .send(ProtocolFrame::new(
                super::super::CONTENT_TYPE,
                "{\"broken\":\"SENTINEL-BROKER-PAYLOAD-7f3a\"}".to_string(),
            ))
            .await
            .unwrap();
        assert!(failed.next().await.is_none());

        // 2. A peer that leaves before sending setup is expected lifecycle and
        //    must not be reported as a failed request.
        let quiet = tokio::net::UnixStream::connect(&path).await.unwrap();
        let mut quiet = Framed::new(
            quiet,
            ProtocolFrameCodec::new(super::super::HELLO_LIMIT).unwrap(),
        );
        quiet.send(hello()).await.unwrap();
        assert!(
            quiet
                .next()
                .await
                .unwrap()
                .unwrap()
                .body
                .contains("generation")
        );
        drop(quiet);

        // 3. A sibling admitted after the failure still completes its hello.
        let sibling = tokio::net::UnixStream::connect(&path).await.unwrap();
        let mut sibling = Framed::new(
            sibling,
            ProtocolFrameCodec::new(super::super::HELLO_LIMIT).unwrap(),
        );
        sibling.send(hello()).await.unwrap();
        assert!(
            sibling
                .next()
                .await
                .unwrap()
                .unwrap()
                .body
                .contains("generation")
        );
        drop(sibling);
        stop.notify_one();
    };
    let (served, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(serve, client_work)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 3);

    let reported = super::pipeline_diagnostics::take_reported_failures();
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(reported[0].contains("stage=profile"), "{}", reported[0]);
    assert!(
        reported[0].contains("class=invalid_args"),
        "{}",
        reported[0]
    );
    assert!(!reported[0].contains("SENTINEL"), "{}", reported[0]);
    assert!(!reported[0].contains("broken"), "{}", reported[0]);

    drop(listener);
    assert!(!path.exists());
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
