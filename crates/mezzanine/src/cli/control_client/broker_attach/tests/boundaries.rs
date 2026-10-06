//! Read-only discovery and uncertain-setup boundaries with synthetic local peers.
//!
//! Fixtures own disposable configuration roots and synthetic protected profiles.
//! They neither dial a remote host nor allocate persistent endpoint identities.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use futures_util::{SinkExt, StreamExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use tokio_util::codec::Framed;

/// Provisions an isolated authored root and a synthetic host profile. Saving a
/// profile must not create the endpoint key used by a live transport owner.
fn fixture() -> (PathBuf, crate::cli::CliEnv, crate::config::ConfigPaths) {
    let home = std::env::temp_dir().join(format!(
        "mez-attach-boundary-{:032x}",
        rand::random::<u128>()
    ));
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    RemoteClientProfileStore::under_config_root(paths.root())
        .save(&RemoteClientProfile {
            name: "fixture".into(),
            server_addr: EndpointAddr::new(iroh::SecretKey::generate().public()),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: SecretString::from("synthetic-private-proof".to_string()),
        })
        .unwrap();
    (home, env, paths)
}

/// Absent discovery leaves direct setup eligible without endpoint mutation.
/// Outbound veto and unsafe discovery reject instead of authorizing fallback or
/// repairing authored paths. The caller's Create operation remains unchanged.
#[tokio::test]
async fn broker_attach_missing_veto_and_unsafe_discovery_are_distinct() {
    for mode in ["missing", "refused", "veto", "unsafe"] {
        let (home, env, paths) = fixture();
        let authored = paths.root().join("authored");
        std::fs::write(&authored, b"preserve").unwrap();
        if mode == "refused" {
            let listener =
                std::os::unix::net::UnixListener::bind(paths.root().join("outbound.sock")).unwrap();
            std::fs::set_permissions(
                paths.root().join("outbound.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            drop(listener);
        } else if mode != "missing" {
            symlink(&authored, paths.root().join("outbound.sock")).unwrap();
        }
        if mode == "veto" {
            std::fs::write(
                paths.default_primary_file(),
                format!(
                    "version = {}\n[transport.iroh]\noutbound_enabled = false\n",
                    crate::config::CURRENT_CONFIG_SCHEMA_VERSION
                ),
            )
            .unwrap();
        }
        let routing = IrohSessionRouting::Create {
            name: Some("second".into()),
            idempotency_key: "original".into(),
        };
        let result = try_open(
            &crate::cli::ControlTargetSelection::IrohProfile("fixture".into()),
            &env,
            "primary",
            &routing,
            80,
            24,
            "xterm",
            false,
        )
        .await;
        match result {
            Ok(None) => assert!(matches!(mode, "missing" | "refused")),
            Err(error) if mode == "veto" => assert!(
                error
                    .message()
                    .contains("outbound Iroh connections are disabled")
            ),
            Err(error) if mode == "unsafe" => assert!(
                error
                    .message()
                    .contains("discovery socket must remain private")
            ),
            _ => panic!("unexpected discovery outcome for {mode}"),
        }
        assert_eq!(std::fs::read(&authored).unwrap(), b"preserve");
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        assert_eq!(routing.idempotency_key(), Some("original"));
        std::fs::remove_dir_all(home).unwrap();
    }
}

/// Successful setup retains the exact broker session and independently selected
/// local clipboard adapter. It sends one original-key setup and a view request,
/// never device proof or an endpoint-key acquisition. This synthetic IPC test
/// qualifies the CLI adapter, not remote authorization or interactive output.
#[tokio::test]
async fn broker_attach_success_retains_exact_session_without_endpoint_acquisition() {
    let (home, env, paths) = fixture();
    let socket = paths.root().join("outbound.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let routing = IrohSessionRouting::Create {
        name: Some("second 雪".into()),
        idempotency_key: "exact-original-create".into(),
    };
    let target = crate::cli::ControlTargetSelection::IrohProfile("fixture".into());
    let peer = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap());
        stream.next().await.unwrap().unwrap();
        let handle = serde_json::json!({"owner":"00000000000000000000000000000001","generation":1});
        stream
            .send(ProtocolFrame::new(
                "application/vnd.mezzanine.outbound+json",
                serde_json::json!({"protocol":"mez-outbound/1","handle":handle}).to_string(),
            ))
            .await
            .unwrap();
        let setup = stream.next().await.unwrap().unwrap();
        let setup: serde_json::Value = serde_json::from_str(&setup.body).unwrap();
        assert_eq!(
            setup["initialize"]["idempotency_key"],
            "exact-original-create"
        );
        assert_eq!(setup["initialize"]["session_intent"], "create");
        assert_eq!(setup["initialize"]["event_stream_version"], 2);
        assert_eq!(
            setup["initialize"]["client"]["metadata"]["session_name"],
            "second 雪"
        );
        assert!(setup["initialize"].get("authentication").is_none());
        let view = stream.next().await.unwrap().unwrap();
        let view: serde_json::Value = serde_json::from_str(&view.body).unwrap();
        assert_eq!(
            view,
            serde_json::json!({"handle":handle,"columns":80,"rows":24})
        );
        stream
            .send(ProtocolFrame::new(
                "application/vnd.mezzanine.outbound+json",
                serde_json::json!({"handle":handle,"session":{
                "selected_version":3,"granted_role":"primary","session_id":"$1",
                "lease_id":"lease-one","client_id":"c1"},"lines":["retained output"],
                "line_style_spans":[[]],"cursor":{"row":0,"column":0,"visible":false},
                "output_modes":{},"presentation_ids":[]})
                .to_string(),
            ))
            .await
            .unwrap();
        stream
    };
    let (result, mut peer) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            try_open(&target, &env, "primary", &routing, 80, 24, "xterm", false),
            peer
        )
    })
    .await
    .unwrap();
    let attachment = result.unwrap().unwrap();
    assert!(attachment.primary);
    assert_eq!(
        attachment.budget,
        crate::runtime::RuntimeIrohTransportPolicy::default().setup_timeout
    );
    let summary = serde_json::to_value(attachment.session.summary()).unwrap();
    assert_eq!(summary["session_id"], "$1");
    assert_eq!(summary["client_id"], "c1");
    assert!(!paths.root().join("remote/client/endpoint.key").exists());
    drop(attachment);
    assert!(
        peer.next().await.is_none(),
        "setup adapter must not replay on disposal"
    );
    drop(peer);
    drop(listener);
    std::fs::remove_dir_all(home).unwrap();
}

