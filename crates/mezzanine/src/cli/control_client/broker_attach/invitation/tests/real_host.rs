//! Real host invitation redemption followed by original-key session creation.
//!
//! Disposable host/client roots exercise the production broker supervisor and
//! attachment selector. No provider, desktop clipboard or physical terminal is
//! used. Trust issues a loopback test invitation; production foreign-route
//! invitation export remains unchanged. All cleanup is exact-owner scoped.

use super::*;
use crate::host::iroh::HostIrohRuntime;
use crate::host::outbound_endpoint::OutboundEndpointOwner;
use crate::host::outbound_frontend::OutboundFrontendListener;
use crate::host::router::{HostDefaultSessionPolicy, HostSessionRouter, HostSessionRouterConfig};
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::{
    RemoteHostRoutingAuthority, RemoteSessionAttachScope, RemoteTrustStore,
};
use std::time::Duration;

/// The actual attachment selector must redeem through the live endpoint owner,
/// publish the alias privately, and create exactly one host runtime with the
/// original prepared key. Self-detach preserves the committed session and shared
/// endpoint. A later management authentication proves the published proof works.
#[tokio::test]
async fn broker_invitation_real_host_creates_once_after_private_pairing() {
    let root =
        std::env::temp_dir().join(format!("mez-invite-create-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = crate::cli::CliEnv {
        home: Some(root.join("client-home")),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let host_root = root.join("host");
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
    let endpoint = OutboundEndpointOwner::bind(paths.root(), &policy)
        .await
        .unwrap();
    let identity = endpoint.endpoint_id();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 2, policy.setup_timeout).unwrap();
    let router = HostSessionRouter::new(HostSessionRouterConfig {
        runtime_root: root.join("host-runtime"),
        owner_uid: crate::runtime::current_effective_uid(),
        config_root: host_root.clone(),
        config_layers: vec![],
        shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        max_sessions: 2,
        max_live_sessions: 2,
        default_session_policy: HostDefaultSessionPolicy::MostRecentAttachable,
        default_lease_lifetime_seconds: 0,
    });
    let trust = RemoteTrustStore::under_host_config_root(&host_root).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invitation = trust
        .create_host_invitation(
            host.endpoint_id(),
            RemoteRoleCeiling::Primary,
            RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 2,
                max_live_sessions: 2,
                lease_lifetime_ceiling_seconds: None,
            },
            600,
            now,
        )
        .unwrap();
    let invitation_path = root.join("invitation.json");
    crate::security::remote::write_remote_invitation_file_new(&invitation_path, serde_json::json!({
        "format_version":1,"profile_name":"authored","server_addr":host.endpoint_addr().unwrap(),
        "server_endpoint_id":host.endpoint_id(),"role":"primary","profile_scope":"host",
        "token":invitation.token.expose_secret(),"expires_at_unix_seconds":invitation.expires_at_unix_seconds
    }).to_string().as_bytes()).unwrap();
    let host_stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let listener_stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stopped_host = host_stop.clone();
    let stopped_listener = listener_stop.clone();
    let serve = host.serve_routed(router.clone(), async move { stopped_host.notified().await });
    let supervise = listener.serve(async move { stopped_listener.notified().await });
    let clients = async {
        let target = crate::cli::ControlTargetSelection::IrohInvitation {
            path: invitation_path,
            save_as: Some("paired-create".into()),
        };
        let routing = IrohSessionRouting::Create {
            name: Some("from-broker-invitation".into()),
            idempotency_key: "original-invitation-create".into(),
        };
        let mut child = None;
        let attachment = crate::cli::control_client::broker_attach::try_open_starting(
            &target, &env, "primary", &routing, 80, 24, "xterm", false, &mut child,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            child.is_none(),
            "live owner must not spawn another endpoint"
        );
        assert!(attachment.primary);
        assert_eq!(
            routing.idempotency_key(),
            Some("original-invitation-create")
        );
        let summary = serde_json::to_value(attachment.session.summary()).unwrap();
        assert_eq!(summary["granted_role"], "primary");
        let profile = RemoteClientProfileStore::under_config_root(paths.root())
            .load_for_outbound("paired-create")
            .unwrap()
            .unwrap();
        assert_eq!(profile.server_addr.id.to_string(), host.endpoint_id());
        assert_eq!(profile.scope, RemoteClientProfileScope::Host);
        assert_eq!(profile.role, RemoteRoleCeiling::Primary);
        assert!(
            !summary
                .to_string()
                .contains(profile.device_credential.expose_secret())
        );
        assert_eq!(router.snapshots().await.unwrap().len(), 1);
        let (session, lines) = attachment
            .session
            .snapshot(80, 24, policy.setup_timeout)
            .await
            .unwrap();
        assert!(!lines.is_empty());
        session
            .detach_self("invitation-self-detach", policy.setup_timeout)
            .await
            .unwrap();
        let management = OutboundFrontendClient::connect(paths.root(), policy.setup_timeout)
            .await
            .unwrap();
        let leases = management
            .list_sessions("paired-create", policy.setup_timeout)
            .await
            .unwrap();
        let leases = serde_json::to_value(leases).unwrap();
        assert_eq!(leases.as_array().unwrap().len(), 1);
        assert_eq!(leases[0]["session_id"], summary["session_id"]);
        assert_eq!(
            router.snapshots().await.unwrap().len(),
            1,
            "disconnect preserves committed runtime"
        );
        assert_eq!(endpoint.endpoint_id(), identity);
        assert!(RemoteClientIdentity::load_or_create(paths.root()).is_err());
        listener_stop.notify_one();
        host_stop.notify_one();
    };
    let (host_result, listener_result, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(serve, supervise, Box::pin(clients))
    })
    .await
    .unwrap();
    assert_eq!(host_result.unwrap(), 3);
    assert_eq!(listener_result.unwrap(), 3);
    router
        .shutdown_all(true, Duration::from_secs(5))
        .await
        .unwrap();
    drop(listener);
    endpoint
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
