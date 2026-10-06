//! Management handoff through disposable authenticated IPC, without remote effects.
//!
//! A held persistent identity reproduces the active owner's exclusion. Pairing
//! consumes one stream and management uses fresh readiness; uncertain outcomes
//! cannot restore direct ownership or resend redemption/destructive mutations.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use futures_util::{SinkExt, StreamExt};
use std::os::unix::fs::PermissionsExt;
use tokio_util::codec::Framed;

/// Negotiates one independent local handle; no credential enters the hello.
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
    let handle = serde_json::json!({"owner":"f".repeat(32),"generation":generation});
    stream
        .send(ProtocolFrame::new(
            "application/vnd.mezzanine.outbound+json",
            serde_json::json!({"protocol":"mez-outbound/1","handle":handle}).to_string(),
        ))
        .await
        .unwrap();
    (stream, handle)
}

/// Listing and kill each redeem only once through the owner, then submit their
/// fixed operation with exact alias/target and one retained mutation key. Lost
/// pairing/reconnect/management replies remain terminal while the identity lock
/// is held. Success exposes only closed settlement, not invitation/device proof.
#[tokio::test]
async fn remote_invitation_management_handoff_preserves_operation_without_replay() {
    for kill in [false, true] {
        for case in ["success", "pair-loss", "reconnect-loss", "operation-loss"] {
            let home = std::env::temp_dir().join(format!("mez-im-{:032x}", rand::random::<u128>()));
            let env = crate::cli::CliEnv {
                home: Some(home.clone()),
                ..Default::default()
            };
            let paths = env.config_paths().unwrap();
            paths.ensure_default_config().unwrap();
            let path = home.join("invitation.json");
            crate::security::remote::write_remote_invitation_file_new(&path, serde_json::json!({
                "format_version":1,"profile_name":"fixture","server_addr":EndpointAddr::new(iroh::SecretKey::generate().public())
                    .with_ip_addr("127.0.0.1:43210".parse().unwrap()),
                "role":"primary","profile_scope":"host","token":"synthetic-first-use-proof","expires_at_unix_seconds":u64::MAX
            }).to_string().as_bytes()).unwrap();
            let before = std::fs::read(&path).unwrap();
            let identity = RemoteClientIdentity::load_or_create(paths.root()).unwrap();
            let listener =
                tokio::net::UnixListener::bind(paths.root().join("outbound.sock")).unwrap();
            std::fs::set_permissions(
                paths.root().join("outbound.sock"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let selection = crate::cli::ControlTargetSelection::IrohInvitation {
                path: path.clone(),
                save_as: Some("alias".into()),
            };
            let peer = async {
                let (mut pairing, handle) = hello(&listener, 1).await;
                let frame = pairing.next().await.unwrap().unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&frame.body).unwrap(),
                    serde_json::json!({"operation":"pair","handle":handle,"path":path,"save_as":"alias"})
                );
                assert!(!frame.body.contains("synthetic-first-use-proof"));
                if case == "pair-loss" {
                    return;
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
                if case == "reconnect-loss" {
                    let (mut reconnect, _) = listener.accept().await.unwrap();
                    use tokio::io::AsyncReadExt;
                    let mut bytes = [0; 4096];
                    assert!(reconnect.read(&mut bytes).await.unwrap() > 0);
                    return;
                }
                let (mut management, handle) = hello(&listener, 2).await;
                let setup = management.next().await.unwrap().unwrap();
                let setup: serde_json::Value = serde_json::from_str(&setup.body).unwrap();
                assert_eq!(setup["handle"], handle);
                assert_eq!(setup["profile"], "alias");
                assert_eq!(setup["initialize"]["session_intent"], "host_only");
                assert!(setup["initialize"].get("authentication").is_none());
                let request = management.next().await.unwrap().unwrap();
                let request: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                assert_eq!(request["handle"], handle);
                if kill {
                    assert_eq!(request["operation"], "kill");
                    assert_eq!(request["target"], "$1");
                    assert!(
                        request["idempotency_key"]
                            .as_str()
                            .unwrap()
                            .starts_with("cli-remote-session-kill-")
                    );
                } else {
                    assert_eq!(
                        request,
                        serde_json::json!({"handle":handle,"authentication_only":false})
                    );
                }
                if case == "operation-loss" {
                    return;
                }
                let host = serde_json::json!({"selected_version":3,"granted_role":"observer","host_only":true});
                let reply = if kill {
                    serde_json::json!({"handle":handle,"host":host,"target":"$1","idempotency_key":request["idempotency_key"],
                        "settlement":{"killed":true,"lease_id":"lease-one","session_id":"$1","state":"revoked"}})
                } else {
                    serde_json::json!({"handle":handle,"host":host,"sessions":[]})
                };
                management
                    .send(ProtocolFrame::new(
                        "application/vnd.mezzanine.outbound+json",
                        reply.to_string(),
                    ))
                    .await
                    .unwrap();
                assert!(
                    management.next().await.is_none(),
                    "management must retire without replay"
                );
            };
            let operation = async {
                if kill {
                    force_kill_iroh_host_session(&selection, &env, "$1").await
                } else {
                    list_iroh_host_sessions(&selection, &env).await
                }
            };
            let ((), result) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(Box::pin(peer), Box::pin(operation))
            })
            .await
            .unwrap();
            if case == "success" {
                let response: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
                if kill {
                    assert_eq!(response["result"]["killed"], true);
                } else {
                    assert_eq!(response["result"]["sessions"], serde_json::json!([]));
                }
            } else {
                assert!(result.is_err());
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err(),
                "uncertain handoff must not reconnect or replay"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            drop((listener, identity));
            std::fs::remove_dir_all(home).unwrap();
        }
    }
}
