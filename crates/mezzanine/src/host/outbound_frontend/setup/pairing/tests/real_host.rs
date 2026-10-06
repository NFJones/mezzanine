//! Actual host redemption, protected publication and secret-free local replies.
//!
//! Disposable roots and the real supervising listener reuse one endpoint. A
//! subsequent authentication-only exchange proves issued proof is usable, while
//! a separate retained frontend proves pairing does not close sibling ownership.

use super::*;
use crate::host::iroh::HostIrohRuntime;
use crate::host::outbound_frontend::OutboundFrontendListener;
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::{
    RemoteClientIdentity, RemoteRoleCeiling, RemoteTrustStore, write_remote_invitation_file_new,
};
use std::os::unix::fs::PermissionsExt;

/// Acquires real UID-authenticated bounded hello admission without accessing
/// device credentials. Only the exact returned handle enters later operations.
async fn local(
    root: &std::path::Path,
) -> (
    Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
    serde_json::Value,
) {
    let stream = tokio::net::UnixStream::connect(root.join("outbound.sock"))
        .await
        .unwrap();
    let mut stream = Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
    stream
        .send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({"protocol":PROTOCOL}).to_string(),
        ))
        .await
        .unwrap();
    let hello = stream.next().await.unwrap().unwrap();
    assert_eq!(hello.content_type, CONTENT_TYPE);
    let hello: serde_json::Value = serde_json::from_str(&hello.body).unwrap();
    (stream, hello["handle"].clone())
}

