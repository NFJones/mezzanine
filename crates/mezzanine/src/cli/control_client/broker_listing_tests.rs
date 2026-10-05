//! Ordinary listing discovery boundaries without remote dialing or credentials.
//!
//! Policy and unsafe/protocol discovery failures must stop before direct identity
//! acquisition. Missing discovery alone retains the existing direct path.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use futures_util::{SinkExt, StreamExt};
use std::os::unix::fs::symlink;
use tokio_util::codec::Framed;

/// Supplies a disposable authored primary root, not the user's configuration.
fn fixture() -> (PathBuf, crate::cli::CliEnv, crate::config::ConfigPaths) {
    let home =
        std::env::temp_dir().join(format!("mez-list-boundary-{:032x}", rand::random::<u128>()));
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    (home, env, paths)
}

/// Outbound veto must precede broker connection and identity creation. An unsafe
/// socket entry must reject rather than trigger direct fallback or overwrite it.
#[tokio::test]
async fn remote_list_broker_veto_and_unsafe_discovery_are_terminal() {
    for veto in [true, false] {
        let (home, env, paths) = fixture();
        let authored = paths.root().join("authored");
        std::fs::write(&authored, b"preserve").unwrap();
        symlink(&authored, paths.root().join("outbound.sock")).unwrap();
        if veto {
            std::fs::write(
                paths.default_primary_file(),
                "version = 101\n[transport.iroh]\noutbound_enabled = false\n",
            )
            .unwrap();
        }
        let error = list_iroh_host_sessions(
            &crate::cli::ControlTargetSelection::IrohProfile("missing".into()),
            &env,
        )
        .await
        .unwrap_err();
        if veto {
            assert!(
                error
                    .message()
                    .contains("outbound Iroh connections are disabled")
            );
        } else {
            assert!(
                error
                    .message()
                    .contains("discovery socket must remain private")
            );
        }
        assert_eq!(std::fs::read(authored).unwrap(), b"preserve");
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        std::fs::remove_dir_all(home).unwrap();
    }
}

/// A reachable broker's malformed hello is not absence. Protocol failure must
/// propagate without falling back to profile lookup or competing identity use.
#[tokio::test]
async fn remote_list_broker_protocol_failure_never_falls_back() {
    let (home, env, paths) = fixture();
    let socket = paths.root().join("outbound.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let target = crate::cli::ControlTargetSelection::IrohProfile("missing".into());
    let peer = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap());
        stream.next().await.unwrap().unwrap();
        stream.send(ProtocolFrame::new("application/vnd.mezzanine.outbound+json", "{\"protocol\":\"wrong\",\"handle\":{\"owner\":\"00000000000000000000000000000000\",\"generation\":1}}")).await.unwrap();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(list_iroh_host_sessions(&target, &env), peer)
    })
    .await
    .unwrap();
    assert!(
        result
            .unwrap_err()
            .message()
            .contains("readiness identity invalid")
    );
    assert!(!paths.root().join("remote/client/endpoint.key").exists());
    drop(listener);
    std::fs::remove_dir_all(home).unwrap();
}

/// Missing broker discovery preserves the direct target-resolution contract.
/// The absent profile must fail before any endpoint key is created.
#[tokio::test]
async fn remote_list_missing_broker_preserves_direct_profile_lookup() {
    let (home, env, paths) = fixture();
    let error = list_iroh_host_sessions(
        &crate::cli::ControlTargetSelection::IrohProfile("missing".into()),
        &env,
    )
    .await
    .unwrap_err();
    assert_eq!(error.message(), "Iroh client profile not found");
    assert!(!paths.root().join("remote/client/endpoint.key").exists());
    std::fs::remove_dir_all(home).unwrap();
}
