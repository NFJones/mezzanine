//! Closed kill reply ownership and mutation identity checks.
use super::*;

/// The consumed client accepts only its exact handle, original target/key and
/// host-only settlement. Correlated display facts cannot authorize another kill.
#[test]
fn outbound_kill_client_reply_preserves_mutation_identity() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"handle":handle,"host":{
        "selected_version":3,"granted_role":"observer","host_only":true},
        "target":"$1","idempotency_key":"exact","settlement":{
        "killed":true,"lease_id":"lease-one","session_id":"$1","state":"revoked"}});
    let reply: KillReply = serde_json::from_value(original.clone()).unwrap();
    validate_reply(&reply, &handle, "$1", "exact").unwrap();
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/target", serde_json::json!("$2")),
        ("/idempotency_key", serde_json::json!("other")),
        ("/host/host_only", serde_json::json!(false)),
        ("/settlement/session_id", serde_json::json!("invalid")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let reply: KillReply = serde_json::from_value(changed).unwrap();
        assert!(validate_reply(&reply, &handle, "$1", "exact").is_err());
    }
    let mut extra = original;
    extra["device_credential"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<KillReply>(extra).is_err());
}
