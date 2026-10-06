//! Real invitation-first broker startup and sibling session acceptance.
//!
//! Disposable protected roots and a loopback authorizing host exercise the real
//! executable without a prestarted endpoint. Invitation proof is redeemed only
//! by the elected owner, and a later profile attachment reuses that identity.
//! No provider, desktop clipboard, X server or physical terminal work occurs.

use super::*;
use crate::host::iroh::HostIrohRuntime;
use crate::host::router::{HostDefaultSessionPolicy, HostSessionRouter, HostSessionRouterConfig};
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::{
    RemoteHostRoutingAuthority, RemoteSessionAttachScope, RemoteTrustStore,
};
use std::os::unix::fs::PermissionsExt;

/// Starting from invitation evidence and no owner must launch exactly one real
/// broker, publish paired proof, and create a distinct sibling using the saved
/// alias while the first remains attached. Retiring the first preserves sibling
/// control and the child. Teardown signals only the exact retained child and
/// verifies identity reuse; no ambiguous operation is retried.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for invitation-first process qualification"]
async fn broker_attach_invitation_first_owner_creates_live_sibling() {
    let executable = PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("explicit trusted executable required"),
    );
    assert!(executable.is_absolute());
    let root = std::env::temp_dir().join(format!("mez-is-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = crate::cli::CliEnv {
        home: Some(root.join("home")),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    std::fs::write(
        paths.default_primary_file(),
        format!(
            "version = {}\n[transport.iroh]\ncompression_codecs = [\"none\"]\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION,
        ),
    )
    .unwrap();
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        ..Default::default()
    };
    let host_root = root.join("host");
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
    let router = HostSessionRouter::new(HostSessionRouterConfig {
        runtime_root: root.join("runtime"),
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
        "format_version":1,"profile_name":"fixture","server_addr":host.endpoint_addr().unwrap(),
        "server_endpoint_id":host.endpoint_id(),"role":"primary","profile_scope":"host",
        "token":invitation.token.expose_secret(),"expires_at_unix_seconds":invitation.expires_at_unix_seconds
    }).to_string().as_bytes()).unwrap();
    let before = std::fs::read(&invitation_path).unwrap();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stopped = stop.clone();
    let mut child = None;
    let mut sibling_child = None;
    let work = async {
        let first = try_open_inner(
            &crate::cli::ControlTargetSelection::IrohInvitation {
                path: invitation_path.clone(),
                save_as: None,
            },
            &env,
            "primary",
            &IrohSessionRouting::Create {
                name: Some("invitation-first".into()),
                idempotency_key: "original-invitation-first-success".into(),
            },
            80,
            24,
            "xterm",
            None,
            Some(&mut child),
            Some(&executable),
        )
        .await?
        .ok_or_else(|| MezError::invalid_state("invitation startup bypassed shared owner"))?;
        assert!(child.is_some());
        let first_summary = serde_json::to_value(first.session.summary()).unwrap();
        let profile = RemoteClientProfileStore::under_config_root(paths.root())
            .load("fixture")?
            .ok_or_else(|| MezError::invalid_state("paired fixture profile missing"))?;
        assert_eq!(profile.server_addr.id.to_string(), host.endpoint_id());
        assert_eq!(profile.role, RemoteRoleCeiling::Primary);
        assert_eq!(profile.scope, RemoteClientProfileScope::Host);
        assert!(
            !first_summary
                .to_string()
                .contains(profile.device_credential.expose_secret())
        );
        let second = try_open_inner(
            &crate::cli::ControlTargetSelection::IrohProfile("fixture".into()),
            &env,
            "primary",
            &IrohSessionRouting::Create {
                name: Some("invitation-sibling".into()),
                idempotency_key: "original-invitation-sibling".into(),
            },
            80,
            24,
            "xterm",
            None,
            Some(&mut sibling_child),
            Some(&executable),
        )
        .await?
        .ok_or_else(|| MezError::invalid_state("sibling setup bypassed shared owner"))?;
        assert!(sibling_child.is_none());
        let second_summary = serde_json::to_value(second.session.summary()).unwrap();
        assert_ne!(first_summary["session_id"], second_summary["session_id"]);
        assert_ne!(first_summary["lease_id"], second_summary["lease_id"]);
        assert_eq!(router.snapshots().await?.len(), 2);
        first
            .session
            .detach_self("retire-invitation-first", policy.setup_timeout)
            .await?;
        let (second, connected, _) = second
            .session
            .sample_transport_health(policy.setup_timeout)
            .await?;
        assert!(connected);
        let (second, lines) = second.snapshot(80, 24, policy.setup_timeout).await?;
        assert!(!lines.is_empty());
        assert_eq!(
            serde_json::to_value(second.summary()).unwrap(),
            second_summary
        );
        assert!(child.as_mut().unwrap().try_wait()?.is_none());
        assert!(RemoteClientIdentity::load_or_create(paths.root()).is_err());
        second
            .detach_self("retire-invitation-sibling", policy.setup_timeout)
            .await?;
        assert_eq!(router.snapshots().await?.len(), 2);
        assert_eq!(std::fs::read(&invitation_path)?, before);
        Ok::<(), MezError>(())
    };
    let work = async {
        let result = tokio::time::timeout(Duration::from_secs(60), Box::pin(work)).await;
        stop.notify_one();
        result
    };
    let (served, result) = tokio::join!(
        host.serve_routed(router.clone(), async move {
            stopped.notified().await;
        }),
        Box::pin(work)
    );
    // Cleanup precedes result assertions and uses only caller-retained children.
    let shutdown = if let Some(child) = child.as_mut() {
        tokio::time::timeout(Duration::from_secs(10), child.shutdown_for_tests()).await
    } else {
        Ok(Err(MezError::invalid_state("fixture broker child missing")))
    };
    for retained in [&mut child, &mut sibling_child] {
        if let Some(child) = retained.as_mut()
            && child.try_wait().unwrap().is_none()
        {
            tokio::time::timeout(Duration::from_secs(5), child.terminate_for_tests())
                .await
                .unwrap()
                .unwrap();
        }
    }
    router
        .shutdown_all(true, Duration::from_secs(5))
        .await
        .unwrap();
    served.unwrap();
    result.unwrap().unwrap();
    assert!(shutdown.unwrap().unwrap().success());
    assert!(!paths.root().join("outbound.sock").exists());
    let identity = RemoteClientIdentity::load_or_create(paths.root()).unwrap();
    assert!(
        trust
            .list_records()
            .unwrap()
            .iter()
            .any(|record| record.endpoint_id == identity.endpoint_id().to_string())
    );
    drop(identity);
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
