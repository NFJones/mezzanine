//! Control state tests.

use super::*;

/// Verifies that generic read-only state uses only authoritative session data
/// and does not claim a pane process is running when no runtime process source
/// supplied a primary PID.
#[test]
fn dispatches_read_only_session_methods() {
    let (mut session, primary) = test_session();

    let sessions = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":0,"method":"session/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(sessions.contains(r#""id":"$1""#), "{sessions}");
    assert!(sessions.contains(r#""version":1"#), "{sessions}");
    assert!(sessions.contains(r#""created_at":""#), "{sessions}");
    assert!(sessions.contains(r#""last_attached_at":""#), "{sessions}");
    assert!(sessions.contains(r#""window_count":1"#), "{sessions}");
    assert!(
        sessions.contains(r#""attached_client_count":1"#),
        "{sessions}"
    );
    assert!(sessions.contains(r#""has_primary":true"#), "{sessions}");
    assert!(
        sessions.contains(r#""active_window_id":"@1""#),
        "{sessions}"
    );

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""id":1"#));
    assert!(response.contains(r#""session_id":"$1""#));
    assert!(response.contains(r#""state":"running""#));
    assert!(response.contains(r#""created_at":""#), "{response}");
    assert!(response.contains(r#""updated_at":""#), "{response}");
    assert!(
        response.contains(r#""window_id":"@1","index":0,"name":"0","active":true,"created_at":""#),
        "{response}"
    );
    assert!(response.contains(r#""config_generation":0"#));
    let response_json: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(
        response_json["result"]["session"]["permission_summary"]["command_rule_generation"],
        builtin_rules().len()
    );

    let panes = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"pane/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(panes.contains(r#""session_id":"$1""#));
    assert!(panes.contains(r#""window_id":"@1""#));
    assert!(!panes.contains(r#""session_id":null"#));
    let panes_json: serde_json::Value = serde_json::from_str(&panes).unwrap();
    let pane = &panes_json["result"]["panes"][0];
    assert_eq!(pane["primary_pid"], serde_json::Value::Null);
    assert_eq!(pane["process_state"], "starting");
    assert_eq!(pane["terminal_profile"], DEFAULT_PANE_TERM);
    assert_eq!(pane["history_limit"], DEFAULT_HISTORY_LIMIT);

    let pane_id = pane["pane_id"].as_str().unwrap().to_string();
    session.set_pane_live_state(&pane_id, false).unwrap();
    let exited = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"pane/list","params":{}}"#,
        &mut session,
        &primary,
    );
    let exited_json: serde_json::Value = serde_json::from_str(&exited).unwrap();
    assert_eq!(
        exited_json["result"]["panes"][0]["primary_pid"],
        serde_json::Value::Null
    );
    assert_eq!(exited_json["result"]["panes"][0]["process_state"], "exited");
}

/// Verifies that generic read-only pane state preserves pane metadata restored
/// from snapshots instead of falling back to offline placeholder values.
#[test]
fn generic_pane_state_serializes_restored_snapshot_metadata() {
    let shell = ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh);
    let payload = SessionSnapshotPayload {
        session_id: "$8".to_string(),
        name: "restored".to_string(),
        state: SnapshotSessionState::Detached,
        authoritative_columns: 100,
        authoritative_rows: 40,
        active_window_id: Some("@4".to_string()),
        shell: SnapshotShellMetadata::default(),
        active_config_layers: Vec::new(),
        frame_state: SnapshotFrameState::default(),
        agent_sessions: Vec::new(),
        approval_grants: Vec::new(),
        approval_requests: Vec::new(),
        message_state: None,
        mcp_servers: Vec::new(),
        window_groups: Vec::new(),
        windows: vec![WindowSnapshotPayload {
            window_id: "@4".to_string(),
            index: 0,
            name: "work".to_string(),
            active: true,
            columns: 100,
            rows: 40,
            layout_policy: LayoutPolicy::Tiled.name().to_string(),
            layout_root: None,
            panes: vec![PaneSnapshotPayload {
                pane_id: "%9".to_string(),
                index: 0,
                title: "shell".to_string(),
                active: true,
                live_at_snapshot: true,
                columns: 100,
                rows: 40,
                primary_pid: Some(4242),
                process_state: "running".to_string(),
                current_working_directory: Some("/workspace/project".to_string()),
                readiness_state: "ready".to_string(),
                exit_status: None,
                geometry: Some(SnapshotPaneGeometry {
                    column: 0,
                    row: 0,
                    columns: 100,
                    rows: 40,
                }),
                terminal_modes: TerminalModeState::default(),
                terminal_saved_state: TerminalSavedState::default(),
                terminal_history: Vec::new(),
                terminal_history_line_style_spans: Vec::new(),
                visible_lines: Vec::new(),
                visible_line_style_spans: Vec::new(),
                alternate_screen_active: true,
                transcript_refs: Vec::new(),
            }],
        }],
    };
    let restore_input = crate::snapshot::session_restore_input(&payload).unwrap();
    let mut session = Session::from_restore_input(shell, restore_input).unwrap();
    let primary = session.attach_primary("primary", true).unwrap();

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/list","params":{}}"#,
        &mut session,
        &primary,
    );
    let body: serde_json::Value = serde_json::from_str(&response).unwrap();
    let pane = &body["result"]["panes"][0];

    assert_eq!(pane["pane_id"], "%9");
    assert_eq!(pane["primary_pid"], serde_json::Value::Null);
    assert_eq!(pane["process_state"], "exited");
    assert_eq!(pane["current_working_directory"], "/workspace/project");
    assert_eq!(pane["readiness_state"], "ready");
    assert_eq!(pane["alternate_screen_active"], true);
}

/// Verifies that read-only generic state methods enforce the target fields
/// defined by the protocol instead of accepting and ignoring mismatched target
/// objects. This matters because callers use these methods to scope state to a
/// session or window before rendering it to a client.
#[test]
fn read_only_state_requests_validate_and_apply_targets() {
    let (mut session, primary) = test_session();
    session.new_window(&primary, "work", false).unwrap();

    let targeted_panes = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/list","params":{"target":{"window_id":"@2"}}}"#,
        &mut session,
        &primary,
    );
    let targeted_panes: serde_json::Value = serde_json::from_str(&targeted_panes).unwrap();
    let panes = targeted_panes["result"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1, "{targeted_panes}");
    assert_eq!(panes[0]["window_id"], "@2");

    let session_panes = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"pane/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
    );
    let session_panes: serde_json::Value = serde_json::from_str(&session_panes).unwrap();
    let panes = session_panes["result"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 2, "{session_panes}");
    assert!(panes.iter().any(|pane| pane["window_id"] == "@1"));
    assert!(panes.iter().any(|pane| pane["window_id"] == "@2"));

    let named_session = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"session/get","params":{"target":{"name":"default"}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        named_session.contains(r#""session_id":"$1""#),
        "{named_session}"
    );

    let missing_session = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"window/list","params":{"target":{"session_id":"$missing"}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        missing_session.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session}"
    );
}

/// Verifies that `observer/list` applies the spec-defined state filter in
/// addition to SessionTarget validation. Without this, callers asking for one
/// observer decision state receive unrelated pending or decided requests.
#[test]
fn observer_list_filters_by_requested_state() {
    let (mut session, primary) = test_session();
    session.request_observer("pending");
    let (_rejected_client, rejected_request) = session.request_observer("rejected");
    session
        .reject_observer_target_with_reason(&primary, rejected_request.as_str(), Some("no".into()))
        .unwrap();

    let rejected = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"observer/list","params":{"target":{"default":true},"state":"rejected"}}"#,
        &mut session,
        &primary,
    );
    let rejected: serde_json::Value = serde_json::from_str(&rejected).unwrap();
    let observers = rejected["result"]["observers"].as_array().unwrap();
    assert_eq!(observers.len(), 1, "{rejected}");
    assert_eq!(observers[0]["state"], "rejected");

    let all = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"observer/list","params":{"state":null}}"#,
        &mut session,
        &primary,
    );
    let all: serde_json::Value = serde_json::from_str(&all).unwrap();
    assert_eq!(all["result"]["observers"].as_array().unwrap().len(), 2);

    let invalid = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"observer/list","params":{"state":"missing"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid}"
    );
}

/// Verifies session attach dispatcher enforces primary and observer semantics.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_attach_dispatcher_enforces_primary_and_observer_semantics() {
    let (mut session, primary) = test_session();
    session.detach_primary(&primary).unwrap();

    let primary_attach = dispatch_session_attach_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/attach","params":{"role":"primary","client":{"name":"reattach","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"idempotency_key":"reattach"}}"#,
        &mut session,
    );
    assert!(primary_attach.contains(r#""role":"primary""#));
    assert!(primary_attach.contains(r#""approval_pending":false"#));
    assert!(
        primary_attach.contains(
            r#""descriptor":{"name":"reattach","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}"#
        ),
        "{primary_attach}"
    );

    let observer_attach = dispatch_session_attach_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/attach","params":{"role":"observer","client":{"name":"watch","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"idempotency_key":"observer"}}"#,
        &mut session,
    );
    assert!(observer_attach.contains(r#""role":"pending_observer""#));
    assert!(observer_attach.contains(r#""approval_pending":true"#));
    assert_eq!(session.observers().len(), 1);
    let attached_primary = session.primary_client_id().cloned().unwrap();
    let observer_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"observer/list","params":{}}"#,
        &mut session,
        &attached_primary,
    );
    assert!(
        observer_list.contains(
            r#""descriptor":{"name":"watch","interactive":false,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}"#
        ),
        "{observer_list}"
    );
}

/// Verifies pending observer can attach observer without receiving session data.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pending_observer_can_attach_observer_without_receiving_session_data() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let mut input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"observer","requested_version":1,"requested_role":"observer","client":{"name":"observer","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    input.extend_from_slice(&encode_control_body(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/attach","params":{"role":"observer","client":{"name":"observer","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"idempotency_key":"observer-attach"}}"#,
    ));

    let (output, _) = handle_control_frames_for_connection(
        &input,
        4096,
        &mut session,
        &mut connection,
        &mut cache,
    )
    .unwrap();
    let (init_body, first_len) = decode_control_frame(&output, 4096).unwrap();
    let (attach_body, _) = decode_control_frame(&output[first_len..], 4096).unwrap();

    assert!(init_body.contains(r#""granted_role":"pending_observer""#));
    assert!(attach_body.contains(r#""role":"pending_observer""#));
    assert!(attach_body.contains(r#""approval_pending":true"#));
    assert!(!attach_body.contains(r#""windows""#));
    assert!(!attach_body.contains(r#""panes""#));
}

/// Verifies dispatches mutating window and pane methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_mutating_window_and_pane_methods() {
    let (mut session, primary) = test_session();

    let window_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"a"}}"#,
        &mut session,
        &primary,
    );
    assert!(window_response.contains(r#""window_id":"@2""#));

    let rename_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":6,"method":"window/rename","params":{"target":{"window_id":"@2"},"name":"renamed","idempotency_key":"rename"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        rename_response.contains(r#""window":{"id":"@2""#),
        "{rename_response}"
    );
    assert!(rename_response.contains(r#""name":"renamed""#));
    assert!(!rename_response.contains(r#""renamed":true"#));

    let pane_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"pane/create","params":{"split":"vertical","select":true,"idempotency_key":"b"}}"#,
        &mut session,
        &primary,
    );
    assert!(pane_response.contains(r#""pane_id":"%3""#));
    assert_eq!(session.active_window().unwrap().panes().len(), 2);

    let resize_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"pane/resize","params":{"target":{"pane_index":1},"size":{"mode":"cells","columns":20,"rows":10},"idempotency_key":"resize"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        resize_response.contains(r#""columns":20"#),
        "{resize_response}"
    );
    assert!(resize_response.contains(r#""rows":10"#));

    let delta_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"pane/resize","params":{"target":{"pane_index":1},"size":{"mode":"delta","direction":"right","amount":5},"idempotency_key":"resize-delta"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        delta_response.contains(r#""columns":25"#),
        "{delta_response}"
    );
}

/// Verifies that generic control dispatch rejects pane-process creation fields
/// that require a live terminal runtime instead of silently creating in-memory
/// windows or panes without starting the requested process.
#[test]
fn generic_creation_rejects_runtime_required_process_fields_without_mutation() {
    let (mut session, primary) = test_session();

    let window_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","shell_command":"true","start_directory":"/tmp","idempotency_key":"window-runtime-required"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        window_response.contains(r#""mezzanine_code":"invalid_state""#),
        "{window_response}"
    );
    assert!(
        window_response
            .contains("window/create requires an attached terminal runtime for `shell_command`"),
        "{window_response}"
    );
    assert_eq!(session.windows().len(), 1);

    let pane_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"pane/create","params":{"split":"vertical","shell_command":"true","start_directory":"/tmp","idempotency_key":"pane-runtime-required"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        pane_response.contains(r#""mezzanine_code":"invalid_state""#),
        "{pane_response}"
    );
    assert!(
        pane_response
            .contains("pane/create requires an attached terminal runtime for `shell_command`"),
        "{pane_response}"
    );
    assert_eq!(session.active_window().unwrap().panes().len(), 1);

    let size_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"pane/create","params":{"split":"vertical","size":{"mode":"cells","columns":20,"rows":10},"idempotency_key":"pane-size-runtime-required"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        size_response.contains(r#""mezzanine_code":"invalid_state""#),
        "{size_response}"
    );
    assert!(
        size_response.contains("pane/create requires an attached terminal runtime for `size`"),
        "{size_response}"
    );
    assert_eq!(session.active_window().unwrap().panes().len(), 1);
}

/// Verifies that `LayoutState.root` is a reconstructable recursive tree. A
/// vertical split whose right child is split horizontally must serialize as a
/// split node containing another split node, not as one flat list of panes.
#[test]
fn layout_state_serializes_recursive_geometry_tree() {
    let (mut session, primary) = test_session();

    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Horizontal, true)
        .unwrap();

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/list","params":{}}"#,
        &mut session,
        &primary,
    );
    let body: serde_json::Value = serde_json::from_str(&response).unwrap();
    let root = &body["result"]["windows"][0]["layout"]["root"];

    assert_eq!(root["type"], "split");
    assert_eq!(root["direction"], "vertical");
    assert_eq!(root["sizes"], serde_json::json!([40, 40]));
    assert_eq!(root["children"][0]["type"], "pane");
    assert_eq!(root["children"][0]["pane_id"], "%1");
    assert_eq!(root["children"][1]["type"], "split");
    assert_eq!(root["children"][1]["direction"], "horizontal");
    assert_eq!(root["children"][1]["sizes"], serde_json::json!([12, 12]));
    assert_eq!(root["children"][1]["children"][0]["pane_id"], "%2");
    assert_eq!(root["children"][1]["children"][1]["pane_id"], "%3");
}

/// Verifies that `LayoutState.root` uses the stored split ancestry instead of
/// reconstructing a possible tree from pane rectangles. A symmetric 2x2 grid
/// can be cut vertically or horizontally; the protocol must report the
/// horizontal root created by the user's first split.
#[test]
fn layout_state_preserves_ambiguous_original_split_ancestry() {
    let (mut session, primary) = test_session();

    session
        .split_active_pane_select(&primary, SplitDirection::Horizontal, true)
        .unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    session.select_pane(&primary, "%1").unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/list","params":{}}"#,
        &mut session,
        &primary,
    );
    let body: serde_json::Value = serde_json::from_str(&response).unwrap();
    let root = &body["result"]["windows"][0]["layout"]["root"];

    assert_eq!(root["type"], "split");
    assert_eq!(root["direction"], "horizontal");
    assert_eq!(root["sizes"], serde_json::json!([12, 12]));
    assert_eq!(root["children"][0]["direction"], "vertical");
    assert_eq!(root["children"][1]["direction"], "vertical");
    assert_eq!(root["children"][0]["children"][0]["pane_id"], "%1");
    assert_eq!(root["children"][0]["children"][1]["pane_id"], "%4");
    assert_eq!(root["children"][1]["children"][0]["pane_id"], "%2");
    assert_eq!(root["children"][1]["children"][1]["pane_id"], "%3");
}
