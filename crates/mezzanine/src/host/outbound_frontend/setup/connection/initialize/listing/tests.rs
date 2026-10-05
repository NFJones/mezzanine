//! Host listing correlation and closed local request validation.
use super::*;

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
