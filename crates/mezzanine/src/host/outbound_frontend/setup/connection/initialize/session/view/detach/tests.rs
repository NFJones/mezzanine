//! Self-detach request bounds and correlated exact-client runtime evidence.
use super::*;

/// Caller-controlled targeting is forbidden. Foreign handles, observer owners,
/// invalid keys and malformed settlement reject without inventing success.
#[test]
fn outbound_detach_owner_is_primary_and_target_free() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let summary = serde_json::json!({"granted_role":"primary","client_id":"c1"});
    let original =
        serde_json::json!({"operation":"detach","handle":handle,"idempotency_key":"exact"});
    let request: DetachRequest = serde_json::from_value(original.clone()).unwrap();
    validate_request(&request, &handle, &summary).unwrap();
    assert!(
        validate_request(
            &request,
            &handle,
            &serde_json::json!({"granted_role":"observer"})
        )
        .is_err()
    );
    for (pointer, value) in [
        ("/handle/generation", serde_json::json!(2)),
        ("/operation", serde_json::json!("step")),
        ("/idempotency_key", serde_json::json!("")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let request: DetachRequest = serde_json::from_value(changed).unwrap();
        assert!(validate_request(&request, &handle, &summary).is_err());
    }
    let mut targeting = original;
    targeting["client_id"] = serde_json::json!("c2");
    assert!(serde_json::from_value::<DetachRequest>(targeting).is_err());
    let valid = serde_json::json!({"jsonrpc":"2.0","id":DETACH_ID,"result":{"detached":true,"client_id":"c1"}});
    project_response(&valid.to_string(), &summary).unwrap();
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/client_id", serde_json::json!("c2")),
        ("/result/detached", serde_json::json!(false)),
    ] {
        let mut changed = valid.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(project_response(&changed.to_string(), &summary).is_err());
    }
}
