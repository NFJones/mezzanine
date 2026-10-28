//! Host-scoped transport, trust, routing and cleanup regressions.
//!
//! Offline endpoints and private stores exercise initialization, authority and
//! exact routed connection cleanup. Native session owners are stopped explicitly;
//! diagnostic assertions never grant lease, replay or process-retirement authority.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::host::router::HostSessionRouterConfig;
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::security::audit::{AuditConfig, AuditLog};
use crate::security::remote::RemoteSessionAttachScope;

use super::*;

fn test_root(label: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp").join(format!(
        "mez-host-iroh-{label}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ))
}

/// Endpoint-close degradation and a connection-task panic must both remain
/// visible after cleanup without exposing the panic payload.
#[tokio::test(flavor = "current_thread")]
async fn host_shutdown_report_aggregates_close_and_task_failures() {
    let mut tasks = JoinSet::new();
    tasks.spawn(async { panic!("private panic payload") });
    let joined = tasks.join_next().await.unwrap();
    let mut report = HostIrohShutdownReport {
        endpoint_close_failed: true,
        ..HostIrohShutdownReport::default()
    };
    report.record_task_completion(joined, false);

    let error = report.finish(7, true).unwrap_err();

    assert_eq!(error.kind(), MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("persistent host Iroh listener closed unexpectedly"),
        "{error:?}"
    );
    assert!(
        error.message().contains("endpoint close timed out"),
        "{error:?}"
    );
    assert!(
        error.message().contains("1 connection task joins failed"),
        "{error:?}"
    );
    assert!(
        error.message().contains("accepting 7 connections"),
        "{error:?}"
    );
    assert!(
        !error.message().contains("private panic payload"),
        "{error:?}"
    );
}

/// A connection task that exceeds the bounded drain is aborted, joined,
/// and reported instead of being detached or misreported as clean.
#[tokio::test(flavor = "current_thread")]
async fn host_shutdown_report_records_forced_abort_and_joins_task() {
    let mut tasks = JoinSet::new();
    tasks.spawn(std::future::pending::<()>());
    let mut report = HostIrohShutdownReport::default();

    drain_host_iroh_connection_tasks(
        &mut tasks,
        std::time::Duration::from_millis(10),
        &mut report,
    )
    .await;

    assert!(tasks.is_empty());
    assert_eq!(report.forced_aborts, 1);
    assert_eq!(report.unexpected_task_cancellations, 0);
    let error = report.finish(1, false).unwrap_err();
    assert!(
        error
            .message()
            .contains("1 connection tasks required forced abort"),
        "{error:?}"
    );
}

/// Fully clean endpoint and task cleanup preserves the accepted count.
#[test]
fn host_shutdown_report_accepts_clean_completion() {
    assert_eq!(
        HostIrohShutdownReport::default().finish(3, false).unwrap(),
        3
    );
}

/// A complete malformed setup header is terminal and must fail before the
/// much longer setup timeout even when the peer keeps its stream open.
#[tokio::test(flavor = "current_thread")]
async fn host_setup_reader_rejects_malformed_header_immediately() {
    let (mut peer, mut host) = tokio::io::duplex(1024);
    peer.write_all(b"Content-Length: invalid\r\n\r\n")
        .await
        .unwrap();

    let error = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        read_one_control_frame(&mut host, std::time::Duration::from_secs(5)),
    )
    .await
    .expect("terminal malformed setup frame should fail before timeout")
    .unwrap_err();

    assert_eq!(error.kind(), MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("invalid Content-Length"),
        "{error:?}"
    );
}

/// Follow-up frame decoding must propagate a terminal duplicate length
/// error without retaining the authenticated connection until idle expiry.
#[tokio::test(flavor = "current_thread")]
async fn host_follow_up_reader_rejects_malformed_header_immediately() {
    let (mut peer, mut host) = tokio::io::duplex(1024);
    peer.write_all(b"Content-Length: 2\r\nContent-Length: 3\r\n\r\n{}!")
        .await
        .unwrap();

    let error = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        read_optional_control_frame(&mut host, std::time::Duration::from_secs(5)),
    )
    .await
    .expect("terminal malformed follow-up frame should fail before timeout")
    .unwrap_err();

    assert_eq!(error.kind(), MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("duplicate Content-Length"),
        "{error:?}"
    );
}

/// EOF after a declared but incomplete body must retain the framing error
/// instead of replacing it with a generic stream-closure diagnostic.
#[tokio::test(flavor = "current_thread")]
async fn host_setup_reader_reports_truncated_frame_at_eof() {
    let (mut peer, mut host) = tokio::io::duplex(1024);
    peer.write_all(b"Content-Length: 4\r\n\r\n{").await.unwrap();
    peer.shutdown().await.unwrap();

    let error = read_one_control_frame(&mut host, std::time::Duration::from_secs(1))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("incomplete protocol frame body"),
        "{error:?}"
    );
}

