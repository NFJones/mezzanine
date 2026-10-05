//! Consumed setup qualification over authenticated Unix streams and private profiles.
//!
//! These tests never dial a peer or create a session. Device credentials stay in
//! owner-side profile state, and rejected requests cannot retarget local handles.

use super::*;
use crate::runtime::RuntimeIrohTransportPolicy;
use crate::security::remote::RemoteRoleCeiling;
use secrecy::SecretString;

/// A held private profile lock deterministically stalls worker I/O beyond the
/// setup deadline. Cancellation of its waiter must not permit another worker
/// or endpoint shutdown until that original read actually finishes.
#[tokio::test]
async fn outbound_frontend_setup_timeout_retains_worker_capacity() {
    use rustix::fs::{FlockOperation, flock};
    use std::os::unix::fs::OpenOptionsExt;
    let (root, endpoint, admission, frontend, mut client) = fixture().await;
    let profile = RemoteClientProfile {
        name: "paired".into(),
        server_addr: iroh::EndpointAddr::new(iroh::SecretKey::generate().public()),
        role: RemoteRoleCeiling::Observer,
        scope: RemoteClientProfileScope::Host,
        device_credential: SecretString::from("worker-owned-proof".to_string()),
    };
    RemoteClientProfileStore::under_config_root(&root)
        .save(&profile)
        .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join("remote/client/profiles.lock"))
        .unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();
    client
        .send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({
                "handle":frontend.handle(), "profile":"paired", "initialize": {
                    "client_name":"frontend", "requested_version":3,
                    "requested_role":"observer", "session_intent":"host_only"
                }
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert!(frontend.prepare(Duration::from_millis(100)).await.is_err());
    assert_eq!(admission.slots.available_permits(), 0);
    assert!(endpoint.clone().begin_shutdown().is_err());
    let (server, _other) = tokio::net::UnixStream::pair().unwrap();
    assert_eq!(
        admission.admit(server).await.err().unwrap().kind(),
        MezErrorKind::RateLimited
    );
    flock(&lock, FlockOperation::Unlock).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while admission.slots.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(lock);
    drop(client);
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Builds a real authenticated frontend with independently retained teardown.
async fn fixture() -> (
    std::path::PathBuf,
    OutboundEndpointOwner,
    OutboundFrontendAdmission,
    AdmittedFrontend,
    Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
) {
    let root = std::env::temp_dir().join(format!("mez-setup-{:032x}", rand::random::<u128>()));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 1, Duration::from_secs(2)).unwrap();
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let mut client = Framed::new(client, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
    client
        .send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({"protocol":PROTOCOL}).to_string(),
        ))
        .await
        .unwrap();
    let frontend = admission.admit(server).await.unwrap();
    client.next().await.unwrap().unwrap();
    (root, endpoint, admission, frontend, client)
}

/// Protected aliases supply role/scope evidence, while Create retains its exact
/// invocation key. Caller credentials, stale handles, legacy creation and role
/// escalation reject before any network work. Failed streams release capacity.
#[tokio::test]
async fn outbound_frontend_setup_is_profile_bound_and_credential_free() {
    for case in [
        "valid",
        "credentials",
        "stale",
        "legacy",
        "ceiling",
        "missing-key",
    ] {
        let (root, endpoint, admission, frontend, mut client) = fixture().await;
        let profile = RemoteClientProfile {
            name: "paired".into(),
            server_addr: iroh::EndpointAddr::new(iroh::SecretKey::generate().public()),
            role: if case == "ceiling" {
                RemoteRoleCeiling::Observer
            } else {
                RemoteRoleCeiling::Primary
            },
            scope: if case == "legacy" {
                RemoteClientProfileScope::LegacySession
            } else {
                RemoteClientProfileScope::Host
            },
            device_credential: SecretString::from("owner-only-device-proof".to_string()),
        };
        RemoteClientProfileStore::under_config_root(&root)
            .save(&profile)
            .unwrap();
        let mut handle = frontend.handle().clone();
        if case == "stale" {
            handle.generation += 1;
        }
        let mut initialize = serde_json::json!({"client_name":"frontend","requested_version":3,
            "requested_role":"primary","session_intent":"create","idempotency_key":"exact-invocation",
            "client":{"name":"frontend","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}});
        if case == "credentials" {
            initialize["authentication"] = serde_json::json!({"token":"must-not-enter-owner"});
        }
        if case == "missing-key" {
            initialize
                .as_object_mut()
                .unwrap()
                .remove("idempotency_key");
        }
        client
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({"handle":handle,"profile":"paired","initialize":initialize})
                    .to_string(),
            ))
            .await
            .unwrap();
        let result = frontend.prepare(Duration::from_secs(2)).await;
        if case == "valid" {
            let prepared = result.unwrap();
            assert_eq!(prepared.initialize["idempotency_key"], "exact-invocation");
            assert!(prepared.initialize.get("authentication").is_none());
            assert_eq!(prepared.profile.name, "paired");
            assert_eq!(prepared.frontend.handle(), &handle);
            assert!(!format!("{:?}", prepared.profile).contains("owner-only-device-proof"));
            assert!(endpoint.clone().begin_shutdown().is_err());
            drop(prepared);
        } else {
            assert!(result.is_err(), "case={case}");
        }
        assert_eq!(admission.slots.available_permits(), 1);
        drop(client);
        drop(admission);
        endpoint.begin_shutdown().unwrap().finish().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
