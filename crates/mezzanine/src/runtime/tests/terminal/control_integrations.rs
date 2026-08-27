//! Runtime regressions for external harness control integrations.

use super::*;

/// Verifies pane statuses are source-isolated, bounded, and focus preserving.
///
/// Clearing one source must reveal the previous source rather than removing
/// unrelated harness state, and malformed semantic states must be rejected.
#[test]
fn runtime_pane_status_is_source_isolated_and_focus_preserving() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let background_pane = service.active_pane_id().unwrap();
    let focused_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let running = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"status-running","method":"pane/status","params":{{"target":{{"pane_id":"{background_pane}"}},"source":"hooks","state":"running","text":"Building","idempotency_key":"status-running"}}}}"#
        ),
        &primary,
    );
    assert!(
        running.contains(r#""state":"running","text":"Building""#),
        "{running}"
    );
    assert_eq!(service.active_pane_id().unwrap(), focused_pane.to_string());

    let failed = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"status-failed","method":"pane/status","params":{{"target":{{"pane_id":"{background_pane}"}},"source":"ci","state":"failed","text":"Tests failed","idempotency_key":"status-failed"}}}}"#
        ),
        &primary,
    );
    assert!(failed.contains(r#""state":"failed""#), "{failed}");
    let failed_context = service.terminal_frame_context();
    let failed_status = failed_context.panes.get(&background_pane).unwrap();
    assert_eq!(failed_status.pane_status_state.as_deref(), Some("failed"));
    assert_eq!(
        failed_status.pane_status_text.as_deref(),
        Some("Tests failed")
    );

    let clear_ci = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"status-clear","method":"pane/status","params":{{"target":{{"pane_id":"{background_pane}"}},"source":"ci","state":null,"idempotency_key":"status-clear"}}}}"#
        ),
        &primary,
    );
    assert!(clear_ci.contains(r#""state":null"#), "{clear_ci}");
    let restored_context = service.terminal_frame_context();
    let restored = restored_context.panes.get(&background_pane).unwrap();
    assert_eq!(restored.pane_status_state.as_deref(), Some("running"));
    assert_eq!(restored.pane_status_text.as_deref(), Some("Building"));

    let invalid = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"status-invalid","method":"pane/status","params":{"source":"hooks","state":"unknown","idempotency_key":"status-invalid"}}"#,
        &primary,
    );
    assert!(
        invalid.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid}"
    );
}

/// Verifies pane notices append bounded structured message events.
///
/// The notice must retain pane, source, severity, and text without writing raw
/// bytes into a pane, while invalid severity values are rejected.
#[test]
fn runtime_pane_notice_emits_structured_message_event() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let emitted = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"notice","method":"pane/notice","params":{"source":"deploy","severity":"warning","text":"Approval needed","idempotency_key":"notice"}}"#,
        &primary,
    );
    assert!(
        emitted.contains(r#""severity":"warning","emitted":true"#),
        "{emitted}"
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.kind == EventKind::Message
            && event.payload.contains(r#""source":"deploy""#)
            && event.payload.contains(r#""text":"Approval needed""#)
    }));

    let invalid = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"notice-invalid","method":"pane/notice","params":{"source":"deploy","severity":"fatal","text":"no","idempotency_key":"notice-invalid"}}"#,
        &primary,
    );
    assert!(
        invalid.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid}"
    );
}

/// Verifies primary clients can round-trip bounded internal paste buffers.
///
/// Creation must reject implicit replacement, list/read results must remain
/// valid JSON for escaped content, and deletion must remove the named buffer.
#[test]
fn runtime_buffer_controls_round_trip_and_reject_implicit_replace() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"create","method":"buffer/create","params":{"name":"handoff","content":"alpha\n\"beta\"","idempotency_key":"create"}}"#,
        &primary,
    );
    assert!(
        create.contains(r#""created":true,"replaced":false"#),
        "{create}"
    );

    let duplicate = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"duplicate","method":"buffer/create","params":{"name":"handoff","content":"other","idempotency_key":"duplicate"}}"#,
        &primary,
    );
    assert!(
        duplicate.contains(r#""mezzanine_code":"conflict""#),
        "{duplicate}"
    );

    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"buffer/list","params":{}}"#,
        &primary,
    );
    let list_json: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list_json["result"]["buffers"][0]["name"], "handoff");
    assert_eq!(
        list_json["result"]["buffers"][0]["origin"],
        "control:buffer/create"
    );

    let read = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"read","method":"buffer/read","params":{"name":"handoff"}}"#,
        &primary,
    );
    let read_json: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert_eq!(read_json["result"]["content"], "alpha\n\"beta\"");

    let delete = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"delete","method":"buffer/delete","params":{"name":"handoff","idempotency_key":"delete"}}"#,
        &primary,
    );
    assert!(delete.contains(r#""deleted":true"#), "{delete}");
    assert!(service.paste_buffers().get("handoff").is_none());
}

/// Verifies an authoritative terminal view reports the latest applied event.
///
/// Iroh attach clients use this cutoff to discard only redraw wakeups already
/// represented by the returned view, avoiding a redundant control round trip.
#[test]
fn runtime_terminal_view_reports_latest_event_cutoff() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let expected_cutoff = service.event_log().unwrap().latest_event_id();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();

    assert!(!response["result"]["view"].is_null());
    assert_eq!(response["result"]["event_cutoff"], expected_cutoff);
}

/// Verifies conditional terminal-step rendering returns a view only when the
/// applied mutation changes presentation, with a cutoff from the same runtime
/// boundary.
///
/// The client retains `render = false` for old-server compatibility and adds
/// `extensions.render_mode = "if_changed"` for new servers. Invalid modes must
/// remain a visible protocol error rather than silently changing legacy behavior.
#[test]
fn runtime_terminal_step_renders_inline_only_if_changed() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();

    let changed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"changed","method":"terminal/step","params":{"idempotency_key":"changed","client_size":{"columns":80,"rows":24},"render":false,"extensions":{"render_mode":"if_changed"},"input_bytes":[1,58]}}"#,
        &primary,
    );
    let changed: serde_json::Value = serde_json::from_str(&changed).unwrap();
    assert!(changed["result"]["application"]["view_refresh_required"] == true);
    assert!(!changed["result"]["view"].is_null());
    assert_eq!(
        changed["result"]["event_cutoff"],
        service.event_log().unwrap().latest_event_id()
    );

    let unchanged = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"unchanged","method":"terminal/step","params":{"idempotency_key":"unchanged","client_size":{"columns":80,"rows":24},"render":false,"extensions":{"render_mode":"if_changed"},"input_bytes":[]}}"#,
        &primary,
    );
    let unchanged: serde_json::Value = serde_json::from_str(&unchanged).unwrap();
    assert!(unchanged["result"]["view"].is_null());
    assert!(unchanged["result"]["event_cutoff"].is_null());

    let invalid = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"invalid","method":"terminal/step","params":{"idempotency_key":"invalid","render":false,"extensions":{"render_mode":"always_later"},"input_bytes":[]}}"#,
        &primary,
    );
    assert!(
        invalid.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid}"
    );
    service.terminate_all_pane_processes().unwrap();
}
