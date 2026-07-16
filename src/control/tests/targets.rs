//! Control targets tests.

use super::*;

/// Verifies target parsing rejects conflicting independent selectors.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn target_parsing_rejects_conflicting_independent_selectors() {
    let (mut session, primary) = test_session();
    session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/select","params":{"target":{"pane_id":"%1","pane_index":1},"idempotency_key":"bad-target"}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""error""#));
    assert!(response.contains("multiple independent selectors"));
}

/// Verifies target parsing resolves nested session window and pane objects.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn target_parsing_resolves_nested_session_window_and_pane_objects() {
    let (mut session, primary) = test_session();
    dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"new-window"}}"#,
        &mut session,
        &primary,
    );
    dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"pane/create","params":{"target":{"window":{"session":{"default":true},"window_index":1},"active":true},"split":"vertical","select":true,"idempotency_key":"split-window-pane"}}"#,
        &mut session,
        &primary,
    );

    let rename = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"window/rename","params":{"target":{"session":{"default":true},"window_index":1},"name":"renamed","idempotency_key":"rename-nested"}}"#,
        &mut session,
        &primary,
    );
    assert!(rename.contains(r#""name":"renamed""#), "{rename}");

    let resize = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"pane/resize","params":{"target":{"window":{"window_id":"@2"},"pane_index":1},"size":{"mode":"cells","columns":33,"rows":11},"idempotency_key":"resize-nested"}}"#,
        &mut session,
        &primary,
    );
    assert!(resize.contains(r#""pane_id":"%3""#), "{resize}");
    assert!(resize.contains(r#""columns":33"#), "{resize}");
    assert!(resize.contains(r#""rows":11"#), "{resize}");
}

/// Verifies target parsing rejects unstructured target values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn target_parsing_rejects_unstructured_target_values() {
    let (mut session, primary) = test_session();
    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/select","params":{"target":"%1","idempotency_key":"string-target"}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""error""#));
    assert!(response.contains("PaneTarget must be an object"));
}

/// Verifies dispatches pane move swap break and join methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_pane_move_swap_break_and_join_methods() {
    let (mut session, primary) = test_session();

    dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/create","params":{"split":"vertical","select":true,"idempotency_key":"split"}}"#,
        &mut session,
        &primary,
    );
    let first_pane = session.windows()[0].panes()[0].id.to_string();
    let second_pane = session.windows()[0].panes()[1].id.to_string();

    let swap = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"pane/swap","params":{{"source":{{"pane_id":"{}"}},"destination":{{"pane_id":"{}"}},"idempotency_key":"swap"}}}}"#,
        json_escape(&first_pane),
        json_escape(&second_pane)
    );
    let swap_response = dispatch_control_request(&swap, &mut session, &primary);
    assert!(swap_response.contains(r#""layout""#));
    assert_eq!(session.windows()[0].panes()[0].id.to_string(), second_pane);

    let break_request = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"pane/break","params":{{"target":{{"pane_id":"{}"}},"name":"moved","idempotency_key":"break"}}}}"#,
        json_escape(&second_pane)
    );
    let break_response = dispatch_control_request(&break_request, &mut session, &primary);
    assert!(break_response.contains(r#""window""#));
    assert_eq!(session.windows().len(), 2);

    let destination_window = session.windows()[0].id.to_string();
    let join_request = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"pane/join","params":{{"source":{{"pane_id":"{}"}},"destination":{{"window_id":"{}"}},"position":"vertical","idempotency_key":"join"}}}}"#,
        json_escape(&second_pane),
        json_escape(&destination_window)
    );
    let join_response = dispatch_control_request(&join_request, &mut session, &primary);
    assert!(join_response.contains(r#""pane_id""#));
    assert_eq!(session.windows().len(), 1);

    let moved_pane = session.windows()[0].panes()[0].id.to_string();
    let target_pane = session.windows()[0].panes()[1].id.to_string();
    let move_request = format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"pane/move","params":{{"source":{{"pane_id":"{}"}},"destination":{{"pane_id":"{}"}},"position":"horizontal","idempotency_key":"move"}}}}"#,
        json_escape(&moved_pane),
        json_escape(&target_pane)
    );
    let move_response = dispatch_control_request(&move_request, &mut session, &primary);
    assert!(move_response.contains(r#""layout""#));
    assert_eq!(session.windows()[0].panes().len(), 2);
}
