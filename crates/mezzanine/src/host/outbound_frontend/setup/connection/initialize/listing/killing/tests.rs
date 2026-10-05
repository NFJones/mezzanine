//! Closed force-kill request validation and redacted response correlation.
use super::*;

/// Foreign handles, malformed mutation identity and arbitrary methods reject
/// before remote writes; wrong replies cannot become successful settlement.
#[test]
fn outbound_kill_owner_validates_request_and_correlation() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"operation":"kill","handle":handle,
        "target":"$1","idempotency_key":"exact"});
    let request: KillRequest = serde_json::from_value(original.clone()).unwrap();
    validate_local_request(&request, &handle).unwrap();
    for (pointer, value) in [
        ("/operation", serde_json::json!("execute")),
        ("/handle/generation", serde_json::json!(2)),
        ("/target", serde_json::json!("")),
        ("/idempotency_key", serde_json::json!("")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let request: KillRequest = serde_json::from_value(changed).unwrap();
        assert!(validate_local_request(&request, &handle).is_err());
    }
    let valid = serde_json::json!({"jsonrpc":"2.0","id":KILL_ID,"result":{
        "killed":true,"lease_id":"lease-one","session_id":"$1","state":"revoked"}});
    project_response(&valid.to_string(), "$1").unwrap();
    for body in [
        serde_json::json!({"jsonrpc":"2.0","id":"other","result":valid["result"]}),
        serde_json::json!({"jsonrpc":"2.0","id":KILL_ID,"error":{"message":"private-proof"}}),
    ] {
        assert!(
            !project_response(&body.to_string(), "$1")
                .unwrap_err()
                .message()
                .contains("private-proof")
        );
    }
}
