//! Closed detach replies preserve mutation and exact-client ownership.
use super::*;

/// A successful detach cannot be attributed to another handle, session, client
/// or mutation key. Extra response fields cannot smuggle private remote facts.
#[test]
fn outbound_detach_client_requires_exact_settlement() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"idempotency_key":"exact",
        "detached":true,"client_id":"c1"});
    let reply: DetachReply = serde_json::from_value(original.clone()).unwrap();
    validate_reply(&reply, &handle, &reply.session, "exact").unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/session_id", serde_json::json!("$2")),
        ("/client_id", serde_json::json!("c2")),
        ("/idempotency_key", serde_json::json!("other")),
        ("/detached", serde_json::json!(false)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let changed: DetachReply = serde_json::from_value(changed).unwrap();
        assert!(validate_reply(&changed, &handle, &reply.session, "exact").is_err());
    }
    let mut extra = original;
    extra["token"] = serde_json::json!("private");
    assert!(serde_json::from_value::<DetachReply>(extra).is_err());
}