/// Authenticated readiness is not absence. Protocol failure, unsupported X11,
/// and lost initial settlement are terminal; none may acquire a competing key,
/// export proof, allocate another invocation key or retry via the direct path.
#[tokio::test]
async fn broker_attach_ready_errors_never_authorize_direct_fallback() {
    for mode in ["protocol", "x11", "lost-setup", "disappeared"] {
        let (home, env, paths) = fixture();
        let socket = paths.root().join("outbound.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let routing = IrohSessionRouting::Create {
            name: Some("second".into()),
            idempotency_key: "exact-operation".into(),
        };
        let target = crate::cli::ControlTargetSelection::IrohProfile("fixture".into());
        let peer = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap());
            stream.next().await.unwrap().unwrap();
            let handle =
                serde_json::json!({"owner":"00000000000000000000000000000001","generation":1});
            if mode == "disappeared" {
                std::fs::remove_file(&socket).unwrap();
            }
            stream.send(ProtocolFrame::new("application/vnd.mezzanine.outbound+json",
                serde_json::json!({"protocol":if mode == "protocol" { "wrong" } else { "mez-outbound/1" },"handle":handle}).to_string())).await.unwrap();
            if mode == "lost-setup" {
                let setup = stream.next().await.unwrap().unwrap();
                let setup: serde_json::Value = serde_json::from_str(&setup.body).unwrap();
                assert_eq!(setup["initialize"]["idempotency_key"], "exact-operation");
                assert_eq!(setup["initialize"]["session_intent"], "create");
                assert_eq!(
                    setup["initialize"]["client"]["metadata"]["session_name"],
                    "second"
                );
                assert!(setup["initialize"].get("authentication").is_none());
                assert!(!setup.to_string().contains("synthetic-private-proof"));
                // Deliberately lose settlement after setup. Closing this stream
                // cannot authorize another initialization on another endpoint.
            } else {
                assert!(
                    stream.next().await.is_none(),
                    "rejected readiness/X11 must send no setup"
                );
            }
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                try_open(
                    &target,
                    &env,
                    "primary",
                    &routing,
                    80,
                    24,
                    "xterm",
                    mode == "x11"
                ),
                peer
            )
        })
        .await
        .unwrap();
        assert!(
            result.is_err(),
            "ready broker failure must never yield direct fallback"
        );
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        drop(listener);
        std::fs::remove_dir_all(home).unwrap();
    }
}