/// A valid setup frame split across reads remains incomplete until its
/// final bytes arrive and is then decoded exactly once.
#[tokio::test(flavor = "current_thread")]
async fn host_setup_reader_accepts_fragmented_valid_frame() {
    let (mut peer, mut host) = tokio::io::duplex(1024);
    let encoded = encode_control_body(r#"{"jsonrpc":"2.0","id":1}"#);
    let split_at = encoded.len() - 3;
    let writer = async {
        peer.write_all(&encoded[..split_at]).await.unwrap();
        tokio::task::yield_now().await;
        peer.write_all(&encoded[split_at..]).await.unwrap();
    };

    let (body, ()) = tokio::join!(
        read_one_control_frame(&mut host, std::time::Duration::from_secs(1)),
        writer,
    );

    assert_eq!(body.unwrap(), r#"{"jsonrpc":"2.0","id":1}"#);
}

/// Routine connection logs expose route intent without retaining peer
/// addresses or relay topology.
#[test]
fn remote_client_route_reports_privacy_safe_category() {
    let address = "192.0.2.42:443".parse().unwrap();

    assert_eq!(
        remote_client_route(iroh::endpoint::IncomingAddr::Ip(address)),
        "direct"
    );
}

/// Host identity and trust are stable across restart and isolated from
/// legacy per-session security roots.
#[test]
fn host_security_is_stable_locked_and_session_independent() {
    let root = test_root("security");
    let host = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
    let endpoint_id = host.endpoint_id().to_string();
    assert!(RemoteEndpointIdentity::load_or_create_host(&root).is_err());
    let session = RemoteEndpointIdentity::load_or_create(&root, "$1").unwrap();
    assert_ne!(session.endpoint_id(), endpoint_id);
    let host_trust = RemoteTrustStore::under_host_config_root(&root).unwrap();
    let session_trust = RemoteTrustStore::under_config_root(&root, "$1").unwrap();
    assert_ne!(host_trust.directory(), session_trust.directory());
    drop(host);
    let restarted = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
    assert_eq!(restarted.endpoint_id(), endpoint_id);
    let _ = std::fs::remove_dir_all(root);
}

/// Host-only initialization commits trust while returning no session or
/// lease and rejects every session-routing intent before allocation.
#[test]
fn host_only_initialize_pairs_without_session_authority() {
    let root = test_root("initialize");
    let identity = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
    let trust = RemoteTrustStore::under_host_config_root(&root).unwrap();
    let audit_path = root.join("host-audit.jsonl");
    let audit_log = std::sync::Arc::new(std::sync::Mutex::new(Some(AuditLog::new(AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }))));
    let now = current_unix_seconds().unwrap();
    let invitation = trust
        .create_invitation(
            identity.endpoint_id(),
            RemoteRoleCeiling::Observer,
            600,
            now,
        )
        .unwrap();
    let client_id = iroh::SecretKey::generate().public().to_string();
    let request = json!({
        "jsonrpc": "2.0",
        "id": "init",
        "method": "control/initialize",
        "params": {
            "client_name": "test-client",
            "requested_version": 3,
            "requested_role": "observer",
            "session_intent": "host_only",
            "authentication": {
                "mechanism": "extension:iroh_invitation",
                "token": invitation.token.expose_secret()
            }
        }
    })
    .to_string();
    let response = handle_host_only_initialize_with_audit(
        &request,
        &trust,
        identity.endpoint_id(),
        &client_id,
        Some(&audit_log),
    )
    .unwrap();
    let response: Value = serde_json::from_str(&response.body).unwrap();
    assert!(response["result"]["session"].is_null());
    assert!(response["result"]["lease"].is_null());
    assert!(response["result"]["device_credential"].is_string());
    assert_eq!(trust.list_records().unwrap().len(), 1);
    let audit = fs::read_to_string(audit_path).unwrap();
    assert!(audit.contains(r#""action":"invitation_redeem""#), "{audit}");
    assert!(audit.contains(r#""outcome":"attempted""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(!audit.contains(invitation.token.expose_secret()), "{audit}");

    let attach = request.replace("host_only", "attach");
    assert!(
        handle_host_only_initialize(&attach, &trust, identity.endpoint_id(), &client_id,).is_err()
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Required audit denial must prevent invitation authority from being
/// committed when the host cannot write its pairing attribution record.
#[test]
fn host_only_pairing_rolls_back_when_required_audit_is_unavailable() {
    let root = test_root("pairing-audit-denied");
    let identity = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
    let trust = RemoteTrustStore::under_host_config_root(&root).unwrap();
    let invitation = trust
        .create_invitation(
            identity.endpoint_id(),
            RemoteRoleCeiling::Observer,
            600,
            current_unix_seconds().unwrap(),
        )
        .unwrap();
    let client_id = iroh::SecretKey::generate().public().to_string();
    let request = json!({
        "jsonrpc": "2.0",
        "id": "audit-denied",
        "method": "control/initialize",
        "params": {
            "client_name": "audit-denied-client",
            "requested_version": 3,
            "requested_role": "observer",
            "session_intent": "host_only",
            "authentication": {
                "mechanism": "extension:iroh_invitation",
                "token": invitation.token.expose_secret()
            }
        }
    })
    .to_string();
    let audit_log = std::sync::Arc::new(std::sync::Mutex::new(Some(AuditLog::new(AuditConfig {
        enabled: false,
        path: root.join("disabled-audit.jsonl"),
        hash_chain: false,
        required: true,
    }))));

    let error = handle_host_only_initialize_with_audit(
        &request,
        &trust,
        identity.endpoint_id(),
        &client_id,
        Some(&audit_log),
    )
    .unwrap_err();
    assert_eq!(error.kind(), MezErrorKind::Forbidden);
    assert!(trust.list_records().unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

/// The bound host front door must isolate an invalid routing intent, then
/// pair and reconnect the same client endpoint without ever returning
/// session or lease authority.
#[tokio::test(flavor = "current_thread")]
async fn host_listener_isolates_invalid_intent_and_reconnects_device() {
    let root = test_root("listener");
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        setup_timeout: std::time::Duration::from_secs(10),
        idle_timeout: std::time::Duration::from_secs(2),
        ..RuntimeIrohTransportPolicy::default()
    };
    let host = HostIrohRuntime::bind(&root, policy.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        host.create_invitation(
            "unreachable",
            RemoteRoleCeiling::Observer,
            600,
            current_unix_seconds().unwrap()
        )
        .is_err()
    );
    let invitation = host
        .trust
        .create_invitation(
            host.endpoint_id(),
            RemoteRoleCeiling::Observer,
            600,
            current_unix_seconds().unwrap(),
        )
        .unwrap();
    let server_addr = host.endpoint_addr().unwrap();
    let client =
        crate::runtime::bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
            .await
            .unwrap();
    let diagnostics = host.endpoint.diagnostics();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();

    let server = host.serve(async move { server_stop.notified().await });
    let client_work = async {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !diagnostics.snapshot().listener_active {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("live persistent-host listener should report active");
        let invalid = exchange_test_host_initialize(
            &client,
            &server_addr,
            "extension:iroh_invitation",
            invitation.token.expose_secret(),
            "attach",
        )
        .await;
        assert!(invalid.get("error").is_some(), "{invalid}");

        let paired = exchange_test_host_initialize(
            &client,
            &server_addr,
            "extension:iroh_invitation",
            invitation.token.expose_secret(),
            "host_only",
        )
        .await;
        assert!(paired["result"]["session"].is_null(), "{paired}");
        assert!(paired["result"]["lease"].is_null(), "{paired}");
        let credential = paired["result"]["device_credential"]
            .as_str()
            .unwrap()
            .to_string();

        let checked = exchange_test_host_initialize(
            &client,
            &server_addr,
            "extension:iroh_device",
            &credential,
            "host_only",
        )
        .await;
        assert!(checked["result"]["session"].is_null(), "{checked}");
        assert!(checked["result"]["lease"].is_null(), "{checked}");
        stop.notify_one();
    };

    let (served, ()) = tokio::join!(server, client_work);
    assert_eq!(served.unwrap(), 3);
    let snapshot = diagnostics.snapshot();
    assert!(!snapshot.listener_active);
    assert_eq!(snapshot.active_connections, 0);
    assert_eq!(snapshot.connections_accepted, 3);
    assert_eq!(snapshot.connections_rejected, 0);
    assert_eq!(snapshot.setup_successes, 3);
    assert_eq!(snapshot.setup_failures, 0);
    assert_eq!(
        snapshot
            .connections_completed
            .saturating_add(snapshot.connections_failed),
        3
    );
    assert_eq!(host.trust.list_records().unwrap().len(), 1);
    client.close().await;
    drop(host);
    let restarted = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
    assert_eq!(restarted.endpoint_id(), server_addr.id.to_string());
    let _ = std::fs::remove_dir_all(root);
}

/// Endpoint loss outside the host shutdown handle must fail the persistent
/// listener instead of looking like a clean zero-connection completion.
#[tokio::test(flavor = "current_thread")]
async fn host_listener_reports_unexpected_endpoint_closure() {
    let root = test_root("unexpected-endpoint-closure");
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        setup_timeout: std::time::Duration::from_secs(10),
        ..RuntimeIrohTransportPolicy::default()
    };
    let host = HostIrohRuntime::bind(&root, policy).await.unwrap().unwrap();
    let endpoint = host.endpoint.endpoint().clone();
    let diagnostics = host.endpoint.diagnostics();
    endpoint.close().await;

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        host.serve(std::future::pending()),
    )
    .await
    .expect("closed host endpoint should terminate its listener")
    .unwrap_err();
    assert_eq!(error.kind(), MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("persistent host Iroh listener closed unexpectedly"),
        "{error:?}"
    );
    assert!(!diagnostics.snapshot().listener_active);

    drop(host);
    let _ = fs::remove_dir_all(root);
}

/// Closing through the runtime shutdown handle marks endpoint exhaustion
/// intentional and therefore retains clean host-listener completion.
#[tokio::test(flavor = "current_thread")]
async fn host_listener_accepts_intentional_endpoint_shutdown() {
    let root = test_root("intentional-endpoint-closure");
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        setup_timeout: std::time::Duration::from_secs(10),
        ..RuntimeIrohTransportPolicy::default()
    };
    let host = HostIrohRuntime::bind(&root, policy).await.unwrap().unwrap();
    let diagnostics = host.endpoint.diagnostics();
    let shutdown = host.endpoint.shutdown_handle();
    let (served, closed) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(host.serve(std::future::pending()), shutdown.close())
    })
    .await
    .expect("intentional host endpoint shutdown should remain bounded");
    assert!(closed);
    assert_eq!(served.unwrap(), 0);
    assert!(!diagnostics.snapshot().listener_active);

    drop(host);
    let _ = fs::remove_dir_all(root);
}

/// A peer that opens its control stream but withholds the mandatory first
/// frame must release its sole admission slot within the setup deadline so
/// a valid peer can initialize without waiting for application-idle expiry.
#[tokio::test(flavor = "current_thread")]
async fn host_connection_times_out_when_first_frame_is_withheld() {
    let root = test_root("withheld-first-frame");
    let endpoint_policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        max_connections: 1,
        setup_timeout: std::time::Duration::from_secs(10),
        idle_timeout: std::time::Duration::from_secs(5),
        ..RuntimeIrohTransportPolicy::default()
    };
    let host = HostIrohRuntime::bind(&root, endpoint_policy.clone())
        .await
        .unwrap()
        .unwrap();
    let server_addr = host.endpoint_addr().unwrap();
    let client = crate::runtime::bind_runtime_iroh_client_endpoint(
        &endpoint_policy,
        iroh::SecretKey::generate(),
    )
    .await
    .unwrap();
    let mut handler_policy = endpoint_policy;
    handler_policy.setup_timeout = std::time::Duration::from_millis(250);
    let client_endpoint = client.clone();
    let client_setup = tokio::spawn(async move {
        let connection = client_endpoint
            .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let (mut send, recv) = connection.open_bi().await.unwrap();
        send.write_all(&[0]).await.unwrap();
        send.flush().await.unwrap();
        (connection, (send, recv))
    });
    let incoming = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        host.endpoint.endpoint().accept(),
    )
    .await
    .expect("host should accept the silent peer")
    .expect("host endpoint should remain open");
    let remote_route = remote_client_route(incoming.remote_addr());
    let diagnostics = host.endpoint.diagnostics();
    let authenticated_endpoint_id = std::sync::OnceLock::new();
    let handler = serve_host_only_connection(
        incoming,
        handler_policy,
        host.trust.clone(),
        host.endpoint_id().to_string(),
        None,
        diagnostics.clone(),
        None,
        remote_route,
        &authenticated_endpoint_id,
    );
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let (server_result, client_result) = tokio::join!(handler, client_setup);
        let (connection, streams) = client_result.unwrap();
        drop(streams);
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
        server_result
    })
    .await
    .expect("silent first frame must not outlive the setup deadline")
    .unwrap_err();
    assert!(
        error.to_string().contains("initialize read timed out"),
        "{error}"
    );
    assert!(
        authenticated_endpoint_id.get().is_some(),
        "post-connect failures must retain the authenticated endpoint identity"
    );
    let snapshot = diagnostics.snapshot();
    assert_eq!(snapshot.active_connections, 0);
    assert_eq!(snapshot.connections_accepted, 0);
    assert_eq!(snapshot.connections_rejected, 1);
    assert_eq!(snapshot.setup_successes, 0);
    assert_eq!(snapshot.setup_failures, 1);

    client.close().await;
    drop(host);
    let _ = fs::remove_dir_all(root);
}

