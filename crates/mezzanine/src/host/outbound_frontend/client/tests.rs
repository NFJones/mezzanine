//! Local readiness qualification without remote connections or session creation.
//!
//! Real listener negotiation proves readiness and retained stream ownership;
//! rejection cases must remain read-only and cannot launch replacement owners.

use super::*;
use crate::runtime::RuntimeIrohTransportPolicy;
use std::os::unix::fs::{PermissionsExt, symlink};

/// Either half of the retained identity must independently reject discovery.
/// A real private socket remains untouched while synthetic stale evidence tests
/// device-only and inode-only mismatches without needing another filesystem.
#[test]
fn outbound_discovery_rejects_device_or_inode_mismatch_read_only() {
    let root = std::env::temp_dir().join(format!("mez-disc-id-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join(SOCKET_NAME);
    let socket = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut discovery = Discovery::capture(&root).unwrap();
    let original = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!(discovery.device, original.dev());
    assert_eq!(discovery.inode, original.ino());
    discovery.device ^= 1;
    assert!(discovery.validate().is_err());
    discovery.device ^= 1;
    discovery.inode ^= 1;
    assert!(discovery.validate().is_err());
    discovery.inode ^= 1;
    discovery.validate().unwrap();
    let current = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!(
        (current.dev(), current.ino()),
        (original.dev(), original.ino())
    );
    drop(discovery);
    drop(socket);
    std::fs::remove_dir_all(root).unwrap();
}

/// Real readiness returns distinct exact handles, not merely socket existence.
/// Root relocation invalidates the retained client without recreating its path.
#[tokio::test]
async fn outbound_readiness_authenticates_and_retains_exact_streams() {
    let root = std::env::temp_dir().join(format!("mez-readiness-{:032x}", rand::random::<u128>()));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 2, Duration::from_secs(2)).unwrap();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let serve = listener.serve(async move { server_stop.notified().await });
    let clients = async {
        let first = OutboundFrontendClient::connect(&root, Duration::from_secs(2))
            .await
            .unwrap();
        let mut second = OutboundFrontendClient::connect(&root, Duration::from_secs(2))
            .await
            .unwrap();
        assert_ne!(first.handle().unwrap(), second.handle().unwrap());
        assert!(endpoint.clone().begin_shutdown().is_err());
        let moved = root.with_extension("moved");
        std::fs::rename(&root, &moved).unwrap();
        assert!(second.handle().is_err());
        assert!(!root.exists());
        std::fs::rename(&moved, &root).unwrap();
        second.handle().unwrap();
        drop(first);
        stop.notify_one();
        assert!(second.stream.next().await.is_none());
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(serve, clients)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 2);
    drop(listener);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Missing, symlinked and permissive discovery paths reject without creating
/// identity material, unlinking entries or starting a replacement broker.
#[tokio::test]
async fn outbound_readiness_rejects_unsafe_or_missing_paths_read_only() {
    let root = std::env::temp_dir().join(format!(
        "mez-readiness-missing-{:032x}",
        rand::random::<u128>()
    ));
    assert!(
        OutboundFrontendClient::connect(&root, Duration::from_secs(1))
            .await
            .is_err()
    );
    assert!(!root.exists());
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.join("authored");
    std::fs::write(&target, b"preserve").unwrap();
    symlink(&target, root.join(SOCKET_NAME)).unwrap();
    assert!(
        OutboundFrontendClient::connect(&root, Duration::from_secs(1))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
    std::fs::remove_file(root.join(SOCKET_NAME)).unwrap();
    let socket = std::os::unix::net::UnixListener::bind(root.join(SOCKET_NAME)).unwrap();
    std::fs::set_permissions(
        root.join(SOCKET_NAME),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(
        OutboundFrontendClient::connect(&root, Duration::from_secs(1))
            .await
            .is_err()
    );
    drop(socket);
    std::fs::remove_dir_all(root).unwrap();
}
