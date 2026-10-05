//! Host listing correlation and closed local request validation.
use super::*;

/// Health requests select only a boolean authentication-only mode, never an
/// arbitrary remote method. Omission preserves the existing listing behavior;
/// malformed modes reject before any remote follow-up is issued.
#[test]
fn outbound_host_health_request_is_closed_and_typed() {
    let handle = serde_json::json!({"owner":"fixture","generation":1});
    let request: ListRequest =
        serde_json::from_value(serde_json::json!({"handle":handle})).unwrap();
    assert!(!request.authentication_only);
    let request: ListRequest =
        serde_json::from_value(serde_json::json!({"handle":handle,"authentication_only":true}))
            .unwrap();
    assert!(request.authentication_only);
    for invalid in [
        serde_json::json!("true"),
        serde_json::Value::Null,
        serde_json::json!(1),
    ] {
        assert!(
            serde_json::from_value::<ListRequest>(
                serde_json::json!({"handle":handle,"authentication_only":invalid})
            )
            .is_err()
        );
    }
}

/// Only correlated read-only results project summaries; peer diagnostics remain
/// redacted and local callers cannot supply arbitrary methods or targets.
#[test]
fn outbound_host_list_reply_requires_correlation() {
    let original = serde_json::json!({"jsonrpc":"2.0","id":LIST_ID,"result":{"sessions":[]}});
    assert!(project_response(&original.to_string()).unwrap().is_empty());
    for body in [
        serde_json::json!({"jsonrpc":"2.0","id":"other","result":{"sessions":[]}}),
        serde_json::json!({"jsonrpc":"2.0","id":LIST_ID,"error":{"message":"private-proof"}}),
        serde_json::json!({"jsonrpc":"2.0","id":LIST_ID,"result":{"sessions":null}}),
    ] {
        assert!(
            !project_response(&body.to_string())
                .unwrap_err()
                .message()
                .contains("private-proof")
        );
    }
    assert!(
        serde_json::from_value::<ListRequest>(serde_json::json!({
            "handle":{"owner":"fixture","generation":1},"method":"host/session/kill"
        }))
        .is_err()
    );
}
