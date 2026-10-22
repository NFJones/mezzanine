//! Runtime regressions for external harness control integrations.

use super::*;
use mez_core::ids::ClientId;

/// Hostile combining output must remain bounded at the product output boundary
/// and leave the serialized runtime able to answer ordinary control requests.
#[test]
fn runtime_hostile_combining_output_preserves_control_progress() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("owner", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .apply_pane_output_bytes("%1", format!("e{}x", "\u{301}".repeat(16_384)).into_bytes())
        .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"healthy","method":"session/get","params":{}}"#,
        &primary,
    );
    assert!(response.contains("\"result\""), "{response}");
    let lines = service.pane_screen("%1").unwrap().visible_lines();
    assert!(lines[0].len() <= 257);
    assert!(lines[0].ends_with('x'));
}

/// A valid resource-bounded size can still fail a later window's split minimum.
/// Repeated control rejection must not emit layout events, change screens or
/// canonical geometry, or let the equality fast path accept a corrupt retry.
#[test]
fn runtime_control_rejected_layout_resize_preserves_session_and_event_cutoff() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("owner", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.session.new_window(&primary, "split", true).unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let before = format!("{:?}", service.session());
    let cutoff = service.event_log().unwrap().latest_event_id();
    for id in ["first", "retry"] {
        let body = serde_json::json!({
            "jsonrpc":"2.0", "id":id, "method":"terminal/step",
            "params":{"client_size":{"columns":2,"rows":24},
                "idempotency_key":id, "input_bytes":[], "render":false}
        })
        .to_string();
        let response = service.dispatch_runtime_control_body(&body, &primary);
        assert!(response.contains("invalid_params"), "{response}");
        assert_eq!(format!("{:?}", service.session()), before);
        assert_eq!(service.event_log().unwrap().latest_event_id(), cutoff);
    }
}

/// Excessive control view/step geometry and direct resize must fail before
/// changing descriptors, layout, event cutoffs, or scheduling PTY resize work.
/// A healthy request afterward confirms the session remains usable.
#[test]
fn runtime_control_rejects_excessive_geometry_without_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let before = format!("{:?}", service.session());
    let cutoff = service.event_log().unwrap().latest_event_id();
    for method in ["terminal/step", "terminal/view"] {
        let request = serde_json::json!({
            "jsonrpc":"2.0", "id":"oversized", "method":method,
            "params":{"client_size":{"columns":1024,"rows":257},
                "idempotency_key":"oversized", "input_bytes":[]}
        })
        .to_string();
        let response = service.dispatch_runtime_control_body(&request, &primary);
        assert!(response.contains("invalid_params"), "{response}");
        assert_eq!(format!("{:?}", service.session()), before);
        assert_eq!(service.event_log().unwrap().latest_event_id(), cutoff);
    }
    assert!(
        service
            .resize_attached_primary_terminal(
                &primary,
                Size {
                    columns: 1024,
                    rows: 257
                }
            )
            .is_err()
    );
    assert_eq!(format!("{:?}", service.session()), before);
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"healthy","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        &primary,
    );
    assert!(response.contains("\"result\""), "{response}");
}

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

