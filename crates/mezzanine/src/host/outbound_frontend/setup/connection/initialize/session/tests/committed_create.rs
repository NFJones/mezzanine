//! Shared-owner acceptance of host-committed creation whose response is lost.
//!
//! A request-local test seam closes the real host response bridge after commit.
//! The consumed broker/client operation never retries; explicit same-key recovery
//! is a separate caller decision. Disposable loopback sessions invoke no provider,
//! desktop clipboard or X work. A live sibling fences isolated cleanup.

use super::*;
use crate::host::iroh::HostIrohRuntime;
use crate::host::outbound_frontend::{OutboundFrontendListener, client::OutboundFrontendClient};
use crate::host::router::{HostDefaultSessionPolicy, HostSessionRouter, HostSessionRouterConfig};
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
use crate::security::remote::{
    RemoteHostRoutingAuthority, RemoteSessionAttachScope, RemoteTrustStore,
};
use std::os::unix::fs::PermissionsExt;

/// Preserves creation fingerprint inputs independently of the local fault marker.
/// Only client_name changes for explicit recovery; name/key/geometry remain exact.
fn initialize(client_name: &str, name: &str, key: &str) -> serde_json::Value {
    serde_json::json!({"client_name":client_name,"requested_version":3,
        "requested_role":"primary","session_intent":"create",
        "idempotency_key":key,"detach_primary_on_disconnect":true,
        "client":{"name":client_name,"interactive":true,
            "terminal":{"columns":80,"rows":24,"term":"xterm"},
            "metadata":{"session_name":name}}})
}

/// Loss after durable commit must preserve exactly one new lease/runtime, remove
/// the abandoned primary, and leave a sibling live. Only an explicit same-key
/// recovery may attach to the committed result; changed fingerprint rejects
/// without allocating another session. Graceful owner replacement retains key
/// identity and the committed host sessions rather than replaying their creation.
#[tokio::test]
async fn outbound_session_committed_create_lost_reply_preserves_exact_recovery() {
    Box::pin(qualify(true)).await;
}

/// The identical shared-owner workflow without reply loss must return successful
/// correlated settlement. This negative control prevents correlation rejection
/// or unconditional owner disposal from masquerading as publication failure.
#[tokio::test]
async fn outbound_session_committed_create_reply_control_succeeds() {
    Box::pin(qualify(false)).await;
}

