//! Local-only transport lifetime checks; no application authority or sessions.
//!
//! Loopback peers prove independent QUIC connection ownership under one locked
//! persistent identity. Synthetic payloads carry no user configuration secrets.

use super::*;
use std::os::unix::fs::PermissionsExt;

/// Frontend discovery must reject a relocated root without recreating the old
/// pathname, and a replacement private directory cannot inherit the retained
/// endpoint's identity. Restoring the original object permits discovery again.
#[tokio::test]
async fn outbound_endpoint_root_revalidation_is_read_only_and_object_scoped() {
    let root =
        std::env::temp_dir().join(format!("mez-outbound-root-{:032x}", rand::random::<u128>()));
    let moved = root.with_extension("moved");
    let owner = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    assert_eq!(owner.frontend_config_root().unwrap(), root);
    std::fs::rename(&root, &moved).unwrap();
    assert!(owner.frontend_config_root().is_err());
    assert!(
        !root.exists(),
        "revalidation must not recreate a missing root"
    );
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(owner.frontend_config_root().is_err());
    std::fs::remove_dir(&root).unwrap();
    std::fs::rename(&moved, &root).unwrap();
    assert_eq!(owner.frontend_config_root().unwrap(), root);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(owner.frontend_config_root().is_err());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    owner.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Isolated process fixture for intentionally quarantined lock descriptors.
/// It publishes only a static readiness marker after abandoning ownership and
/// waits for parent input, keeping the fail-closed descriptor process-scoped.
#[tokio::test]
#[ignore = "subprocess helper for outbound endpoint quarantine regression"]
async fn outbound_endpoint_quarantine_child() {
    use std::io::{Read, Write};
    let root = std::path::PathBuf::from(std::env::var_os("MEZ_OUTBOUND_TEST_ROOT").unwrap());
    let mode = std::env::var("MEZ_OUTBOUND_TEST_MODE").unwrap();
    let owner = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    if mode == "shutdown" {
        let mut shutdown = owner.begin_shutdown().unwrap();
        shutdown.work = Some(Box::pin(std::future::pending()));
        let mut wait = Box::pin(shutdown.finish());
        assert!(matches!(
            futures_util::poll!(&mut wait),
            std::task::Poll::Pending
        ));
        drop(wait);
        drop(shutdown);
    } else {
        assert_eq!(mode, "owner");
        drop(owner);
    }
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    println!("OUTBOUND_QUARANTINE_READY");
    std::io::stdout().flush().unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
}

/// Last-owner disposal and abandoned shutdown must not release the key while
/// their process survives. Only process exit releases quarantined ownership;
/// the same persisted public identity then becomes safely acquirable again.
#[tokio::test]
async fn outbound_endpoint_abandoned_resources_quarantine_until_process_exit() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    for mode in ["owner", "shutdown"] {
        let root = std::env::temp_dir().join(format!(
            "mez-outbound-quarantine-{:032x}",
            rand::random::<u128>()
        ));
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "host::outbound_endpoint::tests::outbound_endpoint_quarantine_child",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("MEZ_OUTBOUND_TEST_ROOT", &root)
            .env("MEZ_OUTBOUND_TEST_MODE", mode)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
            loop {
                let line = output
                    .next_line()
                    .await
                    .unwrap()
                    .expect("quarantine helper exited before readiness");
                if line == "OUTBOUND_QUARANTINE_READY" {
                    break;
                }
            }
            assert!(RemoteClientIdentity::load_or_create(&root).is_err());
            child.stdin.take().unwrap().write_all(b"x").await.unwrap();
            assert!(child.wait().await.unwrap().success());
        })
        .await
        .unwrap();
        drop(RemoteClientIdentity::load_or_create(&root).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A real connection has just entered draining when exclusive shutdown starts.
/// A deterministic gate models stalled teardown: both a cancelled wait and a
/// deadline leave the original future and lock owned. Releasing the gate allows
/// that same close to complete, after which the identity may be acquired again.
/// This injects a stalled lifecycle boundary, not an actual lost UDP close ACK.
#[tokio::test]
async fn outbound_endpoint_shutdown_wait_retains_lock_until_completion() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-shutdown-{:032x}",
        rand::random::<u128>()
    ));
    let policy = RuntimeIrohTransportPolicy {
        setup_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    let owner = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let identity = owner.endpoint_id();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .alpns(vec![b"mez-owner-test".to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let (lease, remote) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(owner.connect(server.addr(), b"mez-owner-test"), async {
            server.accept().await.unwrap().await.unwrap()
        })
    })
    .await
    .unwrap();
    let lease = lease.unwrap();
    assert_eq!(remote.remote_id(), identity);
    drop(lease);
    let mut shutdown = owner.begin_shutdown().unwrap();
    let (release, gate) = tokio::sync::oneshot::channel();
    let original = shutdown.work.take().unwrap();
    shutdown.work = Some(Box::pin(async move {
        gate.await.unwrap();
        original.await
    }));
    {
        let mut wait = Box::pin(shutdown.finish());
        assert!(matches!(
            futures_util::poll!(&mut wait),
            std::task::Poll::Pending
        ));
        // Dropping this borrowed wait does not discard the owned close future.
    }
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    shutdown.resource.as_mut().unwrap().setup_timeout = Duration::from_millis(100);
    assert!(shutdown.finish().await.is_err());
    assert!(shutdown.resource.as_ref().unwrap().identity.is_some());
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    release.send(()).unwrap();
    shutdown.resource.as_mut().unwrap().setup_timeout = Duration::from_secs(10);
    shutdown.finish().await.unwrap();
    shutdown.finish().await.unwrap();
    assert!(shutdown.resource.is_none());
    assert!(shutdown.work.is_none());
    let restored = RemoteClientIdentity::load_or_create(&root).unwrap();
    assert_eq!(restored.endpoint_id(), identity);
    drop(shutdown);
    drop(restored);
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Two leases share one public endpoint identity but use independent QUIC
/// connections. Capacity is finite; closing a sibling leaves the other usable,
/// and a busy owner cannot shut down the endpoint or release its identity lock.
#[tokio::test]
async fn outbound_endpoint_leases_are_bounded_independent_and_identity_locked() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-owner-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = RuntimeIrohTransportPolicy {
        max_connections: 2,
        setup_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    let owner = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let identity = owner.endpoint_id();
    assert_eq!(owner.clone().endpoint_id(), identity);
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .alpns(vec![b"mez-owner-test".to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let accept = async {
        let first = server.accept().await.unwrap().await.unwrap();
        let second = server.accept().await.unwrap().await.unwrap();
        (first, second)
    };
    let connect = async {
        let first = owner
            .connect(server.addr(), b"mez-owner-test")
            .await
            .unwrap();
        let second = owner
            .connect(server.addr(), b"mez-owner-test")
            .await
            .unwrap();
        (first, second)
    };
    let ((first, second), (remote_first, remote_second)) =
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(connect, accept)
        })
        .await
        .unwrap();
    assert_eq!(remote_first.remote_id(), identity);
    assert_eq!(remote_second.remote_id(), identity);
    assert_ne!(
        first.connection().stable_id(),
        second.connection().stable_id()
    );
    let full = owner
        .connect(server.addr(), b"mez-owner-test")
        .await
        .err()
        .unwrap();
    assert_eq!(full.kind(), MezErrorKind::RateLimited);
    assert!(owner.clone().begin_shutdown().is_err());
    drop(first);
    let send = async {
        let (mut send, mut recv) = second.connection().open_bi().await.unwrap();
        send.write_all(b"sibling").await.unwrap();
        send.finish().unwrap();
        assert_eq!(recv.read_to_end(32).await.unwrap(), b"alive");
    };
    let reply = async {
        let (mut send, mut recv) = remote_second.accept_bi().await.unwrap();
        assert_eq!(recv.read_to_end(32).await.unwrap(), b"sibling");
        send.write_all(b"alive").await.unwrap();
        send.finish().unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(send, reply) })
        .await
        .unwrap();
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    drop(second);
    owner.begin_shutdown().unwrap().finish().await.unwrap();
    let restored = RemoteClientIdentity::load_or_create(&root).unwrap();
    assert_eq!(restored.endpoint_id(), identity);
    drop(restored);
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Disabled policy rejects before protected key creation, so an explicit
/// outbound veto cannot be bypassed by the transport-resource constructor.
#[tokio::test]
async fn outbound_endpoint_disabled_policy_performs_no_key_io() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-disabled-{:032x}",
        rand::random::<u128>()
    ));
    let policy = RuntimeIrohTransportPolicy {
        outbound_enabled: false,
        ..Default::default()
    };
    assert!(OutboundEndpointOwner::bind(&root, &policy).await.is_err());
    assert!(!root.exists());
}