/// A capability-bearing host invitation pairs without provisioning, then
/// routes create replay, conflict, explicit attach, default selection, and
/// principal-filtered listing through one persistent front door. The live
/// routed attachment must expose its exact-client Iroh transport statistics
/// through `show-iroh-status`. A second paired device without routing
/// authority receives structured denials.
#[tokio::test(flavor = "current_thread")]
async fn routed_host_end_to_end_enforces_intent_idempotency_and_authority() {
    let root = test_root("routed");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        setup_timeout: std::time::Duration::from_secs(10),
        idle_timeout: std::time::Duration::from_secs(3),
        ..RuntimeIrohTransportPolicy::default()
    };
    let host = HostIrohRuntime::bind(&root, policy.clone())
        .await
        .unwrap()
        .unwrap();
    let diagnostics = host.endpoint.diagnostics();
    let router = HostSessionRouter::new(HostSessionRouterConfig {
        runtime_root: root.join("runtime"),
        owner_uid: crate::runtime::current_effective_uid(),
        config_root: root.join("config"),
        config_layers: vec![ConfigLayer {
            name: "host-iroh-test".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n"
                .to_string(),
        }],
        shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        max_sessions: 8,
        max_live_sessions: 8,
        default_session_policy: crate::host::router::HostDefaultSessionPolicy::MostRecentAttachable,
        default_lease_lifetime_seconds: 0,
    });
    let invitation = host
        .trust
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
            current_unix_seconds().unwrap(),
        )
        .unwrap();
    let denied_invitation = host
        .trust
        .create_host_invitation(
            host.endpoint_id(),
            RemoteRoleCeiling::Observer,
            RemoteHostRoutingAuthority::default(),
            600,
            current_unix_seconds().unwrap(),
        )
        .unwrap();
    let server_addr = host.endpoint_addr().unwrap();
    let client =
        crate::runtime::bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
            .await
            .unwrap();
    let denied_client =
        crate::runtime::bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
            .await
            .unwrap();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let server_router = router.clone();

    let server = host.serve_routed(server_router, async move { server_stop.notified().await });
    let client_work = async {
        let paired = exchange_test_host_initialize(
            &client,
            &server_addr,
            "extension:iroh_invitation",
            invitation.token.expose_secret(),
            "host_only",
        )
        .await;
        assert!(paired["result"]["session"].is_null(), "{paired}");
        assert!(paired["result"]["lease"].is_null(), "{paired}");
        assert_eq!(
            paired["result"]["capabilities"]["methods"],
            json!(["host/session/list"]),
            "{paired}"
        );
        assert!(router.snapshots().await.unwrap().is_empty());
        let credential = paired["result"]["device_credential"]
            .as_str()
            .unwrap()
            .to_string();

        // Lose the first response after actor attachment and provisioning
        // commit. Retry must reuse the retained session without a stale
        // attached primary or a second creation.
        fail_test_routed_initialize_write(
            &client,
            &server_addr,
            &credential,
            "create",
            Some("create-one"),
            None,
            Some("owned"),
        )
        .await;
        let snapshots = router.snapshots().await.unwrap();
        assert_eq!(snapshots.len(), 1);
        let runtime = router.runtime_for_tests(&snapshots[0].session_id).unwrap();
        assert_eq!(
            runtime.actor().lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Detached
        );

        let created = exchange_test_routed_initialize(
            &client,
            &server_addr,
            &credential,
            "create",
            Some("create-one"),
            None,
            Some("owned"),
        )
        .await;
        assert_eq!(created["result"]["selected_version"], 3, "{created}");
        assert_eq!(created["result"]["lease"]["state"], "active", "{created}");
        let lease_id = created["result"]["lease"]["lease_id"]
            .as_str()
            .unwrap()
            .to_string();
        let session_id = created["result"]["lease"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        fail_test_routed_initialize_write(
            &client,
            &server_addr,
            &credential,
            "attach",
            None,
            Some(json!({"session_id": session_id})),
            None,
        )
        .await;
        assert_eq!(
            runtime.actor().lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
        assert_eq!(
            router.get_lease(&lease_id).unwrap().state,
            RemoteSessionLeaseState::Active
        );

        for fault in ["error", "panic"] {
            fail_test_routed_event_task(&client, &server_addr, &credential, &session_id, fault)
                .await;
            assert_eq!(
                runtime.actor().lifecycle_state().await.unwrap(),
                RuntimeLifecycleState::Detached
            );
            assert_eq!(
                router.get_lease(&lease_id).unwrap().state,
                RemoteSessionLeaseState::Active
            );
        }

        let replay = exchange_test_routed_initialize(
            &client,
            &server_addr,
            &credential,
            "create",
            Some("create-one"),
            None,
            Some("owned"),
        )
        .await;
        assert_eq!(replay["result"]["lease"]["lease_id"], lease_id, "{replay}");
        assert_eq!(router.snapshots().await.unwrap().len(), 1);

        let conflict = exchange_test_routed_initialize(
            &client,
            &server_addr,
            &credential,
            "create",
            Some("create-one"),
            None,
            Some("changed"),
        )
        .await;
        assert_eq!(
            conflict["error"]["data"]["mezzanine_code"], "conflict",
            "{conflict}"
        );

        let attached = exchange_test_routed_initialize(
            &client,
            &server_addr,
            &credential,
            "attach",
            None,
            Some(json!({"session_id": session_id})),
            None,
        )
        .await;
        assert_eq!(
            attached["result"]["lease"]["lease_id"], lease_id,
            "{attached}"
        );
        let selected_default = exchange_test_routed_initialize(
            &client,
            &server_addr,
            &credential,
            "default",
            None,
            None,
            None,
        )
        .await;
        assert_eq!(
            selected_default["result"]["lease"]["lease_id"], lease_id,
            "{selected_default}"
        );
        assert_eq!(router.snapshots().await.unwrap().len(), 1);

        let listed = exchange_test_host_list(&client, &server_addr, &credential, true).await;
        assert_eq!(
            listed["result"]["sessions"].as_array().unwrap().len(),
            1,
            "{listed}"
        );
        assert_eq!(
            listed["result"]["sessions"][0]["lease_id"], lease_id,
            "{listed}"
        );

        let denied_pair = exchange_test_host_initialize(
            &denied_client,
            &server_addr,
            "extension:iroh_invitation",
            denied_invitation.token.expose_secret(),
            "host_only",
        )
        .await;
        assert_eq!(
            denied_pair["result"]["capabilities"]["methods"],
            json!([]),
            "{denied_pair}"
        );
        let denied_credential = denied_pair["result"]["device_credential"].as_str().unwrap();
        let denied_create = exchange_test_routed_initialize(
            &denied_client,
            &server_addr,
            denied_credential,
            "create",
            Some("denied-create"),
            None,
            Some("denied"),
        )
        .await;
        assert_eq!(
            denied_create["error"]["data"]["mezzanine_code"], "forbidden",
            "{denied_create}"
        );
        let denied_list =
            exchange_test_host_list(&denied_client, &server_addr, denied_credential, false).await;
        assert_eq!(
            denied_list["error"]["data"]["mezzanine_code"], "forbidden",
            "{denied_list}"
        );
        assert_eq!(router.snapshots().await.unwrap().len(), 1);

        let (persistent_connection, mut persistent_bridge, persistent_attach) =
            open_test_routed_attach(&client, &server_addr, &credential, Some(&session_id)).await;
        assert_eq!(
            persistent_attach["result"]["lease"]["lease_id"], lease_id,
            "{persistent_attach}"
        );
        let status_request = json!({
            "jsonrpc": "2.0",
            "id": "test-routed-iroh-status",
            "method": "terminal/command",
            "params": {
                "idempotency_key": "test-routed-iroh-status",
                "input": "show-iroh-status"
            }
        })
        .to_string();
        persistent_bridge
            .stream_mut()
            .write_all(&encode_control_body(&status_request))
            .await
            .unwrap();
        persistent_bridge.stream_mut().flush().await.unwrap();
        let status_response = read_one_control_frame(
            persistent_bridge.stream_mut(),
            std::time::Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert!(
            status_response.contains("| State | connected |"),
            "{status_response}"
        );
        assert!(status_response.contains("| RTT |"), "{status_response}");
        assert!(
            !status_response.contains("this client has no correlated live Iroh connection"),
            "{status_response}"
        );
        for request_id in ["test-routed-pane-two", "test-routed-pane-three"] {
            let pane_create_request = json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "pane/create",
                "params": {
                    "split": "vertical",
                    "select": true,
                    "idempotency_key": request_id
                }
            })
            .to_string();
            persistent_bridge
                .stream_mut()
                .write_all(&encode_control_body(&pane_create_request))
                .await
                .unwrap();
            persistent_bridge.stream_mut().flush().await.unwrap();
            let pane_create_response = read_one_control_frame(
                persistent_bridge.stream_mut(),
                std::time::Duration::from_secs(3),
            )
            .await
            .unwrap();
            let pane_create_response: Value = serde_json::from_str(&pane_create_response).unwrap();
            assert!(
                pane_create_response.get("error").is_none(),
                "{pane_create_response}"
            );
        }
        let panes_before_detach = exchange_test_pane_list(&mut persistent_bridge).await;
        assert_eq!(
            panes_before_detach["result"]["panes"]
                .as_array()
                .unwrap()
                .len(),
            3,
            "{panes_before_detach}"
        );
        let detach_request = json!({
            "jsonrpc": "2.0",
            "id": "test-routed-detach",
            "method": "terminal/step",
            "params": {
                "idempotency_key": "test-routed-detach",
                "render": false,
                "input_bytes": [1, 100]
            }
        })
        .to_string();
        persistent_bridge
            .stream_mut()
            .write_all(&encode_control_body(&detach_request))
            .await
            .unwrap();
        persistent_bridge.stream_mut().flush().await.unwrap();
        let detach_response = read_one_control_frame(
            persistent_bridge.stream_mut(),
            std::time::Duration::from_secs(3),
        )
        .await
        .unwrap();
        let detach_response: Value = serde_json::from_str(&detach_response).unwrap();
        assert_eq!(
            detach_response["result"]["client_detached"], true,
            "{detach_response}"
        );
        assert_eq!(
            detach_response["result"]["session_terminated"], false,
            "{detach_response}"
        );
        let _ = persistent_bridge
            .shutdown(std::time::Duration::from_secs(2))
            .await;
        persistent_connection.close(
            iroh::endpoint::VarInt::from_u32(0),
            b"detached test client complete",
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let snapshots = router.snapshots().await.unwrap();
                if snapshots.len() == 1
                    && snapshots[0].state == crate::host::session::SessionSupervisorState::Running
                    && snapshots[0].runtime_state
                        == Some(crate::runtime::RuntimeLifecycleState::Detached)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached routed session should remain supervised and reattachable");
        assert_eq!(
            router.get_lease(&lease_id).unwrap().state,
            RemoteSessionLeaseState::Active
        );

        let (persistent_connection, mut persistent_bridge, persistent_attach) =
            open_test_routed_attach(&client, &server_addr, &credential, None).await;
        assert_eq!(
            persistent_attach["result"]["lease"]["lease_id"], lease_id,
            "{persistent_attach}"
        );
        let panes_after_reconnect = exchange_test_pane_list(&mut persistent_bridge).await;
        assert_eq!(
            panes_after_reconnect["result"]["panes"]
                .as_array()
                .unwrap()
                .len(),
            3,
            "{panes_after_reconnect}"
        );
        assert_eq!(router.snapshots().await.unwrap().len(), 1);
        drop(persistent_bridge);
        persistent_connection.close(
            iroh::endpoint::VarInt::from_u32(1),
            b"abrupt routed client loss",
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let snapshots = router.snapshots().await.unwrap();
                if snapshots.len() == 1
                    && snapshots[0].state == crate::host::session::SessionSupervisorState::Running
                    && snapshots[0].runtime_state
                        == Some(crate::runtime::RuntimeLifecycleState::Detached)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("abrupt routed client loss should leave the session reattachable");
        assert_eq!(
            router.get_lease(&lease_id).unwrap().state,
            RemoteSessionLeaseState::Active
        );

        let (persistent_connection, mut persistent_bridge, persistent_attach) =
            open_test_routed_attach(&client, &server_addr, &credential, Some(&session_id)).await;
        assert_eq!(
            persistent_attach["result"]["lease"]["lease_id"], lease_id,
            "{persistent_attach}"
        );
        let record_id = host
            .trust
            .list_records()
            .unwrap()
            .into_iter()
            .find(|record| record.host_routing.session_create && !record.revoked())
            .unwrap()
            .id;
        host.trust
            .revoke_record(
                &record_id,
                Some("test active connection fence"),
                current_unix_seconds().unwrap(),
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut buffer = [0u8; 1024];
            loop {
                match persistent_bridge.stream_mut().read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await
        .expect("trust revocation should close an idle routed connection");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            persistent_connection.closed().await;
        })
        .await
        .expect("revocation must close the connection without waiting for peer control FIN");
        drop(persistent_bridge);
        persistent_connection.close(
            iroh::endpoint::VarInt::from_u32(0),
            b"revoked test complete",
        );

        let revoked_reconnect = exchange_test_routed_initialize(
            &client,
            &server_addr,
            &credential,
            "attach",
            None,
            Some(json!({"session_id": session_id})),
            None,
        )
        .await;
        assert_eq!(
            revoked_reconnect["error"]["data"]["mezzanine_code"], "forbidden",
            "{revoked_reconnect}"
        );
        stop.notify_one();
    };

    let (served, ()) = tokio::join!(server, client_work);
    assert_eq!(served.unwrap(), 18);
    let snapshot = diagnostics.snapshot();
    assert!(!snapshot.listener_active);
    assert_eq!(snapshot.active_connections, 0);
    assert_eq!(snapshot.connections_accepted, 18);
    assert_eq!(snapshot.setup_successes, 18);
    assert_eq!(snapshot.connections_rejected, snapshot.setup_failures);
    assert_eq!(
        snapshot
            .connections_completed
            .saturating_add(snapshot.connections_failed),
        18
    );
    router
        .shutdown_all(true, std::time::Duration::from_secs(2))
        .await
        .unwrap();
    client.close().await;
    denied_client.close().await;
    drop(host);
    let _ = fs::remove_dir_all(root);
}

async fn exchange_test_host_initialize(
    client: &iroh::Endpoint,
    server_addr: &iroh::EndpointAddr,
    mechanism: &str,
    token: &str,
    intent: &str,
) -> Value {
    let connection = client
        .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
        .await
        .unwrap();
    let (send, recv) = connection.open_bi().await.unwrap();
    let compression = IrohCompressionPolicy::new(
        RuntimeIrohCompressionCodec::None,
        512,
        3,
        HOST_CONTROL_MAX_CONTENT_LENGTH + 1024,
    )
    .unwrap();
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, HOST_CONTROL_MAX_CONTENT_LENGTH)
            .unwrap();
    let request = json!({
        "jsonrpc": "2.0",
        "id": "test-init",
        "method": "control/initialize",
        "params": {
            "client_name": "test-client",
            "requested_version": 3,
            "requested_role": "observer",
            "session_intent": intent,
            "authentication": { "mechanism": mechanism, "token": token }
        }
    })
    .to_string();
    bridge
        .stream_mut()
        .write_all(&encode_control_body(&request))
        .await
        .unwrap();
    bridge.stream_mut().shutdown().await.unwrap();
    let response = read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(2))
        .await
        .unwrap();
    let _ = bridge.shutdown(std::time::Duration::from_secs(2)).await;
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
    serde_json::from_str(&response).unwrap()
}

