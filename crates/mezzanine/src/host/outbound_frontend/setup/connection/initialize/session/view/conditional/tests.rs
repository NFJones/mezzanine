//! Exact delivered-base fencing and unchanged response projection.
use super::*;

/// Conditional requests may reuse only the delivered identity at its original
/// geometry. Unchanged replies cannot contain replacement content or receipts,
/// change correlation, or establish a base when none was requested.
#[test]
fn outbound_conditional_broker_requires_exact_delivered_base() {
    let identity = "a".repeat(64);
    let request = ViewRequest {
        handle: FrontendHandle {
            owner: "fixture".into(),
            generation: 1,
        },
        columns: 80,
        rows: 24,
        if_view_identity: Some(identity.clone()),
    };
    let base = (identity.clone(), 80, 24);
    validate_base(&request, Some(&base)).unwrap();
    assert!(validate_base(&request, None).is_err());
    assert!(validate_base(&request, Some(&(identity.clone(), 81, 24))).is_err());
    assert!(validate_base(&request, Some(&("b".repeat(64), 80, 24))).is_err());
    let response = serde_json::json!({"jsonrpc":"2.0","id":VIEW_REQUEST_ID,
        "result":{"not_modified":true,"view_identity":identity,
            "event_cutoff":7,"render_rate_limit_fps":30,"private":"discard"}});
    let session = serde_json::json!({"granted_role":"primary"});
    let projected: serde_json::Value = serde_json::from_str(
        &project_unchanged(&response.to_string(), &request, &session)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(projected["view_identity"], identity);
    assert_eq!(projected["event_cutoff"], 7);
    assert!(!projected.to_string().contains("discard"));
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/not_modified", serde_json::json!(false)),
        ("/result/view_identity", serde_json::json!("b".repeat(64))),
    ] {
        let mut invalid = response.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(project_unchanged(&invalid.to_string(), &request, &session).is_err());
    }
    for key in ["view", "presentation_ids"] {
        let mut invalid = response.clone();
        invalid["result"][key] = serde_json::Value::Null;
        assert!(project_unchanged(&invalid.to_string(), &request, &session).is_err());
    }
    let no_base = ViewRequest {
        if_view_identity: None,
        ..request
    };
    assert!(project_unchanged(&response.to_string(), &no_base, &session).is_err());
    assert!(
        project_unchanged(
            &serde_json::json!({"jsonrpc":"2.0","id":VIEW_REQUEST_ID,
        "result":{"view":null}})
            .to_string(),
            &no_base,
            &session
        )
        .unwrap()
        .is_none()
    );
}
