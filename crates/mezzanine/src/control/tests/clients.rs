//! Control clients tests.

use super::*;

/// Verifies dispatches client and observer methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_client_and_observer_methods() {
    let (mut session, primary) = test_session();
    let (observer_client, observer_request) = session.request_observer("observer");

    let list_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"client/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list_response.contains(r#""role":"primary""#));
    assert!(list_response.contains(r#""role":"pending_observer""#));
    assert!(list_response.contains(&format!(r#""id":"{}""#, primary)));
    assert!(list_response.contains(r#""version":1"#));
    assert!(list_response.contains(r#""requested_role":"primary""#));
    assert!(list_response.contains(r#""requested_role":"observer""#));
    assert!(
        !list_response.contains(r#""requested_role":"pending_observer""#),
        "{list_response}"
    );
    assert!(list_response.contains(r#""attached_at":""#));
    assert!(list_response.contains(r#""last_seen_at":""#));
    assert!(list_response.contains(r#""attached_at":null"#));
    assert!(list_response.contains(r#""last_seen_at":null"#));
    assert!(list_response.contains(r#""descriptor":{"name":"primary""#));
    assert!(
        list_response.contains(
            r#""descriptor":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"screen-256color"}}"#
        ),
        "{list_response}"
    );
    assert!(
        list_response.contains(r#""terminal_size":{"columns":80,"rows":24}"#),
        "{list_response}"
    );
    assert!(list_response.contains(r#""terminal_size":null"#));

    let inspect_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"observer/inspect","params":{{"observer_request_id":"{}"}}}}"#,
        observer_request
    );
    let inspect_response = dispatch_control_request(&inspect_request, &mut session, &primary);
    assert!(inspect_response.contains(r#""state":"pending""#));
    assert!(inspect_response.contains(&format!(r#""id":"{}""#, observer_request)));
    assert!(inspect_response.contains(r#""version":1"#));
    assert!(inspect_response.contains(r#""requested_at":""#));
    assert!(inspect_response.contains(r#""decided_at":null"#));
    assert!(inspect_response.contains(r#""decided_by_client_id":null"#));
    assert!(inspect_response.contains(r#""visible_from_time":null"#));
    assert!(inspect_response.contains(r#""descriptor":{"name":"observer","interactive":false"#));
    assert!(inspect_response.contains(r#""reason":null"#));

    let approve_request = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"observer/approve","params":{{"observer_request_id":"{}","idempotency_key":"approve"}}}}"#,
        observer_request
    );
    let approve_response = dispatch_control_request(&approve_request, &mut session, &primary);
    assert!(approve_response.contains(r#""state":"approved""#));
    assert!(approve_response.contains(r#""requested_at":""#));
    assert!(approve_response.contains(r#""decided_at":""#));
    assert!(approve_response.contains(&format!(r#""decided_by_client_id":"{}""#, primary)));
    assert!(approve_response.contains(r#""visible_from_time":""#));
    assert!(!approve_response.contains(r#""visible_from_time":"event:"#));

    let revoke_request = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"observer/revoke","params":{{"client_id":"{}","idempotency_key":"revoke"}}}}"#,
        observer_client
    );
    let revoke_response = dispatch_control_request(&revoke_request, &mut session, &primary);
    assert!(revoke_response.contains(r#""revoked":true"#));

    let (_rejected_client, rejected_request) = session.request_observer("rejectee");
    let reject_request = format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"observer/reject","params":{{"observer_request_id":"{}","reason":"not today","idempotency_key":"reject"}}}}"#,
        rejected_request
    );
    let reject_response = dispatch_control_request(&reject_request, &mut session, &primary);
    assert!(reject_response.contains(r#""state":"rejected""#));
    assert!(reject_response.contains(r#""reason":"not today""#));
}

/// Verifies dispatches primary client selection atomically.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_primary_client_selection_atomically() {
    let (mut session, first) = test_session();
    session.detach_primary(&first).unwrap();
    let second = session.attach_primary("second", true).unwrap();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"client/select_primary","params":{{"client_id":"{}","idempotency_key":"select-primary"}}}}"#,
        first
    );

    let response = dispatch_control_request(&request, &mut session, &second);

    assert!(response.contains(&format!(r#""primary_client_id":"{}""#, first)));
    assert_eq!(session.primary_client_id(), Some(&first));
    assert_eq!(
        session
            .clients()
            .iter()
            .filter(|client| client.role == mez_mux::session::ClientRole::Primary)
            .count(),
        1
    );
}

/// Verifies pending observer cannot receive session or mcp data.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pending_observer_cannot_receive_session_or_mcp_data() {
    let (mut session, _primary) = test_session();
    let (observer_client, observer_request) = session.request_observer("observer");

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

    let terminal_response = dispatch_control_request_for_client(
        r#"{"jsonrpc":"2.0","id":4,"method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        &mut session,
        &observer_client,
        None,
    );
    assert!(terminal_response.contains(r#""mezzanine_code":"forbidden""#));
    assert!(!terminal_response.contains(r#""view""#));

    let inspect_request = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"observer/inspect","params":{{"observer_request_id":"{}"}}}}"#,
        observer_request
    );
    let inspect_response =
        dispatch_control_request_for_client(&inspect_request, &mut session, &observer_client, None);
    assert!(inspect_response.contains(r#""state":"pending""#));
    assert!(!inspect_response.contains(r#""windows""#));
}