async fn exchange_test_routed_initialize(
    client: &iroh::Endpoint,
    server_addr: &iroh::EndpointAddr,
    credential: &str,
    intent: &str,
    idempotency_key: Option<&str>,
    target: Option<Value>,
    session_name: Option<&str>,
) -> Value {
    let mut client_metadata = json!({
        "name": "test-client",
        "interactive": true,
        "terminal": {"columns": 80, "rows": 24, "term": "xterm-256color"}
    });
    if let Some(session_name) = session_name {
        client_metadata["metadata"] = json!({"session_name": session_name});
    }
    let mut params = json!({
        "client_name": "test-client",
        "requested_version": 3,
        "requested_role": "primary",
        "detach_primary_on_disconnect": true,
        "session_intent": intent,
        "client": client_metadata,
        "authentication": {
            "mechanism": "extension:iroh_device",
            "token": credential
        }
    });
    if let Some(idempotency_key) = idempotency_key {
        params["idempotency_key"] = Value::String(idempotency_key.to_string());
    }
    if let Some(target) = target {
        params["session_target"] = target;
    }
    exchange_test_initialize_params(client, server_addr, params).await
}

async fn open_test_routed_attach(
    client: &iroh::Endpoint,
    server_addr: &iroh::EndpointAddr,
    credential: &str,
    session_id: Option<&str>,
) -> (iroh::endpoint::Connection, IrohCompressionBridge, Value) {
    let connection = client
        .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
        .await
        .unwrap();
    let (send, recv) = connection.open_bi().await.unwrap();
    let compression = IrohCompressionPolicy::new(
        RuntimeIrohCompressionCodec::None,
        512,
        3,
        HOST_CONTROL_MAX_CONTENT_LENGTH + 1024,
    )
    .unwrap();
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, HOST_CONTROL_MAX_CONTENT_LENGTH)
            .unwrap();
    let mut params = json!({
        "client_name": "test-client",
        "requested_version": 3,
        "requested_role": "primary",
        "detach_primary_on_disconnect": true,
        "session_intent": if session_id.is_some() {
            "attach"
        } else {
            "resolve_or_create"
        },
        "client": {
            "name": "test-client",
            "interactive": true,
            "terminal": {"columns": 80, "rows": 24, "term": "xterm-256color"}
        },
        "authentication": {
            "mechanism": "extension:iroh_device",
            "token": credential
        }
    });
    if let Some(session_id) = session_id {
        params["session_target"] = json!({"session_id": session_id});
    } else {
        params["idempotency_key"] = Value::String("test-persistent-resolve-or-create".to_string());
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": "test-persistent-attach",
        "method": "control/initialize",
        "params": params
    })
    .to_string();
    bridge
        .stream_mut()
        .write_all(&encode_control_body(&request))
        .await
        .unwrap();
    bridge.stream_mut().flush().await.unwrap();
    let response = read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
        .await
        .unwrap();
    (connection, bridge, serde_json::from_str(&response).unwrap())
}

