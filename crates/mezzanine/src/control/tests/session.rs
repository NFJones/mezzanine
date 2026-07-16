//! Control session tests.

use super::*;

/// Verifies dispatches session rename and kill methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_session_rename_and_kill_methods() {
    let (mut session, primary) = test_session();

    let rename_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/rename","params":{"name":"work","idempotency_key":"rename"}}"#,
        &mut session,
        &primary,
    );
    assert!(rename_response.contains(r#""renamed":true"#));
    assert_eq!(session.name, "work");

    let kill_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/kill","params":{"force":true,"idempotency_key":"kill"}}"#,
        &mut session,
        &primary,
    );
    assert!(kill_response.contains(r#""killed":true"#));
    assert!(session.windows().is_empty());
}

/// Verifies mutating control methods require idempotency keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mutating_control_methods_require_idempotency_keys() {
    let (mut session, primary) = test_session();

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work"}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""error""#));
    assert!(response.contains("idempotency_key"));
    assert_eq!(session.windows().len(), 1);
}
