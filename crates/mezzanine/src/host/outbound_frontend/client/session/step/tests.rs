//! Exact client mutation acknowledgement checks, without terminal effects.
//!
//! An accepted byte count cannot replace stream/session/key correlation. Unknown
//! metadata rejects before exposure, and lifecycle flags remain truthful.

use super::*;

/// All immutable identity fields must match the retained mutation owner, while
/// reported detach/termination flags are not inferred from input acceptance.
#[test]
fn outbound_input_client_reply_requires_exact_mutation_owner() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"idempotency_key":"exact",
        "acknowledgement":{"input_bytes":2,"client_detached":true,"session_terminated":false}});
    let reply: InputReply = serde_json::from_value(original.clone()).unwrap();
    validate_reply(&reply, &handle, &reply.session, "exact", 2).unwrap();
    assert!(reply.acknowledgement.client_detached);
    assert!(!reply.acknowledgement.view_refresh_required);
    assert!(!reply.acknowledgement.full_redraw_required);
    let mut redraw = original.clone();
    redraw["acknowledgement"]["view_refresh_required"] = serde_json::json!(true);
    redraw["acknowledgement"]["full_redraw_required"] = serde_json::json!(true);
    let valid: InputReply = serde_json::from_value(redraw.clone()).unwrap();
    validate_reply(&valid, &handle, &reply.session, "exact", 2).unwrap();
    redraw["acknowledgement"]["view_refresh_required"] = serde_json::json!(false);
    let invalid: InputReply = serde_json::from_value(redraw).unwrap();
    assert!(validate_reply(&invalid, &handle, &reply.session, "exact", 2).is_err());
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/session_id", serde_json::json!("$2")),
        ("/session/client_id", serde_json::json!("c2")),
        ("/idempotency_key", serde_json::json!("other")),
        ("/acknowledgement/input_bytes", serde_json::json!(1)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let changed: InputReply = serde_json::from_value(changed).unwrap();
        assert!(validate_reply(&changed, &handle, &reply.session, "exact", 2).is_err());
    }
    let mut extra = original;
    extra["device_credential"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<InputReply>(extra).is_err());
}