async fn exchange_test_pane_list(bridge: &mut IrohCompressionBridge) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": "test-persistent-pane-list",
        "method": "pane/list",
        "params": {}
    })
    .to_string();
    bridge
        .stream_mut()
        .write_all(&encode_control_body(&request))
        .await
        .unwrap();
    bridge.stream_mut().flush().await.unwrap();
    let response = read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
        .await
        .unwrap();
    serde_json::from_str(&response).unwrap()
}

/// Keeps the request direction live after initialization while a negotiated
/// event worker fails or panics. Required-event loss must close control
/// without client FIN and without deleting the committed routed session.
async fn fail_test_routed_event_task(
    client: &iroh::Endpoint,
    server_addr: &iroh::EndpointAddr,
    credential: &str,
    session_id: &str,
    fault: &str,
) {
    let connection = client
        .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
        .await
        .unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let request = json!({"jsonrpc":"2.0", "id":format!("test-event-task-{fault}"),
    "method":"control/initialize", "params":{
        "client_name":"event-fault", "requested_version":3, "requested_role":"primary",
        "detach_primary_on_disconnect":true, "event_stream_version":1,
        "session_intent":"attach", "session_target":{"session_id":session_id},
        "client":{"name":"event-fault", "interactive":true,
            "terminal":{"columns":80,"rows":24,"term":"xterm"}},
        "authentication":{"mechanism":"extension:iroh_device","token":credential}
    }})
    .to_string();
    send.write_all(&encode_control_body(&request))
        .await
        .unwrap();
    send.flush().await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), recv.read_to_end(4096))
        .await
        .expect("event failure must end live control")
        .unwrap();
    let (body, consumed) = decode_control_frame(&response, 4096).unwrap();
    assert_eq!(consumed, response.len());
    assert!(body.contains("granted_role"), "{body}");
    tokio::time::timeout(std::time::Duration::from_secs(10), connection.closed())
        .await
        .unwrap();
}

