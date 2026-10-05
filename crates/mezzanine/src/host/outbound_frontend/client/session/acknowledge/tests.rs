//! Exact local receipt settlement checks; no output commitment is inferred.
use super::*;

/// Local replies must retain stream, session, key and receipt occurrence identity.
/// A truthful false result is valid evidence, not permission to invent an ACK.
#[test]
fn outbound_presentation_client_reply_retains_exact_receipt_owner() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"session":{
        "selected_version":3,"granted_role":"primary","session_id":"$1",
        "lease_id":"lease-one","client_id":"c1"},"idempotency_key":"exact",
        "presentation_ids":[7,8],"acknowledged":false});
    let reply: AcknowledgementReply = serde_json::from_value(original.clone()).unwrap();
    validate_reply(&reply, &handle, &reply.session, "exact", &[7, 8]).unwrap();
    assert!(!reply.acknowledged);
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/session/client_id", serde_json::json!("c2")),
        ("/idempotency_key", serde_json::json!("other")),
        ("/presentation_ids", serde_json::json!([8, 7])),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let changed: AcknowledgementReply = serde_json::from_value(changed).unwrap();
        assert!(validate_reply(&changed, &handle, &reply.session, "exact", &[7, 8]).is_err());
    }
    let mut extra = original;
    extra["device_credential"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<AcknowledgementReply>(extra).is_err());
}
