//! Control connections tests.

use super::*;
use crate::control::AuthenticatedPeer;

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
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#;
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
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
    assert!(body.contains(r#""code":-32003"#), "{body}");
    assert!(
        body.contains(r#""mezzanine_code":"unsupported_version""#),
        "{body}"
    );
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
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","session_target":{"name":"missing"},"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
        r#"{"jsonrpc":"2.0","id":2,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","session_target":{"default":true},"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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
    assert!(init_body.contains(r#""attached_primary_count":1"#));
    assert!(!init_body.contains(r#""has_primary""#));
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
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"observer","requested_version":2,"requested_role":"observer","client":{"name":"observer","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
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

/// Verifies observer initialization owns one disconnect cleanup event.
///
/// Each observer request is created for one live control connection. EOF,
/// reset, and shutdown may race, so the connection must expose its observer
/// client exactly once for role-neutral runtime cleanup.
#[test]
fn observer_disconnect_client_is_taken_once() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"observer","requested_version":2,"requested_role":"observer","client":{"name":"observer","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    handle_control_frames_for_connection(&input, 4096, &mut session, &mut connection, &mut cache)
        .unwrap();

    let observer_client = session.observers()[0].client_id.clone();
    assert_eq!(
        connection.take_disconnect_client_id(),
        Some(observer_client)
    );
    assert!(connection.take_disconnect_client_id().is_none());
}

/// Verifies transport-authenticated identity cannot change on a live connection.
///
/// The transport peer is evidence supplied by the concrete adapter, so accepting
/// a different identity after request state exists would cross authorization
/// contexts even when the byte stream itself remains valid.
#[test]
fn authenticated_peer_binding_is_immutable() {
    let mut connection = ControlConnectionState::new(true, true);
    let unix_peer = AuthenticatedPeer::unix_user(1000);

    connection
        .bind_authenticated_peer(unix_peer.clone())
        .unwrap();
    connection
        .bind_authenticated_peer(unix_peer.clone())
        .unwrap();
    let error = connection
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint("endpoint-a"))
        .unwrap_err();

    assert_eq!(connection.authenticated_peer(), Some(&unix_peer));
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("cannot change"));
}

/// Verifies a primary disconnect can be consumed only once from connection state.
///
/// EOF, stream reset, and supervisor shutdown can race. The shared connection
/// boundary must therefore expose at most one detach event for the initialized
/// primary client.
#[test]
fn primary_disconnect_client_is_taken_once() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let (_, consumed) = handle_control_frames_for_connection(
        &input,
        4096,
        &mut session,
        &mut connection,
        &mut cache,
    )
    .unwrap();

    assert_eq!(consumed, input.len());
    assert!(connection.take_disconnect_client_id().is_some());
    assert!(connection.take_disconnect_client_id().is_none());
}

/// Verifies every primary initialization creates and owns a fresh exact client,
/// even when another attached primary has the same display name.
#[test]
fn same_named_primary_initialization_creates_independent_owned_client() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let existing_primary = session.attach_primary("primary", true).unwrap();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let (output, consumed) = handle_control_frames_for_connection(
        &input,
        4096,
        &mut session,
        &mut connection,
        &mut cache,
    )
    .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert_eq!(consumed, input.len());
    assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    assert_eq!(session.layout_owner_client_id(), Some(&existing_primary));
    let initialized_primary = connection.take_disconnect_client_id().unwrap();
    assert_ne!(initialized_primary, existing_primary);
    assert!(body.contains(&format!(r#""client":{{"id":"{initialized_primary}""#)));
    assert_eq!(session.attached_primaries().count(), 2);
}

/// Verifies an authorized Iroh principal receives a fresh exact primary rather
/// than reusing or taking over a same-named live primary.
#[test]
fn iroh_primary_with_same_display_name_gets_independent_client() {
    use crate::security::remote::{RemoteHostRoutingAuthority, RemotePrincipal, RemoteRoleCeiling};

    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let existing_primary = session.attach_primary("remote-cli", true).unwrap();
    let mut connection = ControlConnectionState::new(false, false);
    connection
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint("endpoint-b"))
        .unwrap();
    connection
        .bind_remote_principal(RemotePrincipal {
            trust_record_id: "trust-b".to_string(),
            endpoint_id: "endpoint-b".to_string(),
            role_ceiling: RemoteRoleCeiling::Primary,
            host_routing: RemoteHostRoutingAuthority::default(),
            requested_role: RequestedRole::Primary,
        })
        .unwrap();
    let mut cache = ControlIdempotencyCache::default();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"remote-cli","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"remote-cli","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#,
    );

    let (output, consumed) = handle_control_frames_for_connection(
        &input,
        4096,
        &mut session,
        &mut connection,
        &mut cache,
    )
    .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert_eq!(consumed, input.len());
    assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    assert!(connection.initialized());
    let remote_primary = connection.caller_client_id().unwrap();
    assert_ne!(remote_primary, &existing_primary);
    assert!(body.contains(&format!(r#""client":{{"id":"{remote_primary}""#)));
    assert_eq!(session.layout_owner_client_id(), Some(&existing_primary));
    assert_eq!(session.attached_primaries().count(), 2);
}