/// Pairing through the actual supervisor redeems a protected invitation on the
/// already-owned endpoint, persists issued proof, and emits no secret metadata.
/// New credentials then authenticate without listing authority or session work;
/// a silent sibling remains live and identity ownership remains exclusive.
#[tokio::test]
async fn outbound_pairing_real_host_publishes_proof_without_local_secret_delivery() {
    let root =
        std::env::temp_dir().join(format!("mez-broker-pair-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host_root = root.join("host");
    let runtime_root = root.join("cli-runtime");
    std::fs::create_dir(&runtime_root).unwrap();
    std::fs::set_permissions(&runtime_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cli_env = crate::cli::CliEnv {
        home: Some(root.join("client-home")),
        runtime: crate::runtime::RuntimeEnv {
            mez_tmpdir: Some(runtime_root.into_os_string()),
            xdg_runtime_dir: None,
            tmpdir: None,
            uid: crate::runtime::current_effective_uid(),
        },
        ..Default::default()
    };
    let paths = crate::config::ConfigPaths::from_home(cli_env.home.clone().unwrap());
    paths.ensure_default_config().unwrap();
    let client_root = paths.root().to_path_buf();
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        ..Default::default()
    };
    let host = HostIrohRuntime::bind(
        &host_root,
        RuntimeIrohTransportPolicy {
            enabled: true,
            ..policy.clone()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let endpoint = OutboundEndpointOwner::bind(&client_root, &policy)
        .await
        .unwrap();
    let identity = endpoint.endpoint_id();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 3, std::time::Duration::from_secs(5))
            .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invitation = RemoteTrustStore::under_host_config_root(&host_root)
        .unwrap()
        .create_invitation(host.endpoint_id(), RemoteRoleCeiling::Observer, 600, now)
        .unwrap();
    // Loopback qualification intentionally supplies a local route; production
    // invitation export still requires foreign-machine reachability.
    let invitation = serde_json::json!({"format_version":1,"profile_name":"authored",
        "server_addr":host.endpoint_addr().unwrap(),"server_endpoint_id":host.endpoint_id(),
        "role":"observer","profile_scope":"host","token":invitation.token.expose_secret(),
        "expires_at_unix_seconds":invitation.expires_at_unix_seconds});
    let path = root.join("invitation.json");
    write_remote_invitation_file_new(&path, invitation.to_string().as_bytes()).unwrap();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let host_stop = stop.clone();
    let local_stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let supervisor_stop = local_stop.clone();
    let host_work = host.serve(async move { host_stop.notified().await });
    let supervisor = listener.serve(async move { supervisor_stop.notified().await });
    let clients = async {
        let (mut sibling, sibling_handle) = local(&client_root).await;
        let (mut pairing, handle) = local(&client_root).await;
        assert_ne!(sibling_handle, handle);
        pairing
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({
                    "operation":"pair","handle":handle,"path":path,"save_as":"paired-alias"
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let frame = pairing.next().await.unwrap().unwrap();
        assert_eq!(frame.content_type, CONTENT_TYPE);
        let reply: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert_eq!(
            reply,
            serde_json::json!({"handle":handle,"paired":true,"profile":"paired-alias"})
        );
        assert!(pairing.next().await.is_none());
        let store = RemoteClientProfileStore::under_config_root(&client_root);
        let profile = store.load_for_outbound("paired-alias").unwrap().unwrap();
        assert_eq!(profile.scope, RemoteClientProfileScope::Host);
        assert_eq!(profile.server_addr.id.to_string(), host.endpoint_id());
        assert_eq!(profile.role, RemoteRoleCeiling::Observer);
        assert!(
            !frame
                .body
                .contains(profile.device_credential.expose_secret())
        );
        assert_eq!(endpoint.endpoint_id(), identity);
        assert!(RemoteClientIdentity::load_or_create(&client_root).is_err());
        let (mut health, handle) = local(&client_root).await;
        health.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
            "handle":handle,"profile":"paired-alias","initialize":{
                "client_name":"paired-health","requested_version":3,"requested_role":"observer","session_intent":"host_only"
            }
        }).to_string())).await.unwrap();
        health
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({"handle":handle,"authentication_only":true}).to_string(),
            ))
            .await
            .unwrap();
        let frame = health.next().await.unwrap().unwrap();
        let reply: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
        assert_eq!(reply["handle"], handle);
        assert_eq!(reply["host"]["host_only"], true);
        assert_eq!(reply["sessions"], serde_json::json!([]));
        assert!(health.next().await.is_none());
        // Exercise ordinary command dispatch while the endpoint lock is held.
        // A separate primary invitation must publish its authored ceiling, not
        // mistake host-only observer initialization for observer-only pairing.
        let cli_invitation = RemoteTrustStore::under_host_config_root(&host_root)
            .unwrap()
            .create_invitation(host.endpoint_id(), RemoteRoleCeiling::Primary, 600, now)
            .unwrap();
        let cli_path = root.join("cli-invitation.json");
        write_remote_invitation_file_new(&cli_path, serde_json::json!({
            "format_version":1,"profile_name":"cli-authored","server_addr":host.endpoint_addr().unwrap(),
            "server_endpoint_id":host.endpoint_id(),"role":"primary","profile_scope":"host",
            "token":cli_invitation.token.expose_secret(),"expires_at_unix_seconds":cli_invitation.expires_at_unix_seconds
        }).to_string().as_bytes()).unwrap();
        let mut output = Vec::new();
        let mut error = Vec::new();
        let code = crate::cli::run_with(
            vec![
                "mez".into(),
                "--json".into(),
                "remote".into(),
                "pair".into(),
                "--invite-file".into(),
                cli_path.to_str().unwrap().into(),
                "--name".into(),
                "cli-paired".into(),
            ],
            cli_env.clone(),
            false,
            &mut output,
            &mut error,
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        let published: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(published["paired"], true);
        assert_eq!(published["profile"]["name"], "cli-paired");
        assert_eq!(published["profile"]["role"], "primary");
        let cli_profile = store.load_for_outbound("cli-paired").unwrap().unwrap();
        assert_eq!(cli_profile.role, RemoteRoleCeiling::Primary);
        assert!(
            !String::from_utf8_lossy(&output)
                .contains(cli_profile.device_credential.expose_secret())
        );
        assert!(!String::from_utf8_lossy(&output).contains(cli_invitation.token.expose_secret()));
        assert!(error.is_empty());
        assert_eq!(endpoint.endpoint_id(), identity);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), sibling.next())
                .await
                .is_err(),
            "pairing must not retire its sibling"
        );
        drop(sibling);
        local_stop.notify_one();
        stop.notify_one();
    };
    let (host_result, local_result, ()) =
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(host_work, supervisor, clients)
        })
        .await
        .unwrap();
    assert_eq!(host_result.unwrap(), 3);
    assert_eq!(local_result.unwrap(), 4);
    drop(listener);
    endpoint
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(
        RemoteClientIdentity::load_or_create(&client_root)
            .unwrap()
            .endpoint_id(),
        identity
    );
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
