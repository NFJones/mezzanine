//! Protected invitation preparation failures and retained blocking-worker ownership.
//!
//! Disposable authenticated socket pairs never dial a host or redeem an invitation.
//! Rejection must preserve authored profiles; cancelled waiters cannot free a slot
//! while the original bounded profile-lock worker still retains endpoint ownership.

use super::*;
use crate::runtime::RuntimeIrohTransportPolicy;
use crate::security::remote::{RemoteRoleCeiling, write_remote_invitation_file_new};
use std::os::unix::fs::PermissionsExt;

/// Builds real hello admission with synthetic protected invitation evidence.
async fn fixture() -> (
    std::path::PathBuf,
    OutboundEndpointOwner,
    OutboundFrontendAdmission,
    AdmittedFrontend,
    Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
) {
    let root = std::env::temp_dir().join(format!("mez-pair-prep-{:032x}", rand::random::<u128>()));
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

/// Scope, expiry, unsafe files and preexisting alias pins reject before network
/// work. A fresh alias remains unpublished, and conflicting profile bytes remain
/// exact; no invalid invitation is promoted into paired application authority.
#[tokio::test]
async fn outbound_pairing_preparation_rejects_unqualified_evidence() {
    for case in ["expired", "legacy", "permissive", "collision"] {
        let (root, endpoint, admission, frontend, mut client) = fixture().await;
        let server = iroh::SecretKey::generate().public();
        let path = root.join("invitation.json");
        let value = serde_json::json!({"format_version":1,"profile_name":"paired",
            "server_addr":iroh::EndpointAddr::new(server),"role":"observer","token":"synthetic-proof",
            "profile_scope":if case == "legacy" { "legacy_session" } else { "host" },
            "expires_at_unix_seconds":if case == "expired" { 0 } else { u64::MAX }});
        write_remote_invitation_file_new(&path, value.to_string().as_bytes()).unwrap();
        if case == "permissive" {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let store = RemoteClientProfileStore::under_config_root(&root);
        let database = root.join("remote/client/profiles.json");
        let before = if case == "collision" {
            store
                .save(&RemoteClientProfile {
                    name: "paired".into(),
                    server_addr: iroh::EndpointAddr::new(iroh::SecretKey::generate().public()),
                    role: RemoteRoleCeiling::Observer,
                    scope: RemoteClientProfileScope::Host,
                    device_credential: secrecy::SecretString::from("original-proof".to_string()),
                })
                .unwrap();
            Some(std::fs::read(&database).unwrap())
        } else {
            None
        };
        client
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({
                    "operation":"pair","handle":frontend.handle(),"path":path,"save_as":null
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert!(
            frontend
                .prepare_operation(Duration::from_secs(2))
                .await
                .is_err()
        );
        assert_eq!(admission.slots.available_permits(), 1);
        if let Some(before) = before {
            assert_eq!(std::fs::read(database).unwrap(), before);
        } else {
            assert!(!database.exists());
        }
        assert!(client.next().await.is_none());
        drop(client);
        drop(admission);
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

/// A held administrator lock lasts beyond the async preparation deadline. The
/// cancelled waiter must leave its finite admission slot and endpoint retained
/// until the original lock attempt exits; no network or publication can follow.
#[tokio::test]
async fn outbound_pairing_preparation_timeout_retains_worker_ownership() {
    use rustix::fs::{FlockOperation, flock};
    let (root, endpoint, admission, frontend, mut client) = fixture().await;
    let store = RemoteClientProfileStore::under_config_root(&root);
    let server = iroh::SecretKey::generate().public();
    store
        .save(&RemoteClientProfile {
            name: "paired".into(),
            server_addr: iroh::EndpointAddr::new(server),
            role: RemoteRoleCeiling::Observer,
            scope: RemoteClientProfileScope::Host,
            device_credential: secrecy::SecretString::from("original-proof".to_string()),
        })
        .unwrap();
    let path = root.join("invitation.json");
    write_remote_invitation_file_new(
        &path,
        serde_json::json!({"format_version":1,"profile_name":"paired",
        "server_addr":iroh::EndpointAddr::new(server),"role":"observer","token":"synthetic-proof",
        "profile_scope":"host","expires_at_unix_seconds":u64::MAX})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("remote/client/profiles.lock"))
        .unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();
    client
        .send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({
                "operation":"pair","handle":frontend.handle(),"path":path,"save_as":null
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert!(
        frontend
            .prepare_operation(Duration::from_millis(100))
            .await
            .is_err()
    );
    assert_eq!(admission.slots.available_permits(), 0);
    assert!(endpoint.clone().begin_shutdown().is_err());
    drop(lock);
    tokio::time::timeout(Duration::from_secs(5), async {
        while admission.slots.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(client.next().await.is_none());
    assert_eq!(
        store
            .load_for_outbound("paired")
            .unwrap()
            .unwrap()
            .device_credential
            .expose_secret(),
        "original-proof"
    );
    drop(client);
    drop(admission);
    endpoint
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
