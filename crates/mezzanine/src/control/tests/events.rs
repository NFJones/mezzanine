//! Control events tests.

use super::*;

/// Verifies event list uses role visibility policy.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn event_list_uses_role_visibility_policy() {
    let (mut session, primary) = test_session();
    let (observer_client, observer_request) = session.request_observer("observer");
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::PaneChanged,
        Some(session.id.to_string()),
        EventVisibility::SessionView,
        "before",
    )
    .unwrap();
    log.append(
        EventKind::ObserverRequested,
        Some(session.id.to_string()),
        EventVisibility::PendingObserverRequest(observer_request.to_string()),
        "{\"state\":\"pending\"}",
    )
    .unwrap();

    let pending_response = dispatch_control_request_for_client_with_events(
        r#"{"jsonrpc":"2.0","id":1,"method":"event/list","params":{}}"#,
        &mut session,
        &observer_client,
        None,
        &log,
    );
    assert!(pending_response.contains(r#""mezzanine_code":"forbidden""#));
    assert!(pending_response.contains("pending observer clients are not authorized"));
    assert!(!pending_response.contains("before"));
    assert!(!pending_response.contains("observer_requested"));

    session
        .approve_observer_target(&primary, observer_request.as_str())
        .unwrap();
    log.append(
        EventKind::PaneChanged,
        Some(session.id.to_string()),
        EventVisibility::SessionView,
        "after",
    )
    .unwrap();
    let observer_response = dispatch_control_request_for_client_with_events(
        r#"{"jsonrpc":"2.0","id":2,"method":"event/list","params":{}}"#,
        &mut session,
        &observer_client,
        None,
        &log,
    );
    assert!(observer_response.contains("after"));
    assert!(!observer_response.contains("before"));

    let primary_response = dispatch_control_request_for_client_with_events(
        r#"{"jsonrpc":"2.0","id":3,"method":"event/list","params":{}}"#,
        &mut session,
        &primary,
        None,
        &log,
    );
    assert!(primary_response.contains("before"));
    assert!(primary_response.contains("after"));
}

/// Verifies event list honors cursor limit and retention metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn event_list_honors_cursor_limit_and_retention_metadata() {
    let (mut session, primary) = test_session();
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::PaneChanged,
        Some(session.id.to_string()),
        EventVisibility::SessionView,
        "first",
    )
    .unwrap();
    let second_id = log
        .append(
            EventKind::PaneChanged,
            Some(session.id.to_string()),
            EventVisibility::SessionView,
            "second",
        )
        .unwrap();
    log.append(
        EventKind::PaneChanged,
        Some(session.id.to_string()),
        EventVisibility::SessionView,
        "third",
    )
    .unwrap();

    let response = dispatch_control_request_for_client_with_events(
        &format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"event/list","params":{{"after_event_id":{},"limit":1}}}}"#,
            second_id.saturating_sub(1)
        ),
        &mut session,
        &primary,
        None,
        &log,
    );

    assert!(response.contains("second"), "{response}");
    assert!(response.contains(r#""time":""#), "{response}");
    assert!(!response.contains(r#""time":"event:"#), "{response}");
    assert!(
        response.contains(r#""event_type":"pane_changed""#),
        "{response}"
    );
    assert!(
        response.contains(r#""object":{"content":"second"}"#),
        "{response}"
    );
    assert!(!response.contains("first"), "{response}");
    assert!(!response.contains("third"), "{response}");
    assert!(response.contains(r#""latest_event_id":3"#), "{response}");
    assert!(
        response.contains(r#""retained_from_event_id":1"#),
        "{response}"
    );
    assert!(response.contains(r#""replay_retention":10"#), "{response}");
    assert!(response.contains(r#""truncated":true"#), "{response}");

    let invalid = dispatch_control_request_for_client_with_events(
        r#"{"jsonrpc":"2.0","id":2,"method":"event/list","params":{"limit":1001}}"#,
        &mut session,
        &primary,
        None,
        &log,
    );
    assert!(invalid.contains(r#""error""#), "{invalid}");
    assert!(
        invalid.contains("event/list limit must be at most"),
        "{invalid}"
    );

    let unknown = dispatch_control_request_for_client_with_events(
        r#"{"jsonrpc":"2.0","id":3,"method":"event/list","params":{"after_event_id":1,"unexpected":true}}"#,
        &mut session,
        &primary,
        None,
        &log,
    );
    assert!(unknown.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(unknown.contains("unknown field"));

    let extension = dispatch_control_request_for_client_with_events(
        r#"{"jsonrpc":"2.0","id":4,"method":"event/list","params":{"after_event_id":1,"extensions":{"vendor":true}}}"#,
        &mut session,
        &primary,
        None,
        &log,
    );
    assert!(extension.contains(r#""events""#), "{extension}");
}

/// Verifies generic control dispatches event list with empty replay state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_dispatches_event_list_with_empty_replay_state() {
    let (mut session, primary) = test_session();

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"event/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(response.contains(r#""events":[]"#), "{response}");
    assert!(response.contains(r#""latest_event_id":0"#), "{response}");
    assert!(
        response.contains(r#""retained_from_event_id":null"#),
        "{response}"
    );
    assert!(
        response.contains(r#""replay_retention":1000"#),
        "{response}"
    );
    assert!(response.contains(r#""truncated":false"#), "{response}");

    let invalid = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"event/list","params":{"unexpected":true}}"#,
        &mut session,
        &primary,
    );
    assert!(invalid.contains(r#""error""#), "{invalid}");
    assert!(
        invalid.contains("event/list params contains unknown field"),
        "{invalid}"
    );
}
