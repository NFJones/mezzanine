//! Control initialize tests.

use super::*;

/// Verifies dispatches control initialize method.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_control_initialize_method() {
    let (mut session, primary) = test_session();
    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""granted_role":"primary""#));
    assert!(response.contains(r#""control/initialize""#));
    assert!(response.contains(r#""approval_pending":false"#));
    assert!(response.contains(r#""session":{"id":"$1""#), "{response}");
    assert!(response.contains(r#""window_count":1"#), "{response}");
    assert!(
        response.contains(r#""active_window_id":"@1""#),
        "{response}"
    );
    assert!(
        !response.contains(r#""session":{"id":"default""#),
        "{response}"
    );
}

/// Verifies dispatch control initialize rejects unsupported protocol version.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatch_control_initialize_rejects_unsupported_protocol_version() {
    let (mut session, primary) = test_session();
    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""error""#));
    assert!(response.contains("unsupported control protocol version"));
}

/// Verifies dispatch control initialize rejects missing required fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatch_control_initialize_rejects_missing_required_fields() {
    let (mut session, primary) = test_session();

    for body in [
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"requested_version":1,"requested_role":"primary"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_role":"primary"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1}}"#,
    ] {
        let response = dispatch_control_request(body, &mut session, &primary);
        assert!(response.contains(r#""error""#), "{response}");
        assert!(
            response.contains(r#""mezzanine_code":"invalid_params""#),
            "{response}"
        );
    }
}

/// Verifies dispatch control initialize rejects zero terminal dimensions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatch_control_initialize_rejects_zero_terminal_dimensions() {
    let (mut session, primary) = test_session();
    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":0,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""error""#));
    assert!(response.contains("dimensions must be non-zero"));
    assert!(response.contains(r#""mezzanine_code":"invalid_params""#));
}

/// Verifies that optional initialization fields from the public schema are
/// validated when present instead of being accepted as unchecked JSON.
#[test]
fn control_initialize_validates_client_version_and_session_target_fields() {
    let (mut session, primary) = test_session();

    let accepted = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client_version":"1.2.3","session_target":{"default":true,"extensions":{"vendor":true}},"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        accepted.contains(r#""granted_role":"primary""#),
        "{accepted}"
    );

    let bad_client_version = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client_version":2,"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        bad_client_version.contains(r#""mezzanine_code":"invalid_params""#),
        "{bad_client_version}"
    );
    assert!(bad_client_version.contains("client_version must be a non-empty string"));

    let string_target = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","session_target":"default","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        string_target.contains(r#""mezzanine_code":"invalid_params""#),
        "{string_target}"
    );
    assert!(string_target.contains("session_target must be an object or null"));

    let conflicting_target = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","session_target":{"default":true,"name":"default"},"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        conflicting_target.contains(r#""mezzanine_code":"invalid_params""#),
        "{conflicting_target}"
    );
    assert!(conflicting_target.contains("SessionTarget must use exactly one"));
}

/// Rich client descriptors are part of the public handshake schema, so the
/// parser must accept and validate each named optional descriptor field instead
/// of forcing clients to hide conforming metadata under `extensions`.
#[test]
fn control_initialize_accepts_rich_client_descriptor_fields() {
    let (mut session, primary) = test_session();
    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","version":"1.0.0","pid":1234,"host":"workstation","user":"tester","purpose":"foreground","requested_role":"primary","interactive":true,"stdio":{"stdin_is_tty":true,"stdout_is_tty":true,"stderr_is_tty":true,"controlling_tty":"/dev/pts/1","tty_device":"pts-1","extensions":{}},"metadata":{"vendor":"example"},"terminal":{"columns":80,"rows":24,"term":"xterm-256color","features":["mouse","bracketed_paste","truecolor"],"extensions":{}}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );

    assert!(
        response.contains(r#""granted_role":"primary""#),
        "{response}"
    );
}

/// Descriptor role metadata must not be allowed to contradict the enclosing
/// control method role, because later authorization and audit paths rely on a
/// single role claim for each initialized connection.
#[test]
fn control_initialize_rejects_descriptor_role_mismatch() {
    let (mut session, primary) = test_session();
    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","requested_role":"observer","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );

    assert!(
        response.contains(r#""mezzanine_code":"invalid_params""#),
        "{response}"
    );
    assert!(response.contains("requested_role must match"), "{response}");
}

/// Required descriptor fields remain required even while optional descriptor
/// metadata is accepted. This prevents malformed clients from silently falling
/// back to invented names or terminal profiles.
#[test]
fn control_initialize_rejects_incomplete_descriptors() {
    let (mut session, primary) = test_session();

    let missing_name = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );
    assert!(missing_name.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(missing_name.contains("client descriptor requires name"));

    let missing_term = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        &mut session,
        &primary,
    );
    assert!(missing_term.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(missing_term.contains("terminal descriptor requires term"));
}

/// Terminal feature metadata supplied through a conforming client descriptor
/// should survive attachment and state serialization, while descriptors without
/// feature claims keep the existing compact JSON shape.
#[test]
fn session_attach_preserves_terminal_descriptor_features() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let response = dispatch_session_attach_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/attach","params":{"role":"primary","client":{"name":"feature-client","requested_role":"primary","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color","features":["mouse","truecolor"]}},"idempotency_key":"attach-features"}}"#,
        &mut session,
    );

    assert!(
        response.contains(r#""approval_pending":false"#),
        "{response}"
    );
    assert!(
        response.contains(r#""features":["mouse","truecolor"]"#),
        "{response}"
    );
}

/// Verifies control initialize rejects unknown fields outside extensions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn control_initialize_rejects_unknown_fields_outside_extensions() {
    let (mut session, primary) = test_session();

    let unknown = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","unexpected":true,"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#,
        &mut session,
        &primary,
    );
    assert!(unknown.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(unknown.contains("unknown field"));

    let bad_extensions = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","extensions":1,"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#,
        &mut session,
        &primary,
    );
    assert!(bad_extensions.contains("extensions must be an object"));

    let accepted = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","extensions":{"vendor":true},"client":{"name":"primary","interactive":true,"extensions":{},"terminal":{"columns":80,"rows":24,"term":"xterm-256color","extensions":{}}},"authentication":{"mechanism":"peer_credentials","extensions":{}}}}"#,
        &mut session,
        &primary,
    );
    assert!(accepted.contains(r#""granted_role":"primary""#));
}
