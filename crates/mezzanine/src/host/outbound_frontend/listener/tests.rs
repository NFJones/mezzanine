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
