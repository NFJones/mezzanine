//! Invitation handoff uses two exact IPC owners and one original creation key.
use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use futures_util::{SinkExt, StreamExt};
use std::os::unix::fs::PermissionsExt;
use tokio_util::codec::Framed;

mod real_host;

/// Supplies synthetic protected invitation data without developer credentials.
fn fixture() -> (
    PathBuf,
    crate::cli::CliEnv,
    crate::config::ConfigPaths,
    PathBuf,
) {
    let home = std::env::temp_dir().join(format!(
        "mez-invite-handoff-{:032x}",
        rand::random::<u128>()
    ));
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let invitation = home.join("invitation.json");
    crate::security::remote::write_remote_invitation_file_new(&invitation, serde_json::json!({
        "format_version":1,"profile_name":"authored","server_addr":EndpointAddr::new(iroh::SecretKey::generate().public()),
        "role":"primary","profile_scope":"host","token":"synthetic-private-proof","expires_at_unix_seconds":u64::MAX
    }).to_string().as_bytes()).unwrap();
    (home, env, paths, invitation)
}

/// Missing discovery retains direct eligibility; expired proof, role escalation
/// and current outbound veto reject before any pairing request or endpoint key.
/// These fixtures never redeem an invitation or acquire transport ownership.
#[tokio::test]
async fn broker_invitation_early_rejections_preserve_proof_and_identity() {
    for case in ["missing", "expired", "ceiling", "veto"] {
        let (home, env, paths, path) = fixture();
        let mut invitation: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if case == "expired" {
            invitation["expires_at_unix_seconds"] = serde_json::json!(0);
        }
        if case == "ceiling" {
            invitation["role"] = serde_json::json!("observer");
        }
        std::fs::write(&path, invitation.to_string()).unwrap();
        if case == "veto" {
            std::fs::write(
                paths.default_primary_file(),
                format!(
                    "version = {}\n[transport.iroh]\noutbound_enabled = false\n",
                    crate::config::CURRENT_CONFIG_SCHEMA_VERSION
                ),
            )
            .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let routing = IrohSessionRouting::Create {
            name: Some("fresh".into()),
            idempotency_key: "original".into(),
        };
        let result = try_open(
            &path, None, &env, "primary", &routing, 80, 24, "xterm", false,
        )
        .await;
        if case == "missing" {
            assert!(matches!(result, Ok(None)));
        } else {
            assert!(result.is_err());
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        assert_eq!(routing.idempotency_key(), Some("original"));
        std::fs::remove_dir_all(home).unwrap();
    }
}

/// Performs strict readiness for a synthetic local peer; handles differ across
/// pairing and attachment streams and cannot borrow one another's settlement.
async fn hello(
    listener: &tokio::net::UnixListener,
    generation: u64,
) -> (
    Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
    serde_json::Value,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut stream = Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap());
    stream.next().await.unwrap().unwrap();
    let handle =
        serde_json::json!({"owner":"00000000000000000000000000000001","generation":generation});
    stream
        .send(ProtocolFrame::new(
            "application/vnd.mezzanine.outbound+json",
            serde_json::json!({"protocol":"mez-outbound/1","handle":handle}).to_string(),
        ))
        .await
        .unwrap();
    (stream, handle)
}

/// Oversized TERM is valid initialization grammar but cannot fit the local
/// attachment envelope. Reject it before any pairing request consumes proof;
/// unchanged invitation bytes and zero wire operations remain observable.
#[tokio::test]
async fn broker_invitation_oversized_setup_rejects_before_pair_request() {
    let (home, env, paths, path) = fixture();
    let before = std::fs::read(&path).unwrap();
    let socket = paths.root().join("outbound.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let routing = IrohSessionRouting::Create {
        name: Some("fresh".into()),
        idempotency_key: "original-create".into(),
    };
    let term = "x".repeat(5 * 1024);
    let peer = async {
        let (mut client, _) = hello(&listener, 1).await;
        let next = client.next().await;
        assert!(
            next.is_none(),
            "oversized setup must not submit a pairing operation"
        );
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            try_open(
                &path,
                Some("alias"),
                &env,
                "primary",
                &routing,
                80,
                24,
                &term,
                false
            ),
            peer
        )
    })
    .await
    .unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!paths.root().join("remote/client/endpoint.key").exists());
    drop(listener);
    std::fs::remove_dir_all(home).unwrap();
}

/// Pairing must transmit only the protected file reference. The later setup
/// preserves the original Create key/name and contains no authentication proof.
/// Discovery disappearance after pairing is terminal, with no direct eligibility
/// or competing key; success retains only the exact attachment session.
#[tokio::test]
async fn broker_invitation_handoff_preserves_key_and_rejects_post_pair_absence() {
    for disappear in [false, true] {
        let (home, env, paths, path) = fixture();
        let socket = paths.root().join("outbound.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let routing = IrohSessionRouting::Create {
            name: Some("fresh".into()),
            idempotency_key: "original-create".into(),
        };
        let peer = async {
            let (mut pairing, handle) = hello(&listener, 1).await;
            let request = pairing.next().await.unwrap().unwrap();
            let request: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(
                request,
                serde_json::json!({"operation":"pair","handle":handle,"path":path,"save_as":"alias"})
            );
            assert!(!request.to_string().contains("synthetic-private-proof"));
            if disappear {
                std::fs::remove_file(&socket).unwrap();
            }
            pairing
                .send(ProtocolFrame::new(
                    "application/vnd.mezzanine.outbound+json",
                    serde_json::json!({"handle":handle,"paired":true,"profile":"alias"})
                        .to_string(),
                ))
                .await
                .unwrap();
            assert!(pairing.next().await.is_none());
            if disappear {
                return None;
            }
            let (mut attached, handle) = hello(&listener, 2).await;
            let setup = attached.next().await.unwrap().unwrap();
            let setup: serde_json::Value = serde_json::from_str(&setup.body).unwrap();
            assert_eq!(setup["profile"], "alias");
            assert_eq!(setup["initialize"]["idempotency_key"], "original-create");
            assert_eq!(setup["initialize"]["session_intent"], "create");
            assert_eq!(
                setup["initialize"]["client"]["metadata"]["session_name"],
                "fresh"
            );
            assert!(setup["initialize"].get("authentication").is_none());
            attached.next().await.unwrap().unwrap();
            attached.send(ProtocolFrame::new("application/vnd.mezzanine.outbound+json", serde_json::json!({
                "handle":handle,"session":{"selected_version":3,"granted_role":"primary","session_id":"$1","lease_id":"lease-one","client_id":"c1"},
                "lines":["retained"],"line_style_spans":[[]],"cursor":{"row":0,"column":0,"visible":false},"output_modes":{},"presentation_ids":[]
            }).to_string())).await.unwrap();
            Some(attached)
        };
        let (result, peer) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                try_open(
                    &path,
                    Some("alias"),
                    &env,
                    "primary",
                    &routing,
                    80,
                    24,
                    "xterm",
                    false
                ),
                peer
            )
        })
        .await
        .unwrap();
        if disappear {
            assert!(
                result.is_err(),
                "post-pair owner loss must never return None"
            );
        } else {
            let attachment = result.unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(attachment.session.summary()).unwrap()["session_id"],
                "$1"
            );
            drop(attachment);
            assert!(peer.unwrap().next().await.is_none());
        }
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        drop(listener);
        std::fs::remove_dir_all(home).unwrap();
    }
}
