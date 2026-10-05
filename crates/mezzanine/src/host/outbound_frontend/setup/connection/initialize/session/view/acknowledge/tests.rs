//! Receipt ownership validation before remote presentation mutation.
use super::*;

/// Only the last delivered receipt list on this exact local stream can be
/// acknowledged. Empty, duplicate, foreign or stale requests reject before I/O.
#[test]
fn outbound_presentation_request_requires_delivered_receipts() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let original = serde_json::json!({"operation":"acknowledge","handle":handle,
        "idempotency_key":"exact","presentation_ids":[7,8]});
    let request: AcknowledgementRequest = serde_json::from_value(original.clone()).unwrap();
    validate_request(&request, &handle, &[7, 8]).unwrap();
    assert!(validate_request(&request, &handle, &[9]).is_err());
    for (pointer, value) in [
        ("/operation", serde_json::json!("step")),
        ("/handle/generation", serde_json::json!(2)),
        ("/idempotency_key", serde_json::json!("")),
        ("/presentation_ids", serde_json::json!([])),
        ("/presentation_ids", serde_json::json!([7, 7])),
        ("/presentation_ids", serde_json::json!([8, 7])),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let request: AcknowledgementRequest = serde_json::from_value(changed).unwrap();
        assert!(validate_request(&request, &handle, &[7, 8]).is_err());
    }
}

/// Correlated true and false results survive unchanged, while malformed or
/// unrelated responses cannot arm presentation and expose no peer diagnostics.
#[test]
fn outbound_presentation_remote_reply_preserves_false() {
    for acknowledged in [true, false] {
        let body = serde_json::json!({"jsonrpc":"2.0","id":ACK_ID,
            "result":{"acknowledged":acknowledged}});
        assert_eq!(
            project_acknowledgement(&body.to_string()).unwrap(),
            acknowledged
        );
    }
    for body in [
        serde_json::json!({"jsonrpc":"2.0","id":"other","result":{"acknowledged":true}}),
        serde_json::json!({"jsonrpc":"2.0","id":ACK_ID,"result":{"acknowledged":"true"}}),
        serde_json::json!({"jsonrpc":"2.0","id":ACK_ID,"error":{"message":"private-proof"}}),
    ] {
        assert!(
            !project_acknowledgement(&body.to_string())
                .unwrap_err()
                .message()
                .contains("private-proof")
        );
    }
}
