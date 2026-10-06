//! Administrative detach replies cannot borrow another session or target result.
use super::*;

/// Exact caller ownership, target and original key must all match. Unknown
/// fields reject rather than exposing remote proof or inventing success.
#[test]
fn outbound_target_detach_client_validates_exact_owner_and_target() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let base = serde_json::json!({"handle":handle,"session":{"selected_version":3,
        "granted_role":"primary","session_id":"$1","lease_id":"lease-one","client_id":"c1"},
        "idempotency_key":"original","detached":true,"client_id":"c2"});
    let reply: Reply = serde_json::from_value(base.clone()).unwrap();
    validate_reply(&reply, &handle, &reply.session, "c2", "original").unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/session_id", serde_json::json!("$2")),
        ("/client_id", serde_json::json!("c1")),
        ("/idempotency_key", serde_json::json!("other")),
        ("/detached", serde_json::json!(false)),
    ] {
        let mut invalid = base.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let invalid: Reply = serde_json::from_value(invalid).unwrap();
        assert!(validate_reply(&invalid, &handle, &reply.session, "c2", "original").is_err());
    }
    let mut extra = base;
    extra["token"] = serde_json::json!("private");
    assert!(serde_json::from_value::<Reply>(extra).is_err());
}
