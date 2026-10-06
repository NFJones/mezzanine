//! Publication spelling never replaces exact retained session ownership.
use super::*;

/// Valid names and absence preserve the exact owner; foreign identities,
/// unsupported versions, directory paths and credential fields reject. Observer
/// settlement may report absence but cannot borrow primary X11 publication.
#[test]
fn outbound_x11_discovery_client_requires_closed_exact_owner() {
    let handle = FrontendHandle {
        owner: "f".repeat(32),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"version":1,
        "socket_name":"x0123456789abcdef.sock"});
    let reply: Reply = serde_json::from_value(original.clone()).unwrap();
    let summary = reply.session.clone();
    validate_reply(&reply, &handle, &summary).unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/client_id", serde_json::json!("c2")),
        ("/version", serde_json::json!(2)),
        ("/socket_name", serde_json::json!("../outbound.sock")),
        ("/socket_name", serde_json::json!("/private/path.sock")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            validate_reply(&serde_json::from_value(changed).unwrap(), &handle, &summary).is_err()
        );
    }
    let mut absent = original.clone();
    absent["socket_name"] = serde_json::Value::Null;
    validate_reply(
        &serde_json::from_value(absent.clone()).unwrap(),
        &handle,
        &summary,
    )
    .unwrap();
    absent["session"]["granted_role"] = serde_json::json!("observer");
    let observer: Reply = serde_json::from_value(absent.clone()).unwrap();
    validate_reply(&observer, &handle, &observer.session).unwrap();
    absent["socket_name"] = original["socket_name"].clone();
    assert!(
        validate_reply(
            &serde_json::from_value(absent).unwrap(),
            &handle,
            &observer.session
        )
        .is_err()
    );
    let mut secret = original;
    secret["route_token"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<Reply>(secret).is_err());
}
