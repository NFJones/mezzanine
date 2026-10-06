//! Foreground composition tests without remote sessions or provider operations.
//!
//! Readiness comes from real authenticated hello admission. Explicit cancellation
//! must remove the private socket and release the identity after completed close.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use crate::security::remote::RemoteClientIdentity;
use futures_util::{SinkExt, StreamExt};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio_util::codec::Framed;

/// The running-binary selector must reuse authenticated readiness without
/// spawning another identity owner. This test serves the real broker in-process;
/// no test executable is launched and no remote session is allocated.
#[tokio::test]
async fn outbound_broker_cli_selector_reuses_ready_owner() {
    let home = std::env::temp_dir().join(format!("mez-selector-{:032x}", rand::random::<u128>()));
    let env = CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let socket = paths.root().join("outbound.sock");
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let broker_stop = stop.clone();
    let broker = run(&env, async move { broker_stop.notified().await });
    let client = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !socket.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut child = None;
        let first = connect_cli(&env, Duration::from_secs(2), &mut child)
            .await
            .unwrap();
        let second = connect_cli(&env, Duration::from_secs(2), &mut child)
            .await
            .unwrap();
        assert!(
            child.is_none(),
            "ready broker must not spawn a competing owner"
        );
        assert_ne!(first.handle().unwrap(), second.handle().unwrap());
        drop(first);
        drop(second);
        stop.notify_one();
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(broker, client)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 2);
    assert!(!socket.exists());
    drop(RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// Cancellation with a contended profile read must preserve the shutdown owner
/// until that worker retires. The held lock models another live administrator;
/// release after cancellation must permit completed teardown and identity reuse.
#[tokio::test]
async fn outbound_broker_cancellation_retires_contended_profile_worker() {
    cancellation_with_profile_lock(true).await;
}

/// A profile administrator retaining its lock cannot block outbound shutdown
/// indefinitely. Nonblocking acquisition expires, releases worker ownership,
/// and permits graceful endpoint teardown while the profile lock is still held.
#[tokio::test]
async fn outbound_broker_cancellation_bounds_permanently_held_profile_lock() {
    cancellation_with_profile_lock(false).await;
}

/// Shares only the two cancellation cases' exact local fixture. No remote
/// connect or profile-content retry occurs; the caller retains the lock guard.
async fn cancellation_with_profile_lock(release_after_cancel: bool) {
    use crate::security::remote::{
        RemoteClientProfile, RemoteClientProfileScope, RemoteClientProfileStore, RemoteRoleCeiling,
    };
    use rustix::fs::{FlockOperation, flock};
    let home =
        std::env::temp_dir().join(format!("mez-broker-worker-{:032x}", rand::random::<u128>()));
    let env = CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    RemoteClientProfileStore::under_config_root(paths.root())
        .save(&RemoteClientProfile {
            name: "blocked".into(),
            server_addr: iroh::EndpointAddr::new(iroh::SecretKey::generate().public()),
            role: RemoteRoleCeiling::Observer,
            scope: RemoteClientProfileScope::Host,
            device_credential: secrecy::SecretString::from("fixture-only".to_string()),
        })
        .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.root().join("remote/client/profiles.lock"))
        .unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();
    let socket = paths.root().join("outbound.sock");
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let broker = run(&env, async move { server_stop.notified().await });
    let client = async {
        let stream = loop {
            if let Ok(stream) = tokio::net::UnixStream::connect(&socket).await {
                break stream;
            }
            tokio::task::yield_now().await;
        };
        let mime = "application/vnd.mezzanine.outbound+json";
        let mut client = Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap());
        client
            .send(ProtocolFrame::new(mime, r#"{"protocol":"mez-outbound/1"}"#))
            .await
            .unwrap();
        let hello = client.next().await.unwrap().unwrap();
        let hello: serde_json::Value = serde_json::from_str(&hello.body).unwrap();
        client.send(ProtocolFrame::new(mime, serde_json::json!({
            "handle":hello["handle"],"profile":"blocked","initialize":{
                "client_name":"fixture","requested_version":3,"requested_role":"observer","session_intent":"host_only"
            }
        }).to_string())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.notify_one();
        assert!(client.next().await.is_none());
        if release_after_cancel {
            flock(&lock, FlockOperation::Unlock).unwrap();
        }
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(broker, client)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 1);
    assert!(!socket.exists());
    drop(RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    drop(lock);
    std::fs::remove_dir_all(home).unwrap();
}

/// Runs the actual foreground composition and cancels a silent setup pipeline.
/// Successful return proves caller-owned teardown releases the identity lock;
/// starting the owner never dials or allocates a remote session.
#[tokio::test]
async fn outbound_broker_foreground_cancellation_releases_publication_and_identity() {
    let home = std::env::temp_dir().join(format!("mez-broker-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    let socket = paths.root().join("outbound.sock");
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let broker = run(&env, async move { server_stop.notified().await });
    let client = async {
        let stream = loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(stream) => break stream,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("{error}"),
            }
        };
        let mut client = Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap());
        client
            .send(ProtocolFrame::new(
                "application/vnd.mezzanine.outbound+json",
                r#"{"protocol":"mez-outbound/1"}"#,
            ))
            .await
            .unwrap();
        let hello = client.next().await.unwrap().unwrap();
        assert!(hello.body.contains("generation"));
        assert!(RemoteClientIdentity::load_or_create(paths.root()).is_err());
        stop.notify_one();
        assert!(client.next().await.is_none());
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(broker, client)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 1);
    assert!(!socket.exists());
    drop(RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// The foreground entry respects an explicit outbound veto before creating
/// identity material or a listener. The authored config remains unchanged.
#[tokio::test]
async fn outbound_broker_veto_precedes_identity_creation() {
    let home =
        std::env::temp_dir().join(format!("mez-broker-veto-{:032x}", rand::random::<u128>()));
    let env = CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    let file = paths.ensure_default_config().unwrap();
    let source = format!(
        "version = {}\n[transport.iroh]\noutbound_enabled = false\n",
        crate::config::CURRENT_CONFIG_SCHEMA_VERSION
    );
    std::fs::write(&file, &source).unwrap();
    assert!(run(&env, std::future::ready(())).await.is_err());
    let mut child = None;
    assert!(
        connect_cli(&env, Duration::from_secs(1), &mut child)
            .await
            .is_err()
    );
    assert!(child.is_none());
    assert!(!paths.root().join("remote/client/endpoint.key").exists());
    assert!(!paths.root().join("outbound.sock").exists());
    assert_eq!(std::fs::read_to_string(file).unwrap(), source);
    std::fs::remove_dir_all(home).unwrap();
}