/// A polled handshake owns a capacity slot, but cancelling its future releases
/// that slot without starting detached application work or releasing the key
/// lock. An owner may be dropped while a sibling clone retains the endpoint.
#[tokio::test]
async fn outbound_endpoint_cancelled_attempt_releases_only_its_slot() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-cancel-{:032x}",
        rand::random::<u128>()
    ));
    let policy = RuntimeIrohTransportPolicy {
        max_connections: 1,
        setup_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    let owner = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let sibling = owner.clone();
    let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = iroh::EndpointAddr::new(iroh::SecretKey::generate().public())
        .with_ip_addr(blackhole.local_addr().unwrap());
    let mut attempt = Box::pin(owner.connect(target.clone(), b"mez-owner-test"));
    assert!(matches!(
        futures_util::poll!(&mut attempt),
        std::task::Poll::Pending
    ));
    assert_eq!(owner.inner.slots.available_permits(), 0);
    let full = sibling
        .connect(target, b"mez-owner-test")
        .await
        .err()
        .unwrap();
    assert_eq!(full.kind(), MezErrorKind::RateLimited);
    drop(attempt);
    assert_eq!(owner.inner.slots.available_permits(), 1);
    drop(owner);
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    sibling.begin_shutdown().unwrap().finish().await.unwrap();
    drop(RemoteClientIdentity::load_or_create(&root).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

/// Direct callers cannot bypass configured resource bounds with an oversized
/// semaphore or deadline. Invalid budgets fail before any protected key I/O.
#[tokio::test]
async fn outbound_endpoint_invalid_budgets_perform_no_key_io() {
    for (connections, deadline) in [
        (0, Duration::from_secs(10)),
        (1025, Duration::from_secs(10)),
        (1, Duration::ZERO),
        (1, Duration::from_secs(121)),
    ] {
        let root = std::env::temp_dir().join(format!(
            "mez-outbound-budget-{:032x}",
            rand::random::<u128>()
        ));
        let policy = RuntimeIrohTransportPolicy {
            max_connections: connections,
            setup_timeout: deadline,
            ..Default::default()
        };
        assert!(OutboundEndpointOwner::bind(&root, &policy).await.is_err());
        assert!(!root.exists());
    }
}
