//! Target detach requires existing administrative-primary session authority.
use super::*;

/// Explicit target syntax cannot replace the initialized session or method;
/// observer authority, stale handles and malformed settlement reject.
#[test]
fn outbound_target_detach_requires_primary_and_correlated_target() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let base = serde_json::json!({"operation":"detach-target","handle":handle,"client_id":"c2","idempotency_key":"original"});
    let request: Request = serde_json::from_value(base.clone()).unwrap();
    validate_request(
        &request,
        &handle,
        &serde_json::json!({"granted_role":"primary"}),
    )
    .unwrap();
    assert!(
        validate_request(
            &request,
            &handle,
            &serde_json::json!({"granted_role":"observer"})
        )
        .is_err()
    );
    for (pointer, value) in [
        ("/operation", serde_json::json!("step")),
        ("/handle/generation", serde_json::json!(2)),
        ("/client_id", serde_json::json!("bad")),
        ("/idempotency_key", serde_json::json!("")),
    ] {
        let mut invalid = base.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(
            validate_request(
                &serde_json::from_value(invalid).unwrap(),
                &handle,
                &serde_json::json!({"granted_role":"primary"})
            )
            .is_err()
        );
    }
    let mut extra = base;
    extra["session_id"] = serde_json::json!("$2");
    assert!(serde_json::from_value::<Request>(extra).is_err());
    let base = serde_json::json!({"jsonrpc":"2.0","id":REQUEST_ID,"result":{"detached":true,"client_id":"c2"}});
    project_response(&base.to_string(), "c2").unwrap();
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/client_id", serde_json::json!("c1")),
        ("/result/detached", serde_json::json!(false)),
    ] {
        let mut invalid = base.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(project_response(&invalid.to_string(), "c2").is_err());
    }
}