/// Owns all host/listener/session futures directly under finite fixture deadlines.
async fn qualify(lose_reply: bool) {
    let root = std::env::temp_dir().join(format!("mez-cf-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host_root = root.join("host");
    let client_root = root.join("client");
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
    let original_endpoint = endpoint.endpoint_id();
    let listener =
        OutboundFrontendListener::bind(endpoint.clone(), 3, policy.setup_timeout).unwrap();
    let router = HostSessionRouter::new(HostSessionRouterConfig {
        runtime_root: root.join("runtime"),
        owner_uid: crate::runtime::current_effective_uid(),
        config_root: host_root.clone(),
        config_layers: vec![],
        shell: ResolvedShell::new(
            std::path::PathBuf::from("/bin/sh"),
            ShellSource::FallbackBinSh,
        ),
        max_sessions: 3,
        max_live_sessions: 3,
        default_session_policy: HostDefaultSessionPolicy::MostRecentAttachable,
        default_lease_lifetime_seconds: 0,
    });
    let trust = RemoteTrustStore::under_host_config_root(&host_root).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invitation = trust
        .create_host_invitation(
            host.endpoint_id(),
            crate::security::remote::RemoteRoleCeiling::Primary,
            RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 3,
                max_live_sessions: 3,
                lease_lifetime_ceiling_seconds: None,
            },
            600,
            now,
        )
        .unwrap();
    let redemption = trust
        .redeem_invitation(
            &invitation.token,
            host.endpoint_id(),
            &original_endpoint.to_string(),
            "fault-fixture",
            RequestedRole::Primary,
            now,
        )
        .unwrap();
    RemoteClientProfileStore::under_config_root(&client_root)
        .save(&RemoteClientProfile {
            name: "creator".into(),
            server_addr: host.endpoint_addr().unwrap(),
            role: crate::security::remote::RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: redemption.device_credential,
        })
        .unwrap();
    let stop_host = Arc::new(tokio::sync::Notify::new());
    let stopped_host = stop_host.clone();
    let stop_listener = Arc::new(tokio::sync::Notify::new());
    let stopped_listener = stop_listener.clone();
    let clients = async {
        let ready = OutboundFrontendClient::connect(&client_root, policy.setup_timeout)
            .await
            .unwrap();
        let (sibling, _) = ready
            .start_session(
                "creator",
                initialize("sibling", "sibling", "create-sibling"),
                80,
                24,
                policy.setup_timeout,
            )
            .await
            .unwrap();
        let sibling_summary = serde_json::to_value(sibling.summary()).unwrap();
        let ready = OutboundFrontendClient::connect(&client_root, policy.setup_timeout)
            .await
            .unwrap();
        let lost = ready
            .start_session(
                "creator",
                initialize(
                    if lose_reply {
                        "test-outbound-lose-committed-reply"
                    } else {
                        "test-outbound-committed-reply-control"
                    },
                    "lost",
                    "create-lost",
                ),
                80,
                24,
                policy.setup_timeout,
            )
            .await;
        if !lose_reply {
            let (created, _) =
                lost.expect("fault-disabled creation must return correlated success");
            assert_ne!(
                serde_json::to_value(created.summary()).unwrap()["session_id"],
                sibling_summary["session_id"]
            );
            assert_eq!(router.snapshots().await.unwrap().len(), 2);
            created
                .detach_self("detach-control-created", policy.setup_timeout)
                .await
                .unwrap();
            sibling
                .detach_self("detach-control-sibling", policy.setup_timeout)
                .await
                .unwrap();
            stop_listener.notify_one();
            return;
        }
        assert!(
            lost.is_err(),
            "committed creation with no response must not report success"
        );
        let snapshots = router.snapshots().await.unwrap();
        assert_eq!(
            snapshots.len(),
            2,
            "one uncertain invocation must create only once"
        );
        let lost_session = snapshots
            .iter()
            .find(|entry| entry.session_id != sibling_summary["session_id"].as_str().unwrap())
            .unwrap()
            .session_id
            .clone();
        let runtime = router.runtime_for_tests(&lost_session).unwrap();
        tokio::time::timeout(policy.setup_timeout, async {
            while runtime.actor().lifecycle_state().await.unwrap()
                != crate::runtime::RuntimeLifecycleState::Detached
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("lost response must retire the abandoned primary");
        let management = OutboundFrontendClient::connect(&client_root, policy.setup_timeout)
            .await
            .unwrap();
        let rows = serde_json::to_value(
            management
                .list_sessions("creator", policy.setup_timeout)
                .await
                .unwrap(),
        )
        .unwrap();
        let lost_row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["session_id"] == lost_session)
            .unwrap();
        assert_eq!(lost_row["state"], "active");
        let lost_lease = lost_row["lease_id"].clone();
        let (sibling, connected, _) = sibling
            .sample_transport_health(policy.setup_timeout)
            .await
            .unwrap();
        assert!(connected);
        assert_eq!(
            serde_json::to_value(sibling.summary()).unwrap(),
            sibling_summary
        );
        assert_eq!(
            router.snapshots().await.unwrap().len(),
            2,
            "inspection must not retry uncertain creation"
        );
        // Deliberate recovery, not a transport retry: the caller supplies the
        // exact original logical operation key after inspecting committed state.
        let ready = OutboundFrontendClient::connect(&client_root, policy.setup_timeout)
            .await
            .unwrap();
        let (recovered, _) = ready
            .start_session(
                "creator",
                initialize("recovery", "lost", "create-lost"),
                80,
                24,
                policy.setup_timeout,
            )
            .await
            .unwrap();
        let recovered_summary = serde_json::to_value(recovered.summary()).unwrap();
        assert_eq!(recovered_summary["session_id"], lost_session);
        assert_eq!(recovered_summary["lease_id"], lost_lease);
        assert_eq!(router.snapshots().await.unwrap().len(), 2);
        recovered
            .detach_self("detach-recovery", policy.setup_timeout)
            .await
            .unwrap();
        tokio::time::timeout(policy.setup_timeout, async {
            while runtime.actor().lifecycle_state().await.unwrap()
                != crate::runtime::RuntimeLifecycleState::Detached
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // There is spare global/principal capacity and no attached primary on
        // the recovered session: those conditions cannot mask fingerprint reuse.
        let ready = OutboundFrontendClient::connect(&client_root, policy.setup_timeout)
            .await
            .unwrap();
        assert!(
            ready
                .start_session(
                    "creator",
                    initialize("conflict", "changed", "create-lost"),
                    80,
                    24,
                    policy.setup_timeout
                )
                .await
                .is_err()
        );
        assert_eq!(
            router.snapshots().await.unwrap().len(),
            2,
            "changed-input replay must not allocate another runtime"
        );
        let (sibling, connected, _) = sibling
            .sample_transport_health(policy.setup_timeout)
            .await
            .unwrap();
        assert!(connected);
        sibling
            .detach_self("detach-sibling", policy.setup_timeout)
            .await
            .unwrap();
        stop_listener.notify_one();
    };
    let owner = async {
        let (accepted, ()) = tokio::time::timeout(std::time::Duration::from_secs(40), async {
            tokio::join!(
                Box::pin(listener.serve(async move { stopped_listener.notified().await })),
                Box::pin(clients)
            )
        })
        .await
        .unwrap();
        assert_eq!(accepted.unwrap(), if lose_reply { 5 } else { 2 });
        drop(listener);
        endpoint
            .retire_and_shutdown()
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
        let replacement = OutboundEndpointOwner::bind(&client_root, &policy)
            .await
            .unwrap();
        assert_eq!(replacement.endpoint_id(), original_endpoint);
        assert_eq!(router.snapshots().await.unwrap().len(), 2);
        replacement
            .retire_and_shutdown()
            .await
            .unwrap()
            .finish()
            .await
            .unwrap();
        stop_host.notify_one();
    };
    // The host future is driven alongside the complete client/owner workflow.
    let (served, ()) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(
            Box::pin(host.serve_routed(router.clone(), async move {
                stopped_host.notified().await;
            })),
            Box::pin(owner)
        )
    })
    .await
    .unwrap();
    served.unwrap();
    router
        .shutdown_all(true, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