/// Sends valid routed initialization into the request-local broken-bridge
/// seam. Connection termination proves the wrapper passed its acknowledged
/// attachment cleanup boundary before the caller inspects actor state.
async fn fail_test_routed_initialize_write(
    client: &iroh::Endpoint,
    server_addr: &iroh::EndpointAddr,
    credential: &str,
    intent: &str,
    key: Option<&str>,
    target: Option<Value>,
    name: Option<&str>,
) {
    let connection = client
        .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
        .await
        .unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let mut params = json!({
        "client_name":"test-client", "requested_version":3, "requested_role":"primary",
        "detach_primary_on_disconnect":true, "session_intent":intent,
        "client":{"name":"test-client","interactive":true,
            "terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},
        "authentication":{"mechanism":"extension:iroh_device","token":credential}
    });
    if let Some(key) = key {
        params["idempotency_key"] = json!(key);
    }
    if let Some(target) = target {
        params["session_target"] = target;
    }
    if let Some(name) = name {
        params["client"]["metadata"] = json!({"session_name":name});
    }
    let body = json!({"jsonrpc":"2.0", "id":"test-fail-routed-response",
        "method":"control/initialize", "params":params})
    .to_string();
    send.write_all(&encode_control_body(&body)).await.unwrap();
    send.finish().unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), recv.read_to_end(4096))
        .await
        .unwrap();
    assert!(
        response.is_err() || response.unwrap().is_empty(),
        "failed bridge cannot deliver success"
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), connection.closed())
        .await
        .unwrap();
}

