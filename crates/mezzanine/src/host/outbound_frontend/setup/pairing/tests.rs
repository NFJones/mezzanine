//! Closed pairing input and credential-bearing settlement stay owner-scoped.
use super::*;

mod preparation;
mod real_host;

/// Foreign local handles and raw credential fields cannot select pairing work.
/// A correlated host reply must prove host-only scope and the pinned endpoint;
/// invalid responses never include submitted/issued proof in diagnostics.
#[test]
fn outbound_pairing_validates_closed_request_and_pinned_settlement() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"operation":"pair","handle":handle,"path":"/fixture/invitation","save_as":"alias"});
    let request: PairRequest = serde_json::from_value(original.clone()).unwrap();
    validate_request(&request, &handle).unwrap();
    for (pointer, value) in [
        ("/operation", serde_json::json!("setup")),
        ("/handle/generation", serde_json::json!(2)),
        ("/path", serde_json::json!("relative")),
        ("/save_as", serde_json::json!("")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(validate_request(&serde_json::from_value(changed).unwrap(), &handle).is_err());
    }
    let mut extra = original;
    extra["token"] = serde_json::json!("private-proof");
    assert!(serde_json::from_value::<PairRequest>(extra).is_err());
    let server = iroh::SecretKey::generate().public();
    let original = serde_json::json!({"jsonrpc":"2.0","id":PAIR_ID,"result":{
        "selected_version":3,"granted_role":"observer","host":{"endpoint_id":server.to_string()},
        "session":null,"lease":null,"client":null,"capabilities":{"features":{"host_only":true}},"device_credential":"private-proof"
    }});
    assert!(validate_response(&original.to_string(), server).is_ok());
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/selected_version", serde_json::json!(2)),
        ("/result/granted_role", serde_json::json!("primary")),
        ("/result/session", serde_json::json!({"id":"$1"})),
        (
            "/result/host/endpoint_id",
            serde_json::json!(iroh::SecretKey::generate().public().to_string()),
        ),
        ("/result/device_credential", serde_json::json!("")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let error = validate_response(&changed.to_string(), server)
            .err()
            .unwrap();
        assert!(!error.message().contains("private-proof"));
    }
}
