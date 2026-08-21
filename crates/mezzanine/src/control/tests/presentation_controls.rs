//! Control coverage for explicit pane and window presentation mutations.

use super::*;

/// Verifies explicit pane and window controls preserve focus and validate state.
///
/// Harness controls target background panes and windows directly, so rename,
/// zoom, layout, rebalance, and synchronized input must not synthesize focus
/// changes. Boolean controls also reject malformed desired state.
#[test]
fn explicit_pane_and_window_controls_preserve_focus_and_validate_state() {
    let (mut session, primary) = test_session();
    let focused_pane = session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let rename = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/rename","params":{"target":{"pane_id":"%1"},"name":"background","idempotency_key":"rename-pane"}}"#,
        &mut session,
        &primary,
    );
    assert!(rename.contains(r#""title":"background""#), "{rename}");
    assert_eq!(
        session.active_window().unwrap().active_pane().id,
        focused_pane
    );

    let zoom = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"pane/zoom","params":{"target":{"pane_id":"%1"},"zoomed":true,"idempotency_key":"zoom-pane"}}"#,
        &mut session,
        &primary,
    );
    assert!(zoom.contains(r#""pane_id":"%1","zoomed":true"#), "{zoom}");
    assert_eq!(
        session.active_window().unwrap().active_pane().id,
        focused_pane
    );
    assert_eq!(
        session
            .active_window()
            .unwrap()
            .zoomed_pane_id()
            .map(|id| id.as_str()),
        Some("%1")
    );

    let layout = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"window/layout","params":{"target":{"window_id":"@1"},"layout":"even-horizontal","idempotency_key":"layout-window"}}"#,
        &mut session,
        &primary,
    );
    assert!(layout.contains(r#""layout":"even-horizontal""#), "{layout}");

    let rebalance = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"window/rebalance","params":{"target":{"window_id":"@1"},"idempotency_key":"rebalance-window"}}"#,
        &mut session,
        &primary,
    );
    assert!(rebalance.contains(r#""window_id":"@1""#), "{rebalance}");

    let synchronize = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":5,"method":"pane/input-sync","params":{"target":{"window_id":"@1"},"enabled":true,"idempotency_key":"sync-window"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        synchronize.contains(r#""window_id":"@1","enabled":true"#),
        "{synchronize}"
    );
    assert!(session.window_panes_synchronized(Some("@1")).unwrap());
    assert_eq!(
        session.active_window().unwrap().active_pane().id,
        focused_pane
    );

    let invalid = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":6,"method":"pane/zoom","params":{"zoomed":"yes","idempotency_key":"invalid-zoom"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid}"
    );
}