/// A client carrying the exact previous presentation identity receives a
/// metadata-only response, but an obsolete identity receives a full view.
#[test]
fn runtime_terminal_view_conditional_response_keeps_exact_base() {
    let mut service = test_runtime_service();
    // Exact-base reuse requires identical rendered content, including chrome.
    // Clock and uptime fields legitimately invalidate identities at rollover.
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "static-conditional-view".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\ntemplate = \"#{window.list}\"\nright_status = \"static\"\n[frames.pane]\ntemplate = \"#{pane.title}\"\nright_status = \"static\"\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let request = |identity: Option<&str>| {
        let mut params = serde_json::json!({"client_size":{"columns":80,"rows":24}});
        if let Some(identity) = identity {
            params["if_view_identity"] = identity.into();
        }
        serde_json::json!({"jsonrpc":"2.0","id":"view","method":"terminal/view","params":params})
            .to_string()
    };
    let initial: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(&request(None), &primary))
            .unwrap();
    let identity = initial["result"]["view_identity"].as_str().unwrap();
    assert!(!initial["result"]["view"].is_null());
    let unchanged: serde_json::Value = serde_json::from_str(
        &service.dispatch_runtime_control_body(&request(Some(identity)), &primary),
    )
    .unwrap();
    assert_eq!(unchanged["result"]["not_modified"], true);
    assert!(unchanged["result"].get("view").is_none());
    assert_eq!(unchanged["result"]["view_identity"], identity);
    assert!(
        service
            .dispatch_runtime_control_body(&request(Some("invalid")), &primary)
            .contains("invalid_params"),
        "malformed conditional identities must be rejected"
    );
    let resized_request = serde_json::json!({
        "jsonrpc": "2.0", "id": "resized", "method": "terminal/view",
        "params": {"client_size": {"columns": 100, "rows": 30}, "if_view_identity": identity}
    });
    let resized: serde_json::Value = serde_json::from_str(
        &service.dispatch_runtime_control_body(&resized_request.to_string(), &primary),
    )
    .unwrap();
    assert!(!resized["result"]["view"].is_null());
    let other = service
        .attach_primary("other", true, Size::new(80, 24).unwrap(), 121)
        .unwrap();
    let other_response: serde_json::Value = serde_json::from_str(
        &service.dispatch_runtime_control_body(&request(Some(identity)), &other),
    )
    .unwrap();
    assert!(!other_response["result"]["view"].is_null());
    assert_ne!(other_response["result"]["view_identity"], identity);
    let changed: serde_json::Value = serde_json::from_str(
        &service.dispatch_runtime_control_body(&request(Some("0".repeat(64).as_str())), &primary),
    )
    .unwrap();
    assert!(!changed["result"]["view"].is_null());
}

