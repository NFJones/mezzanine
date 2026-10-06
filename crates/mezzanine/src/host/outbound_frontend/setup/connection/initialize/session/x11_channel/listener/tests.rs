//! Dedicated publication/slot checks without X credentials or remote effects.
use super::*;
use crate::runtime::RuntimeIrohTransportPolicy;
use std::os::unix::fs::{PermissionsExt, symlink};

/// Pending accept and admitted streams hold the same finite pool. Cancellation
/// releases pending slots, saturation rejects immediately, and accepted streams
/// retain endpoint identity after the listener closes and removes publication.
#[tokio::test]
async fn outbound_x11_listener_bounds_admission_and_retains_endpoint() {
    let root = std::env::temp_dir().join(format!("mez-xlisten-{:016x}", rand::random::<u64>()));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let listener = X11FrontendListener::bind(endpoint.clone(), 1).unwrap();
    let path = listener.socket_path().unwrap().to_path_buf();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    {
        let mut pending = Box::pin(listener.accept());
        assert!(matches!(
            futures_util::poll!(&mut pending),
            std::task::Poll::Pending
        ));
        assert_eq!(listener.slots.available_permits(), 0);
    }
    assert_eq!(listener.slots.available_permits(), 1);
    let peer = tokio::net::UnixStream::connect(&path).await.unwrap();
    let accepted = listener.accept().await.unwrap();
    assert_eq!(listener.slots.available_permits(), 0);
    assert_eq!(
        listener.accept().await.err().unwrap().kind(),
        MezErrorKind::RateLimited
    );
    drop(listener);
    assert!(!path.exists());
    assert!(endpoint.clone().begin_shutdown().is_err());
    drop(accepted);
    drop(peer);
    endpoint
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Exclusive publication preserves existing regular/symlink/live socket entries.
/// Held-directory cleanup removes only its socket after root relocation, leaving
/// replacements untouched. Invalid limits cannot create discovery state.
#[tokio::test]
async fn outbound_x11_listener_publication_preserves_replacements() {
    for replace in [false, true] {
        let root = std::env::temp_dir().join(format!("mez-xclean-{:016x}", rand::random::<u64>()));
        let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
            .await
            .unwrap();
        let name = "fixture.sock".to_string();
        let path = root.join(&name);
        assert!(X11FrontendListener::bind_named(endpoint.clone(), 0, name.clone()).is_err());
        assert!(!path.exists());
        std::fs::write(&path, b"authored").unwrap();
        assert!(X11FrontendListener::bind_named(endpoint.clone(), 1, name.clone()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"authored");
        std::fs::remove_file(&path).unwrap();
        symlink("missing", &path).unwrap();
        assert!(X11FrontendListener::bind_named(endpoint.clone(), 1, name.clone()).is_err());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(&path).unwrap();
        let listener = X11FrontendListener::bind_named(endpoint.clone(), 1, name.clone()).unwrap();
        assert!(X11FrontendListener::bind_named(endpoint.clone(), 1, name.clone()).is_err());
        let moved = root.with_extension("moved");
        std::fs::rename(&root, &moved).unwrap();
        assert!(listener.socket_path().is_err());
        let replacement = if replace {
            std::fs::remove_file(moved.join(&name)).unwrap();
            Some(std::os::unix::net::UnixListener::bind(moved.join(&name)).unwrap())
        } else {
            None
        };
        drop(listener);
        assert_eq!(moved.join(&name).exists(), replace);
        drop(replacement);
        if replace {
            std::fs::remove_file(moved.join(&name)).unwrap();
        }
        std::fs::rename(&moved, &root).unwrap();
        endpoint
            .retire_and_shutdown()
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
