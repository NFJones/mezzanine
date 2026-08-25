//! Control clients tests.

use super::*;

/// Verifies generic client methods manage immediately attached observers and
/// legacy observer request methods remain absent.
#[test]
fn dispatches_client_and_observer_methods() {
    let (mut session, primary) = test_session();
    let observer_client = session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();

    let list_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"client/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list_response.contains(r#""role":"primary""#));
    assert!(list_response.contains(r#""role":"observer""#));
    assert!(!list_response.contains("pending_observer"));
    assert!(list_response.contains(&format!(r#""id":"{}""#, primary)));
    assert!(list_response.contains(&format!(r#""id":"{}""#, observer_client)));
    assert!(list_response.contains(r#""version":2"#));
    assert!(list_response.contains(r#""requested_role":"primary""#));
    assert!(list_response.contains(r#""requested_role":"observer""#));
    assert!(list_response.contains(r#""attached_at":""#));
    assert!(list_response.contains(r#""last_seen_at":""#));
    assert!(list_response.contains(r#""descriptor":{"name":"primary""#));
    assert!(
        list_response.contains(
            r#""descriptor":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}"#
        ),
        "{list_response}"
    );
    assert!(
        list_response.contains(r#""terminal_size":{"columns":80,"rows":24}"#),
        "{list_response}"
    );

    for (id, method) in [
        (2, "observer/inspect"),
        (3, "observer/approve"),
        (4, "observer/reject"),
        (5, "observer/revoke"),
    ] {
        let response = dispatch_control_request(
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{}}}}"#),
            &mut session,
            &primary,
        );
        assert!(
            response.contains(r#""mezzanine_code":"method_not_found""#),
            "{response}"
        );
    }

    let detach_response = dispatch_control_request(
        &format!(
            r#"{{"jsonrpc":"2.0","id":6,"method":"client/detach","params":{{"client_id":"{}","idempotency_key":"detach-observer"}}}}"#,
            observer_client
        ),
        &mut session,
        &primary,
    );
    assert!(
        detach_response.contains(r#""detached":true"#),
        "{detach_response}"
    );
}

/// Verifies dispatches layout-owner transfer atomically.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_layout_owner_transfer_atomically() {
    let (mut session, first) = test_session();
    let second = session.attach_primary("second", true).unwrap();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"client/set_layout_owner","params":{{"client_id":"{}","idempotency_key":"set-layout-owner"}}}}"#,
        second
    );

    let response = dispatch_control_request(&request, &mut session, &first);

    assert!(response.contains(&format!(r#""layout_owner_client_id":"{}""#, second)));
    assert_eq!(session.layout_owner_client_id(), Some(&second));
    assert_eq!(session.attached_primaries().count(), 2);
}

/// Verifies an attached observer remains read-only outside its view and event
/// capabilities, and cannot call the removed request inspection method.
#[test]
fn observer_cannot_receive_primary_session_or_mcp_data() {
    let (mut session, _primary) = test_session();
    let observer_client = session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();

    let session_response = dispatch_control_request_for_client(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{}}"#,
        &mut session,
        &observer_client,
        None,
    );
    assert!(session_response.contains(r#""mezzanine_code":"forbidden""#));
    assert!(!session_response.contains(r#""session_id""#));

    let mcp_response = dispatch_control_request_for_client(
        r#"{"jsonrpc":"2.0","id":2,"method":"mcp/list","params":{}}"#,
        &mut session,
        &observer_client,
        Some(&McpRegistry::default()),
    );
    assert!(mcp_response.contains(r#""mezzanine_code":"forbidden""#));
    assert!(!mcp_response.contains(r#""servers""#));
}
