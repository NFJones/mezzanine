//! Closed pairing publication evidence binds the admitted frontend and alias.
use super::*;

/// An absolute Unix path can contain bytes JSON cannot represent. Reject it
/// before request construction without lossy conversion, panic or wire traffic;
/// consuming the client closes this stream rather than authorizing fallback.
#[tokio::test]
async fn outbound_pairing_client_non_utf8_path_rejects_without_request() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("mez-pair-path-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(root.join("outbound.sock")).unwrap();
    std::fs::set_permissions(
        root.join("outbound.sock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let (stream, mut peer) = tokio::net::UnixStream::pair().unwrap();
    let client = OutboundFrontendClient {
        stream: Framed::new(stream, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap()),
        handle: FrontendHandle {
            owner: "fixture".into(),
            generation: 1,
        },
        discovery: Discovery::capture(&root).unwrap(),
    };
    let path = PathBuf::from(std::ffi::OsString::from_vec(
        b"/fixture/invalid-\xff/invitation.json".to_vec(),
    ));
    let error = client
        .pair_invitation(&path, None, "alias", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), MezErrorKind::InvalidArgs);
    assert!(!error.message().contains("invalid-"));
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::io::AsyncReadExt::read_to_end(&mut peer, &mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        bytes.is_empty(),
        "unrepresentable paths must send no setup bytes"
    );
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// Foreign handles, false publication and changed aliases cannot report success.
/// Credential/authority fields reject rather than entering local output.
#[test]
fn outbound_pairing_client_reply_is_closed_and_owner_scoped() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"paired":true,"profile":"alias"});
    let reply: PairReply = serde_json::from_value(original.clone()).unwrap();
    validate_reply(&reply, &handle, "alias").unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/paired", serde_json::json!(false)),
        ("/profile", serde_json::json!("other")),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let reply: PairReply = serde_json::from_value(invalid).unwrap();
        assert!(validate_reply(&reply, &handle, "alias").is_err());
    }
    let mut extra = original;
    extra["device_credential"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<PairReply>(extra).is_err());
}
