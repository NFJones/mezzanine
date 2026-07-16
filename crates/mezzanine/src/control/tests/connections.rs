//! Control connections tests.

use super::*;

/// Verifies handles one framed control request.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn handles_one_framed_control_request() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let mut request = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    request.extend_from_slice(&encode_control_body(
        r#"{"jsonrpc":"2.0","id":2,"method":"window/list","params":{}}"#,
    ));

    let (response, consumed) =
        handle_control_frames(&request, 4096, &mut session, &mut connection, &mut cache).unwrap();
    let (init_body, first_len) = decode_control_frame(&response, 4096).unwrap();
    let (list_body, second_len) = decode_control_frame(&response[first_len..], 4096).unwrap();

    assert_eq!(consumed, request.len());
    assert_eq!(first_len + second_len, response.len());
    assert!(init_body.contains(r#""granted_role":"primary""#));
    assert!(connection.initialized());
    assert!(list_body.contains(r#""windows""#));
    assert!(list_body.contains(r#""window_id":"@1""#));
}

/// Verifies dispatches cancel and frame read methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_cancel_and_frame_read_methods() {
    let (mut session, primary) = test_session();

    let cancel = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/cancel","params":{"request_id":"missing"}}"#,
        &mut session,
        &primary,
    );
    assert!(cancel.contains(r#""cancel_requested":false"#));

    let invalid_cancel = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"control/cancel","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(invalid_cancel.contains(r#""error""#));
    assert!(invalid_cancel.contains("control/cancel requires request_id"));

    let frame = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"frame/read","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(frame.contains(r#""rendered""#));
    assert!(frame.contains(r#""session.id":"$1""#));
    assert!(frame.contains(r#""window.id":"@1""#));
    assert!(frame.contains(r#""window.title":"shell""#));
    assert!(frame.contains(r#""window.active":"true""#));
    assert!(frame.contains(r#""window.pane_count":"1""#));
    assert!(frame.contains(r#""layout.name":"tiled""#));
    assert!(frame.contains(r#""pane.id":"%1""#));
    assert!(frame.contains(r#""pane.active":"true""#));
    assert!(frame.contains(r#""pane.size":"80x24""#));
    assert!(frame.contains(r#""pane.mode":"normal""#));
    assert!(frame.contains(r#""agent.status":"idle""#));
    assert!(frame.contains(r#""observer.pending_count":"0""#));
    assert!(frame.contains(r#""result":{"fields":"#));
    assert!(!frame.contains(r#""frame""#));

    session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let targeted_frame = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"frame/read","params":{"target":{"pane_id":"%1"}}}"#,
        &mut session,
        &primary,
    );
    assert!(targeted_frame.contains(r#""pane.id":"%1""#));
    assert!(!targeted_frame.contains(r#""pane.id":"%2""#));
}

/// Verifies handles multiple framed control requests with idempotency cache.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn handles_multiple_framed_control_requests_with_idempotency_cache() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let first = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"same"}}"#,
    );
    let second = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"same"}}"#,
    );
    let mut request = initialize;
    request.extend_from_slice(&first);
    request.extend_from_slice(&second);
    let mut cache = ControlIdempotencyCache::default();

    let (responses, consumed) =
        handle_control_frames(&request, 4096, &mut session, &mut connection, &mut cache).unwrap();

    let (init_body, init_len) = decode_control_frame(&responses, 4096).unwrap();
    let (first_body, first_len) = decode_control_frame(&responses[init_len..], 4096).unwrap();
    let (second_body, _) = decode_control_frame(&responses[init_len + first_len..], 4096).unwrap();
    assert_eq!(consumed, request.len());
    assert!(init_body.contains(r#""granted_role":"primary""#));
    assert_eq!(first_body, second_body);
    assert_eq!(cache.len(), 1);
    assert_eq!(session.windows().len(), 2);
}

/// Verifies connection state requires initialize before session data.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn connection_state_requires_initialize_before_session_data() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let input =
        encode_control_body(r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{}}"#);

    let (output, _) =
        handle_control_frame(&input, 4096, &mut session, &mut connection, &mut cache).unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert!(body.contains(r#""error""#));
    assert!(body.contains(r#""code":-32002"#), "{body}");
    assert!(body.contains(r#""mezzanine_code":"forbidden""#), "{body}");
    assert!(body.contains("control/initialize"));
    assert!(!body.contains(r#""windows""#), "{body}");
    assert!(!body.contains(r#""panes""#), "{body}");
    assert!(!connection.initialized());
}

/// Verifies initialized connection rejects repeated initialize.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn initialized_connection_rejects_repeated_initialize() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#;
    let mut input = encode_control_body(initialize);
    input.extend_from_slice(&encode_control_body(initialize));

    let (output, _) =
        handle_control_frames(&input, 4096, &mut session, &mut connection, &mut cache).unwrap();
    let (init_body, first_len) = decode_control_frame(&output, 4096).unwrap();
    let (repeat_body, _) = decode_control_frame(&output[first_len..], 4096).unwrap();

    assert!(
        init_body.contains(r#""granted_role":"primary""#),
        "{init_body}"
    );
    assert!(repeat_body.contains(r#""error""#), "{repeat_body}");
    assert!(repeat_body.contains(r#""code":-32004"#), "{repeat_body}");
    assert!(
        repeat_body.contains(r#""mezzanine_code":"invalid_state""#),
        "{repeat_body}"
    );
}

/// Verifies connection initialize rejects unsupported protocol version.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn connection_initialize_rejects_unsupported_protocol_version() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let (output, _) = handle_control_frames_for_connection(
        &input,
        4096,
        &mut session,
        &mut connection,
        &mut cache,
    )
    .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert!(body.contains(r#""error""#));
    assert!(body.contains("unsupported control protocol version"));
    assert!(!connection.initialized());
}

/// Live connection initialization must honor `session_target` instead of
/// accepting a descriptor for some other session and binding it to the current
/// session implicitly.
#[test]
fn connection_initialize_validates_session_target_against_live_session() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let missing_target = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","session_target":{"name":"missing"},"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let (output, _) = handle_control_frames_for_connection(
        &missing_target,
        4096,
        &mut session,
        &mut connection,
        &mut cache,
    )
    .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert!(body.contains(r#""error""#), "{body}");
    assert!(body.contains(r#""mezzanine_code":"not_found""#), "{body}");
    assert!(body.contains("session target not found"), "{body}");
    assert!(!connection.initialized());

    let mut connection = ControlConnectionState::new(true, true);
    let matching_target = encode_control_body(
        r#"{"jsonrpc":"2.0","id":2,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","session_target":{"default":true},"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let (output, _) = handle_control_frames_for_connection(
        &matching_target,
        4096,
        &mut session,
        &mut connection,
        &mut cache,
    )
    .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    assert!(connection.initialized());
}

/// Verifies connection initialize binds primary caller for followup requests.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn connection_initialize_binds_primary_caller_for_followup_requests() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let mut input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    input.extend_from_slice(&encode_control_body(
        r#"{"jsonrpc":"2.0","id":2,"method":"window/list","params":{}}"#,
    ));
    input.extend_from_slice(&encode_control_body(
        r#"{"jsonrpc":"2.0","id":3,"method":"client/list","params":{}}"#,
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
    let (list_body, second_len) = decode_control_frame(&output[first_len..], 4096).unwrap();
    let (client_body, _) = decode_control_frame(&output[first_len + second_len..], 4096).unwrap();

    assert!(init_body.contains(r#""granted_role":"primary""#));
    assert!(init_body.contains(r#""session":{"id":"$1""#));
    assert!(init_body.contains(r#""window_count":1"#));
    assert!(init_body.contains(r#""has_primary":true"#));
    assert!(connection.caller_client_id().is_some());
    assert!(list_body.contains(r#""windows""#));
    assert!(
        client_body.contains(
            r#""descriptor":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}"#
        ),
        "{client_body}"
    );
}

/// Verifies pending observer connection gets no session data after initialize.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pending_observer_connection_gets_no_session_data_after_initialize() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let mut input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"observer","requested_version":1,"requested_role":"observer","client":{"name":"observer","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    input.extend_from_slice(&encode_control_body(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/get","params":{}}"#,
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
    let (session_body, _) = decode_control_frame(&output[first_len..], 4096).unwrap();

    assert!(init_body.contains(r#""granted_role":"pending_observer""#));
    assert!(init_body.contains(r#""session":null"#));
    assert!(
        init_body.contains(r#""observer_request":{"id":"o1""#),
        "{init_body}"
    );
    assert!(
        init_body.contains(r#""observer_request_id":"o1""#),
        "{init_body}"
    );
    assert!(init_body.contains(r#""client_id":"c2""#), "{init_body}");
    assert!(
        init_body.contains(
            r#""descriptor":{"name":"observer","interactive":false,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}"#
        ),
        "{init_body}"
    );
    assert!(!init_body.contains(r#""request_id":"o1""#), "{init_body}");
    assert!(session_body.contains(r#""error""#));
    assert!(!session_body.contains(r#""windows""#));
    assert_eq!(session.observers().len(), 1);
    assert_eq!(
        session.observers()[0]
            .descriptor_terminal
            .as_ref()
            .unwrap()
            .columns,
        100
    );
    assert_eq!(
        session.observers()[0]
            .descriptor_terminal
            .as_ref()
            .unwrap()
            .rows,
        40
    );
}