/// Verifies control-rendered zen labels carry commit receipts and arm exactly
/// once after an authenticated client reports a complete local frame write.
/// Malformed and duplicate receipts must not create or renew a deadline.
#[test]
fn runtime_terminal_view_focus_receipts_arm_once_after_acknowledgement() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_terminal_zen_mode_for_tests(true);
    service
        .execute_terminal_command(&primary, "new-window receipt-focus")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    let presentation_ids = response["result"]["presentation_ids"]
        .as_array()
        .expect("terminal view should carry presentation receipts");
    assert_eq!(presentation_ids.len(), 1);
    let identity = response["result"]["view_identity"].as_str().unwrap();
    let still_pending: serde_json::Value = serde_json::from_str(
        &service.dispatch_runtime_control_body(
            &format!(r#"{{"jsonrpc":"2.0","id":"same","method":"terminal/view","params":{{"client_size":{{"columns":80,"rows":24}},"if_view_identity":"{identity}"}}}}"#),
            &primary,
        ),
    ).unwrap();
    assert_eq!(still_pending["result"]["not_modified"], true);
    assert_eq!(
        service.zen_focus_label_next_due_ms_for_client(&primary, 1),
        None,
        "rendering alone must leave the label pending"
    );

    let presentation_id = presentation_ids[0].as_u64().unwrap();
    let malformed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"bad","method":"terminal/presentation/acknowledge","params":{"idempotency_key":"bad","presentation_ids":[0]}}"#,
        &primary,
    );
    assert!(
        malformed.contains(r#""mezzanine_code":"invalid_params""#),
        "{malformed}"
    );
    assert_eq!(
        service.zen_focus_label_next_due_ms_for_client(&primary, 1),
        None
    );

    let acknowledgement = format!(
        r#"{{"jsonrpc":"2.0","id":"ack","method":"terminal/presentation/acknowledge","params":{{"idempotency_key":"ack-{presentation_id}","presentation_ids":[{presentation_id}]}}}}"#
    );
    let first = service.dispatch_runtime_control_body(&acknowledgement, &primary);
    assert!(first.contains(r#""acknowledged":true"#), "{first}");
    let after_ack: serde_json::Value = serde_json::from_str(
        &service.dispatch_runtime_control_body(
            &format!(r#"{{"jsonrpc":"2.0","id":"after-ack","method":"terminal/view","params":{{"client_size":{{"columns":80,"rows":24}},"if_view_identity":"{identity}"}}}}"#),
            &primary,
        ),
    ).unwrap();
    assert_eq!(after_ack["result"]["not_modified"], true);
    assert_eq!(after_ack["result"]["view_identity"], identity);
    let deadline = service
        .zen_focus_label_next_due_ms_for_client(&primary, 1)
        .expect("acknowledgement should arm the configured expiry");

    let duplicate = service.dispatch_runtime_control_body(&acknowledgement, &primary);
    assert_eq!(
        duplicate, first,
        "idempotent replay should reuse its response"
    );
    assert_eq!(
        service.zen_focus_label_next_due_ms_for_client(&primary, 1),
        Some(deadline),
        "duplicate acknowledgement must not renew the deadline"
    );

    let stale = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"stale","method":"terminal/presentation/acknowledge","params":{{"idempotency_key":"stale-{presentation_id}","presentation_ids":[{presentation_id}]}}}}"#
        ),
        &primary,
    );
    assert!(stale.contains(r#""acknowledged":false"#), "{stale}");
    assert_eq!(
        service.zen_focus_label_next_due_ms_for_client(&primary, 1),
        Some(deadline),
        "stale acknowledgement must not renew the deadline"
    );
}

/// Verifies an inline terminal-step view carries the receipt for a pane label
/// created by the same direct prefix-arrow focus mutation.
#[test]
fn runtime_terminal_step_focus_view_carries_presentation_receipt() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .execute_terminal_command(
            &primary,
            "split-window; rename-pane receipt-pane; select-pane -t 0; zen on",
        )
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"focus","method":"terminal/step","params":{"idempotency_key":"focus-arrow","client_size":{"columns":80,"rows":24},"render":true,"input_bytes":[1,27,91,67]}}"#,
        &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();

    assert_eq!(response["result"]["application"]["mux_actions_applied"], 1);
    assert!(!response["result"]["view"].is_null());
    assert_eq!(
        response["result"]["presentation_ids"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
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
    assert!(
        service
            .drain_deferred_effects_transition()
            .side_effects
            .into_iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
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

/// Verifies `:exit` acknowledges terminal-session termination instead of
/// attempting to render command feedback against the killed runtime.
#[test]
fn runtime_terminal_step_exit_returns_terminated_response() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"exit-prompt","method":"terminal/step","params":{"idempotency_key":"exit-prompt","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[1,58]}}"#,
        &primary,
    );
    let prompt: serde_json::Value = serde_json::from_str(&prompt).unwrap();
    assert!(prompt.get("result").is_some(), "{prompt}");

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"exit-submit","method":"terminal/step","params":{"idempotency_key":"exit-submit","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[101,120,105,116,13]}}"#,
        &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["session_terminated"], true);
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Killed);
}

/// Verifies a complete `Ctrl+A :exit Enter` sequence delivered in one terminal
/// step opens the command prompt, submits `exit`, and acknowledges termination.
/// Host terminal reads are not key-event boundaries, so fast input must behave
/// identically to the split-step path covered above.
#[test]
fn runtime_terminal_step_coalesced_exit_returns_terminated_response() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"exit-coalesced","method":"terminal/step","params":{"idempotency_key":"exit-coalesced","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[1,58,101,120,105,116,13]}}"#,
        &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["session_terminated"], true);
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Killed);
}

