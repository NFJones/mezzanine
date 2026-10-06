//! Dedicated channel acquisition against disposable authenticated Unix peers.
//!
//! Synthetic readiness tests exercise closed ownership, protected discovery and
//! buffered raw transitions without remote sessions or real X credentials.

use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Supplies private control discovery and a separate dedicated listener. No
/// endpoint key, proof, terminal output or desktop state is created here.
fn fixture() -> (
    PathBuf,
    std::os::unix::net::UnixListener,
    tokio::net::UnixListener,
    X11ChannelOpener,
) {
    let root = std::env::temp_dir().join(format!("mez-xopen-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let control = std::os::unix::net::UnixListener::bind(root.join("outbound.sock")).unwrap();
    std::fs::set_permissions(
        root.join("outbound.sock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let name = "x0123456789abcdef.sock";
    let dedicated = tokio::net::UnixListener::bind(root.join(name)).unwrap();
    std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o600)).unwrap();
    let handle = FrontendHandle {
        owner: "f".repeat(32),
        generation: 1,
    };
    let session = serde_json::from_value(serde_json::json!({"selected_version":3,
        "granted_role":"primary","session_id":"$1","lease_id":"lease-one","client_id":"c1"}))
    .unwrap();
    let opener =
        X11ChannelOpener::capture(Discovery::capture(&root).unwrap(), name, handle, session, 1)
            .unwrap();
    (root, control, dedicated, opener)
}

/// Coalesced readiness and raw setup remain byte-exact. Channel lifetime holds
/// its finite permit; saturation sends no second request, and disposal releases
/// capacity without changing the control socket or acquiring an endpoint key.
#[tokio::test]
async fn outbound_x11_opener_preserves_read_ahead_and_channel_capacity() {
    let (root, control, dedicated, opener) = fixture();
    let peer = async {
        let (stream, _) = dedicated.accept().await.unwrap();
        let mut framed = Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
        let request = framed.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request.body).unwrap(),
            serde_json::json!({
            "protocol":"mez-outbound-x11/2","handle":opener.handle,"session":opener.session})
        );
        let mut raw = framed.into_inner();
        let mut bytes = crate::protocol::framing::encode_frame(&ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({"protocol":"mez-outbound-x11/2","handle":opener.handle,
                "session":opener.session,"occurrence":7,"ready":true})
            .to_string(),
        ));
        bytes.extend_from_slice(b"setup-tail");
        raw.write_all(&bytes).await.unwrap();
        raw.shutdown().await.unwrap();
        raw
    };
    let (channel, mut peer) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(opener.open(Duration::from_secs(1)), peer)
    })
    .await
    .unwrap();
    let mut channel = channel.unwrap();
    assert_eq!(channel.occurrence(), 7);
    assert_eq!(opener.slots.available_permits(), 0);
    assert_eq!(
        opener
            .open(Duration::from_secs(1))
            .await
            .err()
            .unwrap()
            .kind(),
        MezErrorKind::RateLimited
    );
    let mut bytes = Vec::new();
    channel.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"setup-tail");
    channel.write_all(b"response").await.unwrap();
    channel.shutdown().await.unwrap();
    let mut response = Vec::new();
    peer.read_to_end(&mut response).await.unwrap();
    assert_eq!(response, b"response");
    drop(channel);
    assert_eq!(opener.slots.available_permits(), 1);
    assert!(root.join("outbound.sock").exists());
    assert!(!root.join("remote/client/endpoint.key").exists());
    drop((opener, dedicated, control, peer));
    std::fs::remove_dir_all(root).unwrap();
}

/// Foreign or malformed readiness consumes only the dedicated connection and
/// releases its permit. A silent peer times out; cancellation after receiving the
/// request closes that same stream with no second request or automatic replay.
#[tokio::test]
async fn outbound_x11_opener_rejects_readiness_and_cancels_without_replay() {
    for case in [
        "handle",
        "session",
        "protocol",
        "occurrence",
        "ready",
        "extra",
        "type",
        "silent",
        "cancel",
    ] {
        let (root, control, dedicated, opener) = fixture();
        let (received, barrier) = tokio::sync::oneshot::channel();
        let peer = async {
            let (stream, _) = dedicated.accept().await.unwrap();
            let mut framed = Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
            framed.next().await.unwrap().unwrap();
            received.send(()).unwrap();
            let mut reply = serde_json::json!({"protocol":"mez-outbound-x11/2","handle":opener.handle,
                "session":opener.session,"occurrence":7,"ready":true});
            match case {
                "handle" => reply["handle"]["generation"] = serde_json::json!(2),
                "session" => reply["session"]["client_id"] = serde_json::json!("c2"),
                "protocol" => reply["protocol"] = serde_json::json!("mez-outbound-x11/1"),
                "occurrence" => reply["occurrence"] = serde_json::json!(0),
                "ready" => reply["ready"] = serde_json::json!(false),
                "extra" => reply["token"] = serde_json::json!("private-proof"),
                _ => {}
            }
            if !matches!(case, "silent" | "cancel") {
                framed
                    .send(ProtocolFrame::new(
                        if case == "type" {
                            "application/json"
                        } else {
                            CONTENT_TYPE
                        },
                        reply.to_string(),
                    ))
                    .await
                    .unwrap();
            }
            assert!(
                framed.next().await.is_none(),
                "failed opening must close without replay"
            );
        };
        let client = async {
            let mut opening = Box::pin(opener.open(Duration::from_millis(100)));
            if case == "cancel" {
                tokio::select! {
                    result = &mut opening => panic!("opening settled before cancellation: {}", result.is_ok()),
                    result = barrier => { result.unwrap(); },
                }
                assert_eq!(opener.slots.available_permits(), 0);
                drop(opening);
            } else {
                assert!(opening.await.is_err());
            }
            assert_eq!(opener.slots.available_permits(), 1);
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(peer, client);
        })
        .await
        .unwrap();
        drop((opener, dedicated, control));
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Dedicated symlink/permissive/replacement objects cannot inherit captured
/// discovery. Validation is read-only and preserves authored entries.
#[tokio::test]
async fn outbound_x11_opener_rejects_changed_dedicated_discovery() {
    for case in ["mode", "replacement", "symlink"] {
        let (root, control, dedicated, opener) = fixture();
        let path = root.join(&opener.name);
        let replacement = if case == "mode" {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            None
        } else {
            std::fs::remove_file(&path).unwrap();
            if case == "symlink" {
                symlink("outbound.sock", &path).unwrap();
                None
            } else {
                Some(tokio::net::UnixListener::bind(&path).unwrap())
            }
        };
        assert!(opener.open(Duration::from_secs(1)).await.is_err());
        assert_eq!(opener.slots.available_permits(), 1);
        assert!(std::fs::symlink_metadata(&path).is_ok());
        drop((opener, dedicated, control, replacement));
        std::fs::remove_dir_all(root).unwrap();
    }
}