async fn exchange_test_initialize_params(
    client: &iroh::Endpoint,
    server_addr: &iroh::EndpointAddr,
    params: Value,
) -> Value {
    let connection = client
        .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
        .await
        .unwrap();
    let (send, recv) = connection.open_bi().await.unwrap();
    let compression = IrohCompressionPolicy::new(
        RuntimeIrohCompressionCodec::None,
        512,
        3,
        HOST_CONTROL_MAX_CONTENT_LENGTH + 1024,
    )
    .unwrap();
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, HOST_CONTROL_MAX_CONTENT_LENGTH)
            .unwrap();
    let request = json!({
        "jsonrpc": "2.0",
        "id": "test-routed-init",
        "method": "control/initialize",
        "params": params,
    })
    .to_string();
    bridge
        .stream_mut()
        .write_all(&encode_control_body(&request))
        .await
        .unwrap();
    bridge.stream_mut().shutdown().await.unwrap();
    let response = read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
        .await
        .unwrap();
    let _ = bridge.shutdown(std::time::Duration::from_secs(3)).await;
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
    serde_json::from_str(&response).unwrap()
}

async fn exchange_test_host_list(
    client: &iroh::Endpoint,
    server_addr: &iroh::EndpointAddr,
    credential: &str,
    keep_control_open: bool,
) -> Value {
    let connection = client
        .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
        .await
        .unwrap();
    let (send, recv) = connection.open_bi().await.unwrap();
    let compression = IrohCompressionPolicy::new(
        RuntimeIrohCompressionCodec::None,
        512,
        3,
        HOST_CONTROL_MAX_CONTENT_LENGTH + 1024,
    )
    .unwrap();
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, HOST_CONTROL_MAX_CONTENT_LENGTH)
            .unwrap();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": "test-list-init",
        "method": "control/initialize",
        "params": {
            "client_name": "test-client",
            "requested_version": 3,
            "requested_role": "observer",
            "session_intent": "host_only",
            "authentication": {
                "mechanism": "extension:iroh_device",
                "token": credential
            }
        }
    })
    .to_string();
    bridge
        .stream_mut()
        .write_all(&encode_control_body(&initialize))
        .await
        .unwrap();
    bridge.stream_mut().flush().await.unwrap();
    let initialized =
        read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
            .await
            .unwrap();
    let initialized: Value = serde_json::from_str(&initialized).unwrap();
    assert!(initialized.get("result").is_some(), "{initialized}");
    let list = json!({
        "jsonrpc": "2.0",
        "id": "test-list",
        "method": "host/session/list",
        "params": {}
    })
    .to_string();
    bridge
        .stream_mut()
        .write_all(&encode_control_body(&list))
        .await
        .unwrap();
    if !keep_control_open {
        bridge.stream_mut().shutdown().await.unwrap();
    }
    let response = read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
        .await
        .unwrap();
    if keep_control_open {
        tokio::time::timeout(std::time::Duration::from_secs(2), connection.closed())
            .await
            .expect("host-only completion must close without peer control FIN");
        drop(bridge);
    } else {
        let _ = bridge.shutdown(std::time::Duration::from_secs(3)).await;
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
    }
    serde_json::from_str(&response).unwrap()
}