/// Verifies a framed agent-prompt edit without an inline view wakes the exact
/// primary client's pushed-render stream.
///
/// Iroh v3 deliberately sends `terminal/step` with `render = false` because
/// pushed snapshots are authoritative. Agent prompt editing has no PTY echo to
/// produce a pane-output invalidation, so the framed mutation must retain an
/// `AgentPrompt` render effect after acknowledging the edit. Empty follow-on
/// steps must not create spurious invalidations.
#[test]
fn runtime_terminal_step_without_inline_view_invalidates_agent_prompt() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    let _ = service.drain_deferred_effects_transition();
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-edit","method":"terminal/step","params":{"idempotency_key":"agent-edit","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[97]}}"#,
        &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(
        response["result"]["application"]["agent_prompt_inputs_applied"],
        1
    );
    assert!(response["result"]["view"].is_null());
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "a"
    );
    assert_eq!(
        service.drain_deferred_effects_transition().side_effects,
        vec![
            RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: crate::runtime::RenderInvalidationReason::AgentPrompt,
            },
            RuntimeSideEffect::RenderClient {
                client_id: observer,
                reason: crate::runtime::RenderInvalidationReason::AgentPrompt,
            },
        ]
    );

    let unchanged = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-empty","method":"terminal/step","params":{"idempotency_key":"agent-empty","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[]}}"#,
        &primary,
    );
    let unchanged: serde_json::Value = serde_json::from_str(&unchanged).unwrap();
    assert_eq!(
        unchanged["result"]["application"]["agent_prompt_inputs_applied"],
        0
    );
    assert!(
        service
            .drain_deferred_effects_transition()
            .side_effects
            .is_empty()
    );
}

/// Verifies framed terminal steps queue owner-only divider projections while
/// keeping bounded inline responses free of full terminal views. Pane content
/// remains deferred until the release debounce commits layout.
#[test]
fn runtime_terminal_step_projects_divider_only_until_release_debounce() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    let border = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .mouse_border_cells
        .into_iter()
        .next()
        .expect("vertical split should expose a divider");
    let moved_column = border.column.saturating_add(3);
    let movement = format!(
        "\u{1b}[<0;{};{}M\u{1b}[<32;{};{}M",
        border.column.saturating_add(1),
        border.row.saturating_add(1),
        moved_column.saturating_add(1),
        border.row.saturating_add(1),
    )
    .into_bytes();
    let movement_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "divider-move",
        "method": "terminal/step",
        "params": {
            "idempotency_key": "divider-move",
            "client_size": {"columns": 80, "rows": 24},
            "render": true,
            "input_bytes": movement,
        },
    })
    .to_string();

    let movement = service.dispatch_runtime_control_body(&movement_request, &primary);
    let movement: serde_json::Value = serde_json::from_str(&movement).unwrap();
    assert_eq!(
        movement["result"]["application"]["mouse_actions_reported"],
        2
    );
    assert!(movement["result"]["view"].is_null());
    assert_eq!(
        service.drain_deferred_effects_transition().side_effects,
        vec![RuntimeSideEffect::RenderClient {
            client_id: primary.clone(),
            reason: crate::runtime::RenderInvalidationReason::ResizeDrag,
        }]
    );

    let release = format!(
        "\u{1b}[<0;{};{}m",
        moved_column.saturating_add(1),
        border.row.saturating_add(1),
    )
    .into_bytes();
    let release_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "divider-release",
        "method": "terminal/step",
        "params": {
            "idempotency_key": "divider-release",
            "client_size": {"columns": 80, "rows": 24},
            "render": true,
            "input_bytes": release,
        },
    })
    .to_string();

    let release = service.dispatch_runtime_control_body(&release_request, &primary);
    let release: serde_json::Value = serde_json::from_str(&release).unwrap();
    assert_eq!(
        release["result"]["application"]["mouse_actions_reported"],
        1
    );
    assert!(release["result"]["view"].is_null());
    assert!(
        service
            .drain_deferred_effects_transition()
            .side_effects
            .is_empty()
    );

    let commit = service
        .apply_resize_debounce_timer_transition(primary.as_str(), true)
        .unwrap();
    assert!(commit.applied);
    assert!(commit.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            client_id,
            reason: crate::runtime::RenderInvalidationReason::FullRedraw,
        } if client_id == &primary
    )));
}

/// Builds two primary clients with different caller-local focus over one window.
///
/// The first primary focuses the left pane while the second focuses the right
/// pane. The session-wide active pane remains the right pane, so an omitted
/// close target cannot silently mean either caller-local focus or the global
/// projection.
fn two_primary_close_fixture() -> (RuntimeSessionService, ClientId, ClientId, String, String) {
    let size = Size::new(100, 40).unwrap();
    let mut service = test_runtime_service_with_size(size);
    let first = service.attach_primary("first", true, size, 120).unwrap();
    let left = service.active_pane_id().unwrap();
    let right = service
        .session
        .split_active_pane(&first, SplitDirection::Vertical)
        .unwrap()
        .to_string();
    service.session.select_pane(&first, left.as_str()).unwrap();
    let second = service.attach_primary("second", true, size, 121).unwrap();
    service
        .session
        .select_pane(&second, right.as_str())
        .unwrap();
    assert_eq!(
        service.session.active_pane_for(&first).unwrap().id.as_str(),
        left.as_str(),
        "first primary must retain caller-local focus on the left pane"
    );
    assert_eq!(
        service
            .session
            .active_pane_for(&second)
            .unwrap()
            .id
            .as_str(),
        right.as_str(),
        "second primary must retain caller-local focus on the right pane"
    );
    assert_eq!(
        service.active_pane_id().unwrap(),
        right,
        "global focus must differ from the first primary's local focus"
    );
    (service, first, second, left, right)
}

/// Captures the stable display identity of one surviving pane.
///
/// Layout indexes may legitimately shift when a sibling pane is removed, so
/// sibling preservation is asserted by exact pane identity, title and liveness
/// rather than positional geometry, which legitimately reflows.
fn pane_display_identity(service: &RuntimeSessionService, pane_id: &str) -> (String, String, bool) {
    let pane = service
        .session
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .find(|pane| pane.id.as_str() == pane_id)
        .unwrap_or_else(|| panic!("pane {pane_id} must remain present"));
    (pane.id.to_string(), pane.title.clone(), pane.live)
}

/// Asserts one close request is rejected before any session or event effect.
fn assert_pane_close_rejected(
    service: &mut RuntimeSessionService,
    primary: &ClientId,
    request_id: &str,
    params: serde_json::Value,
    expected_code: &str,
) {
    let before = format!("{:?}", service.session);
    let cutoff = service.event_log().unwrap().latest_event_id();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "pane/close",
        "params": params,
    })
    .to_string();
    let response = service.dispatch_runtime_control_body(&body, primary);
    assert!(
        response.contains(expected_code),
        "close rejection must carry {expected_code}: {response}"
    );
    assert_eq!(
        format!("{:?}", service.session),
        before,
        "rejected close must not mutate session state"
    );
    assert_eq!(
        service.event_log().unwrap().latest_event_id(),
        cutoff,
        "rejected close must not append lifecycle events"
    );
}

/// Asserts the admitted close event names the same pane and window descriptor.
fn assert_exact_pane_close_event(
    service: &RuntimeSessionService,
    cutoff: u64,
    pane_id: &str,
    window_id: &str,
) {
    let events =
        service
            .event_log()
            .unwrap()
            .replay_after_for(&EventAudience::AllPrimaries, cutoff, 8);
    let closes: Vec<_> = events
        .iter()
        .filter(|event| {
            event.kind == EventKind::PaneChanged && event.payload.contains(r#""state":"closed""#)
        })
        .collect();
    assert_eq!(
        closes.len(),
        1,
        "admitted close must publish exactly one close event: {events:?}"
    );
    let close = closes[0];
    assert!(
        close.payload.contains(&format!(r#""pane_id":"{pane_id}""#)),
        "close event pane identity must match the admitted target: {}",
        close.payload
    );
    assert!(
        close
            .payload
            .contains(&format!(r#""window_id":"{window_id}""#)),
        "close event window identity must match the admitted descriptor: {}",
        close.payload
    );
}

/// `pane/close` must require one explicit `PaneTarget` and freeze that exact
/// pane through cleanup, layout removal and event publication.
///
/// The two-primary fixture makes caller-local and global focus disagree. An
/// omitted target must be rejected by the supported registry boundary before
/// any turn cancellation or process/layout cleanup runs. Explicit pane-id,
/// title and index selectors must still close exactly one original target,
/// preserve its sibling, honor the live-pane non-force rejection, and publish a
/// close event naming the same descriptor that was removed.
#[test]
fn runtime_pane_close_rejects_omitted_target_and_preserves_exact_admitted_target() {
    for force in [false, true] {
        let (mut service, first, _, left, right) = two_primary_close_fixture();
        let request_id = if force {
            "omitted-force-close"
        } else {
            "omitted-nonforce-close"
        };
        assert_pane_close_rejected(
            &mut service,
            &first,
            request_id,
            serde_json::json!({"force": force, "idempotency_key": request_id}),
            "invalid_params",
        );
        assert!(service.session.contains_pane(&left));
        assert!(service.session.contains_pane(&right));
    }

    for (request_id, params) in [
        (
            "malformed-nested-selector",
            serde_json::json!({
                "target": {"pane_id": 17, "pane_index": 0},
                "force": true,
                "idempotency_key": "malformed-nested-selector",
            }),
        ),
        (
            "malformed-top-level-selector",
            serde_json::json!({
                "pane_id": 17,
                "pane_index": 0,
                "force": true,
                "idempotency_key": "malformed-top-level-selector",
            }),
        ),
    ] {
        let (mut service, first, _, left, right) = two_primary_close_fixture();
        assert_pane_close_rejected(&mut service, &first, request_id, params, "invalid_params");
        assert!(service.session.contains_pane(&left));
        assert!(service.session.contains_pane(&right));
    }

    let (mut service, first, _, left, right) = two_primary_close_fixture();
    assert_pane_close_rejected(
        &mut service,
        &first,
        "conflicting-selectors",
        serde_json::json!({
            "target": {"pane_id": right.clone(), "pane_title": "conflict-title"},
            "force": true,
            "idempotency_key": "conflicting-selectors",
        }),
        "invalid_params",
    );
    assert!(service.session.contains_pane(&left));
    assert!(service.session.contains_pane(&right));

    let (mut service, first, _, left, right) = two_primary_close_fixture();
    assert_pane_close_rejected(
        &mut service,
        &first,
        "null-selector",
        serde_json::json!({
            "target": null,
            "force": true,
            "idempotency_key": "null-selector",
        }),
        "invalid_params",
    );
    assert!(service.session.contains_pane(&left));
    assert!(service.session.contains_pane(&right));

    // A dead caller-local pane makes the pre-fix divergence visible even
    // without force: cleanup inspects global focus while removal later uses
    // the invoking client's navigation. Omission must still reject first.
    let (mut service, first, _, left, right) = two_primary_close_fixture();
    service.session.set_pane_live_state(&left, false).unwrap();
    assert_pane_close_rejected(
        &mut service,
        &first,
        "omitted-dead-nonforce-close",
        serde_json::json!({"force": false, "idempotency_key": "omitted-dead-nonforce-close"}),
        "invalid_params",
    );
    assert!(service.session.contains_pane(&left));
    assert!(service.session.contains_pane(&right));

    // Non-force rejection on a live exact target must not cancel turns or
    // remove the pane; it is a policy refusal, not an omitted-target alias.
    let (mut service, first, _, left, right) = two_primary_close_fixture();
    assert_pane_close_rejected(
        &mut service,
        &first,
        "explicit-live-nonforce-close",
        serde_json::json!({
            "target": {"pane_id": right.clone()},
            "force": false,
            "idempotency_key": "explicit-live-nonforce-close",
        }),
        "forbidden",
    );
    assert!(service.session.contains_pane(&left));
    assert!(service.session.contains_pane(&right));

    // An admitted explicit close must remove exactly the requested pane and
    // publish that same pane/window descriptor after its sibling survives.
    let (mut service, first, _, left, right) = two_primary_close_fixture();
    let sibling_before = pane_display_identity(&service, &left);
    let window_id = service
        .session
        .active_window_for(&first)
        .unwrap()
        .id
        .to_string();
    let cutoff = service.event_log().unwrap().latest_event_id();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "explicit-force-close",
        "method": "pane/close",
        "params": {
            "target": {"pane_id": right.clone()},
            "force": true,
            "idempotency_key": "explicit-force-close",
        },
    })
    .to_string();
    let response: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(&body, &first)).unwrap();
    assert_eq!(response["result"]["closed"], true, "{response}");
    assert!(!service.session.contains_pane(&right));
    assert!(service.session.contains_pane(&left));
    assert_eq!(pane_display_identity(&service, &left), sibling_before);
    assert_exact_pane_close_event(&service, cutoff, &right, &window_id);

    // Title selectors resolve to the named pane, not the global active pane.
    let (mut service, first, _, left, right) = two_primary_close_fixture();
    service
        .session
        .set_pane_title_explicit(&left, "left-close-title")
        .unwrap();
    let sibling_before = pane_display_identity(&service, &right);
    let window_id = service
        .session
        .active_window_for(&first)
        .unwrap()
        .id
        .to_string();
    let cutoff = service.event_log().unwrap().latest_event_id();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "title-force-close",
        "method": "pane/close",
        "params": {
            "target": {"pane_title": "left-close-title"},
            "force": true,
            "idempotency_key": "title-force-close",
        },
    })
    .to_string();
    let response: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(&body, &first)).unwrap();
    assert_eq!(response["result"]["closed"], true, "{response}");
    assert!(!service.session.contains_pane(&left));
    assert!(service.session.contains_pane(&right));
    assert_eq!(pane_display_identity(&service, &right), sibling_before);
    assert_exact_pane_close_event(&service, cutoff, &left, &window_id);

    // Index selectors resolve within the active window to the indexed pane.
    let (mut service, first, _, left, right) = two_primary_close_fixture();
    let cutoff = service.event_log().unwrap().latest_event_id();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "index-force-close",
        "method": "pane/close",
        "params": {
            "target": {"pane_index": 0},
            "force": true,
            "idempotency_key": "index-force-close",
        },
    })
    .to_string();
    let response: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(&body, &first)).unwrap();
    assert_eq!(response["result"]["closed"], true, "{response}");
    assert!(!service.session.contains_pane(&left));
    assert!(service.session.contains_pane(&right));
    let events =
        service
            .event_log()
            .unwrap()
            .replay_after_for(&EventAudience::AllPrimaries, cutoff, 8);
    assert!(
        events.iter().any(|event| {
            event.kind == EventKind::PaneChanged
                && event.payload.contains(r#""state":"closed""#)
                && event.payload.contains(&format!(r#""pane_id":"{left}""#))
        }),
        "index close must publish the exact indexed pane: {events:?}"
    );

    // Explicit non-force close of a dead pane remains admitted and exact.
    let (mut service, first, _, left, right) = two_primary_close_fixture();
    service.session.set_pane_live_state(&left, false).unwrap();
    let cutoff = service.event_log().unwrap().latest_event_id();
    let window_id = service
        .session
        .active_window_for(&first)
        .unwrap()
        .id
        .to_string();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "dead-explicit-close",
        "method": "pane/close",
        "params": {
            "target": {"pane_id": left.clone()},
            "force": false,
            "idempotency_key": "dead-explicit-close",
        },
    })
    .to_string();
    let response: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(&body, &first)).unwrap();
    assert_eq!(response["result"]["closed"], true, "{response}");
    assert!(!service.session.contains_pane(&left));
    assert!(service.session.contains_pane(&right));
    assert_exact_pane_close_event(&service, cutoff, &left, &window_id);
}
