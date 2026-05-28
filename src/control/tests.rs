//! Regression coverage for the control tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Control module tests.

use super::registry::control_method_spec;
use super::types::PRIMARY_CONTROL_METHODS;
use super::{
    AGENT_CONTROL_METHODS, AUTOMATION_CONTROL_METHODS, AgentShellStore, AgentTurnLedger,
    AgentTurnState, AuditLog, AuthenticationMaterial, Capabilities, ClientDescriptor, ConfigFormat,
    ConfigLayer, ConfigScope, ControlConnectionState, ControlIdempotencyCache, GrantedRole,
    InitializeContext, InitializeParams, OBSERVER_CONTROL_METHODS,
    PENDING_OBSERVER_CONTROL_METHODS, PaneCaptureSource, RequestedRole, TerminalDescriptor,
    decode_control_frame, dispatch_control_request, dispatch_control_request_cached,
    dispatch_control_request_for_client, dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_events, dispatch_control_request_for_connection,
    dispatch_control_request_with_approvals, dispatch_control_request_with_approvals_and_audit,
    dispatch_control_request_with_captures, dispatch_control_request_with_mcp,
    dispatch_control_request_with_snapshots, dispatch_project_trust_request,
    dispatch_session_attach_request, encode_control_body, handle_control_frame,
    handle_control_frames, handle_control_frames_for_connection, initialize,
    initialize_result_json, json_escape, parse_json_rpc_request,
};
use crate::event::{EventKind, EventLog, EventVisibility};
use crate::ids::ClientId;
use crate::layout::SplitDirection;
use crate::layout::{LayoutPolicy, Size};
use crate::mcp::McpRegistry;
use crate::mcp::{McpServerConfig, McpToolEffects, McpToolState};
use crate::permissions::{
    BlockedApprovalQueue, BlockedApprovalRequest, BlockedApprovalState, builtin_rules,
};
use crate::project::ProjectTrustStore;
use crate::session::Session;
use crate::shell::{ResolvedShell, ShellSource};
use crate::snapshot::{
    PaneSnapshotPayload, SessionSnapshotPayload, SnapshotFrameState, SnapshotPaneGeometry,
    SnapshotRepository, SnapshotSessionState, SnapshotShellMetadata, WindowSnapshotPayload,
};
use crate::terminal::{DEFAULT_HISTORY_LIMIT, DEFAULT_PANE_TERM};
use crate::terminal::{
    GraphicRendition, TerminalColor, TerminalModeState, TerminalSavedState, TerminalStyleSpan,
};
use crate::test_support::control::JsonRpcRequestBuilder;
use crate::test_support::runtime::SessionFixture;
use crate::test_support::temp::TestTempDir;
use std::fs;
use std::path::PathBuf;

/// Runs the primary params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn primary_params() -> InitializeParams {
    InitializeParams {
        client_name: "test".to_string(),
        requested_version: 1,
        requested_role: RequestedRole::Primary,
        client_version: None,
        session_target_json: None,
        detach_primary_on_disconnect: false,
        client: Some(ClientDescriptor {
            name: "test".to_string(),
            version: None,
            pid: None,
            host: None,
            user: None,
            purpose: None,
            requested_role: None,
            interactive: true,
            stdio: None,
            metadata_json: None,
            terminal: Some(TerminalDescriptor {
                columns: 80,
                rows: 24,
                term: "xterm-256color".to_string(),
                features: Vec::new(),
            }),
        }),
        authentication: Some(AuthenticationMaterial::peer_credentials()),
    }
}

/// Runs the test session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_session() -> (Session, ClientId) {
    SessionFixture::new().build_with_primary()
}

/// Runs the temp root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn temp_root(name: &str) -> TestTempDir {
    TestTempDir::new(&format!("control-test-{name}"))
}

/// Verifies none authentication gets no session data.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn none_authentication_gets_no_session_data() {
    let result = initialize(
        InitializeParams {
            client_name: "observer".to_string(),
            requested_version: 1,
            requested_role: RequestedRole::Observer,
            client_version: None,
            session_target_json: None,
            detach_primary_on_disconnect: false,
            client: None,
            authentication: Some(AuthenticationMaterial::none()),
        },
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: false,
        },
    )
    .unwrap();

    assert_eq!(result.session, None);
    assert_eq!(result.capabilities, Capabilities::unauthenticated());
}

/// Verifies pending observer receives only request local status.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pending_observer_receives_only_request_local_status() {
    let result = initialize(
        InitializeParams {
            client_name: "observer".to_string(),
            requested_version: 1,
            requested_role: RequestedRole::Observer,
            client_version: None,
            session_target_json: None,
            detach_primary_on_disconnect: false,
            client: None,
            authentication: Some(AuthenticationMaterial::peer_credentials()),
        },
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: false,
        },
    )
    .unwrap();

    assert_eq!(result.granted_role, GrantedRole::PendingObserver);
    assert_eq!(result.session, None);
    assert!(result.observer_request.is_some());
    assert!(result.approval_pending);
    assert!(!result.capabilities.methods.contains(&"event/list"));
    assert!(result.capabilities.methods.contains(&"observer/inspect"));
    assert!(!result.capabilities.features.event_replay);
    assert!(!result.capabilities.features.mcp);
    assert!(!result.capabilities.features.snapshots);
}

/// Verifies primary requires trusted interactive terminal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_requires_trusted_interactive_terminal() {
    let error = initialize(
        primary_params(),
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: false,
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies primary initializes when authenticated and interactive.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_initializes_when_authenticated_and_interactive() {
    let result = initialize(
        primary_params(),
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: true,
        },
    )
    .unwrap();

    assert_eq!(result.granted_role, GrantedRole::Primary);
    assert!(result.session.is_some());
    assert_eq!(result.server.implementation_name, "mezzanine");
    assert_eq!(result.server.protocol_versions, vec![1]);
    assert!(result.server.started_at.ends_with('Z'));
}

/// Verifies that a bearer token in the JSON payload does not authenticate a
/// caller by itself. Until control auth is wired to a configured token file or
/// equivalent validator, accepting any non-empty bearer token would grant
/// session data and primary authority without proof.
#[test]
fn bearer_token_payload_without_validator_gets_no_session_data() {
    let mut params = primary_params();
    params.authentication = Some(AuthenticationMaterial::bearer(
        "unguessable-but-unvalidated",
    ));

    let result = initialize(
        params,
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: true,
        },
    )
    .unwrap();

    assert_eq!(result.session, None);
    assert_eq!(result.capabilities, Capabilities::unauthenticated());
}

/// Verifies initialize rejects unsupported protocol version.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn initialize_rejects_unsupported_protocol_version() {
    let mut params = primary_params();
    params.requested_version = 2;

    let error = initialize(
        params,
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: true,
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error
            .message()
            .contains("unsupported control protocol version")
    );
}

/// Verifies primary capabilities include config control surface.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_capabilities_include_config_control_surface() {
    let capabilities = Capabilities::primary();

    for method in [
        "config/validate",
        "config/get",
        "config/set",
        "config/unset",
        "config/reload",
        "project/trust/list",
        "project/trust/inspect",
        "project/trust/decide",
        "project/trust/revoke",
        "mcp/retry",
    ] {
        assert!(
            capabilities.methods.contains(&method),
            "{method} missing from primary capabilities"
        );
    }
}

/// Verifies pending observer capabilities exclude session and terminal view methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pending_observer_capabilities_exclude_session_and_terminal_view_methods() {
    let capabilities = Capabilities::pending_observer();

    for method in ["session/get", "terminal/view", "event/list", "mcp/list"] {
        assert!(
            !capabilities.methods.contains(&method),
            "{method} must not be exposed to pending observers"
        );
    }
    for method in [
        "control/initialize",
        "session/attach",
        "observer/inspect",
        "control/cancel",
        "control/shutdown",
    ] {
        assert!(
            capabilities.methods.contains(&method),
            "{method} missing from pending observer capabilities"
        );
    }
}

/// Restricted-role capability advertisements must use the same method lists as
/// role authorization so clients can rely on initialization results to plan the
/// requests that will be accepted before method-specific parameter checks.
#[test]
fn restricted_role_capabilities_match_authorization_method_sets() {
    assert_eq!(
        Capabilities::pending_observer().methods,
        PENDING_OBSERVER_CONTROL_METHODS
    );
    assert_eq!(Capabilities::observer().methods, OBSERVER_CONTROL_METHODS);
    assert_eq!(Capabilities::agent().methods, AGENT_CONTROL_METHODS);
    assert_eq!(
        Capabilities::automation().methods,
        AUTOMATION_CONTROL_METHODS
    );

    for method in ["control/shutdown", "control/cancel", "event/list"] {
        assert!(
            Capabilities::agent().methods.contains(&method),
            "{method} missing from agent capabilities"
        );
        assert!(
            Capabilities::automation().methods.contains(&method),
            "{method} missing from automation capabilities"
        );
    }
}

/// Verifies initialize json includes required server and capability schema.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn initialize_json_includes_required_server_and_capability_schema() {
    let result = initialize(
        primary_params(),
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: true,
        },
    )
    .unwrap();

    let json = initialize_result_json(&result);

    assert!(json.contains(r#""server":{"id":"mez-"#));
    assert!(json.contains(r#""implementation_name":"mezzanine""#));
    assert!(json.contains(r#""protocol_versions":[1]"#));
    assert!(json.contains(r#""session":{"id":"default""#));
    assert!(json.contains(r#""window_count":0"#));
    assert!(json.contains(r#""protocol_version":1"#));
    assert!(json.contains(r#""event_types":["client_attached""#));
    assert!(json.contains(r#""mcp_server_changed""#));
    assert!(json.contains(r#""roles":["primary","pending_observer""#));
    assert!(json.contains(r#""transports":["unix"]"#));
    assert!(json.contains(r#""max_frame_size":"#) || json.contains(r#""max_frame_size":1048576"#));
    assert!(json.contains(r#""approval_bypass":true"#));
}

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

/// Verifies baseline control methods reject unknown params outside extensions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn baseline_control_methods_reject_unknown_params_outside_extensions() {
    let (mut session, primary) = test_session();

    let unknown = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"a","surprise":true}}"#,
        &mut session,
        &primary,
    );
    assert!(unknown.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(unknown.contains("window/create params contains unknown field"));

    let bad_extensions = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"b","extensions":1}}"#,
        &mut session,
        &primary,
    );
    assert!(bad_extensions.contains("extensions must be an object"));

    let accepted = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"c","extensions":{"vendor":true}}}"#,
        &mut session,
        &primary,
    );
    assert!(accepted.contains(r#""window":"#));
}

/// Verifies that advertised primary control methods are backed by the shared
/// method registry that now owns dispatch and schema metadata. This prevents a
/// future control method from being added to capabilities without a matching
/// dispatch/schema entry.
#[test]
fn advertised_primary_control_methods_have_registry_entries() {
    for method in PRIMARY_CONTROL_METHODS {
        assert!(
            control_method_spec(method).is_some(),
            "{method} is advertised but missing from the control method registry"
        );
    }
}

/// Verifies that approval control methods use the same unknown-parameter schema
/// enforcement as the main control dispatcher. Approval handling has its own
/// specialized path because it needs access to the blocked-approval queue, so
/// this regression keeps that path from silently ignoring extra request fields.
#[test]
fn approval_control_methods_reject_unknown_params_on_specialized_dispatch() {
    let (mut session, primary) = test_session();
    let mut queue = BlockedApprovalQueue::default();

    let list = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{"surprise":true}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(list.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(list.contains("approval/list params contains unknown field"));

    let decide = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":2,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","idempotency_key":"decide","surprise":true}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(decide.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(decide.contains("approval/decide params contains unknown field"));

    let invalid_scope = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":3,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","scope":{"persistence":"session","surprise":true},"idempotency_key":"bad-scope"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(invalid_scope.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_scope.contains("approval/decide scope contains unknown field"));

    let invalid_prefix = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":4,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","scope":{"persistence":"session","command_prefix":"git diff"},"idempotency_key":"bad-prefix"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(invalid_prefix.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_prefix.contains("command_prefix must be an array"));

    let invalid_digest = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":5,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","scope":{"persistence":"session","exact_sha256":"not-a-digest"},"idempotency_key":"bad-digest"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(invalid_digest.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_digest.contains("exact_sha256 must be a 64-character hexadecimal digest"));
}

/// Verifies that snapshot control methods validate their parameter schema before
/// repository operations. Snapshot dispatch is specialized so it can receive a
/// repository and capture context, but it still exposes JSON-RPC methods whose
/// request objects must reject unknown fields outside `extensions`.
#[test]
fn snapshot_control_methods_reject_unknown_params_on_specialized_dispatch() {
    let (mut session, primary) = test_session();
    let root = temp_root("snapshot-unknown-params");
    let snapshots = SnapshotRepository::new(root.to_path_buf());

    let response = dispatch_control_request_with_snapshots(
        &JsonRpcRequestBuilder::method("snapshot/list")
            .params_json(r#"{"surprise":true}"#)
            .build(),
        &mut session,
        &primary,
        &snapshots,
    );

    assert!(response.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(response.contains("snapshot/list params contains unknown field"));

    let null_target = dispatch_control_request_with_snapshots(
        &JsonRpcRequestBuilder::method("snapshot/list")
            .id(2)
            .params_json(r#"{"target":null}"#)
            .build(),
        &mut session,
        &primary,
        &snapshots,
    );
    assert!(null_target.contains(r#""snapshots":[]"#), "{null_target}");

    let invalid_target = dispatch_control_request_with_snapshots(
        r#"{"jsonrpc":"2.0","id":3,"method":"snapshot/list","params":{"target":"default"}}"#,
        &mut session,
        &primary,
        &snapshots,
    );
    assert!(
        invalid_target.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_target}"
    );

    let missing_target = dispatch_control_request_with_snapshots(
        r#"{"jsonrpc":"2.0","id":4,"method":"snapshot/list","params":{"target":{"session_id":"missing"}}}"#,
        &mut session,
        &primary,
        &snapshots,
    );
    assert!(
        missing_target.contains(r#""mezzanine_code":"not_found""#),
        "{missing_target}"
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies generic control dispatches empty snapshot state without repository.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_dispatches_empty_snapshot_state_without_repository() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""snapshots":[]"#), "{list}");

    let null_target_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":10,"method":"snapshot/list","params":{"target":null}}"#,
        &mut session,
        &primary,
    );
    assert!(
        null_target_list.contains(r#""snapshots":[]"#),
        "{null_target_list}"
    );

    let invalid_target_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":11,"method":"snapshot/list","params":{"target":"default"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid_target_list.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_target_list}"
    );

    let create = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"snapshot/create","params":{"target":{"default":true},"name":"manual","idempotency_key":"snapshot-create"}}"#,
        &mut session,
        &primary,
    );
    assert!(create.contains(r#""error""#), "{create}");
    assert!(create.contains(r#""code":-32004"#), "{create}");
    assert!(
        create.contains(r#""mezzanine_code":"invalid_state""#),
        "{create}"
    );
    assert!(
        create.contains("snapshot repository is not configured"),
        "{create}"
    );

    let resume_missing_id = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"snapshot/resume","params":{"idempotency_key":"snapshot-resume"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        resume_missing_id.contains("snapshot/resume requires snapshot_id"),
        "{resume_missing_id}"
    );
}

/// Verifies control frame round trips raw json body.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn control_frame_round_trips_raw_json_body() {
    let encoded = encode_control_body(r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize"}"#);

    let (body, consumed) = decode_control_frame(&encoded, 4096).unwrap();

    assert_eq!(
        body,
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize"}"#
    );
    assert_eq!(consumed, encoded.len());
}

/// Verifies parses json rpc request envelope.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_json_rpc_request_envelope() {
    let request = parse_json_rpc_request(
        r#"{"jsonrpc":"2.0","id":"abc","method":"session/get","params":{}}"#,
    )
    .unwrap();

    assert_eq!(request.id, r#""abc""#);
    assert_eq!(request.method, "session/get");
    assert_eq!(request.params.as_deref(), Some("{}"));
}

/// Verifies json rpc parser uses top level fields and requires object params.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn json_rpc_parser_uses_top_level_fields_and_requires_object_params() {
    let request = parse_json_rpc_request(
        r#"{"jsonrpc":"2.0","id":7,"method":"session/get","params":{"method":"nested/ignored"}}"#,
    )
    .unwrap();
    assert_eq!(request.id, "7");
    assert_eq!(request.method, "session/get");
    assert_eq!(
        request.params.as_deref(),
        Some(r#"{"method":"nested/ignored"}"#)
    );

    let error =
        parse_json_rpc_request(r#"{"jsonrpc":"2.0","id":8,"method":"session/get","params":[]}"#)
            .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

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
    let mut session = Session::from_snapshot_payload(shell, &payload).unwrap();
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

/// Verifies dispatches agent read and shell visibility methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_agent_read_and_shell_visibility_methods() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"agent/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""agents""#));
    assert!(list.contains(r#""pane_id":"%1""#));

    let targeted_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":10,"method":"agent/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
    );
    assert!(targeted_list.contains(r#""agents""#), "{targeted_list}");

    let missing_session_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":11,"method":"agent/list","params":{"target":{"session_id":"missing"}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        missing_session_list.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session_list}"
    );

    let tasks = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"agent/task/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(tasks.contains(r#""tasks":[]"#));

    let targeted_tasks = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":12,"method":"agent/task/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
    );
    assert!(targeted_tasks.contains(r#""tasks":[]"#), "{targeted_tasks}");

    let conflicting_agent_target = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":13,"method":"agent/task/list","params":{"target":{"agent_id":"agent-%1","pane_id":"%1"}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        conflicting_agent_target.contains(r#""mezzanine_code":"invalid_params""#),
        "{conflicting_agent_target}"
    );

    let show = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &mut session,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#));

    let hide = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &mut session,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#));

    let command = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":5,"method":"agent/shell/command","params":{"idempotency_key":"agent-command","input":"summarize"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        command.contains(r#""kind":"requires_runtime""#),
        "{command}"
    );
    assert!(command.contains(r#""command":"prompt""#), "{command}");
    assert!(command.contains(r#""turn":null"#), "{command}");

    let command_alias = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":14,"method":"agent/shell/command","params":{"idempotency_key":"agent-command-alias","command":"summarize"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        command_alias.contains(r#""mezzanine_code":"invalid_params""#),
        "{command_alias}"
    );
    assert!(
        command_alias.contains("agent/shell/command params contains unknown field `command`"),
        "{command_alias}"
    );
}

/// Verifies generic control reports runtime required methods as invalid state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_reports_runtime_required_methods_as_invalid_state() {
    let (mut session, primary) = test_session();

    let terminal_view = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        terminal_view.contains(r#""mezzanine_code":"invalid_state""#),
        "{terminal_view}"
    );
    assert!(
        terminal_view.contains("terminal runtime is not attached"),
        "{terminal_view}"
    );
    assert!(
        !terminal_view.contains(r#""mezzanine_code":"method_not_found""#),
        "{terminal_view}"
    );

    let terminal_step = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"terminal/step","params":{"idempotency_key":"terminal-step","input_bytes":[],"render":false}}"#,
        &mut session,
        &primary,
    );
    assert!(
        terminal_step.contains(r#""mezzanine_code":"invalid_state""#),
        "{terminal_step}"
    );

    let terminal_command = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"terminal/command","params":{"idempotency_key":"terminal-command","input":"list-windows"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        terminal_command.contains(r#""mezzanine_code":"invalid_state""#),
        "{terminal_command}"
    );

    let missing_input = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"terminal/command","params":{"idempotency_key":"terminal-missing"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        missing_input.contains("terminal/command requires input"),
        "{missing_input}"
    );

    let command_alias = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":14,"method":"terminal/command","params":{"idempotency_key":"terminal-command-alias","command":"list-windows"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        command_alias.contains(r#""mezzanine_code":"invalid_params""#),
        "{command_alias}"
    );
    assert!(
        command_alias.contains("terminal/command params contains unknown field `command`"),
        "{command_alias}"
    );

    let spawn = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":5,"method":"agent/spawn","params":{"parent_agent":{"agent_id":"agent-%1"},"placement":{"mode":"new-pane"},"role":"explorer","cooperation_mode":"explore-only","read_scopes":["src"],"write_scopes":[],"prompt":"inspect","idempotency_key":"spawn"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        spawn.contains(r#""mezzanine_code":"invalid_state""#),
        "{spawn}"
    );
    assert!(spawn.contains("agent runtime is not attached"), "{spawn}");

    let invalid_spawn = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":6,"method":"agent/spawn","params":{"idempotency_key":"spawn-bad","unexpected":true}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid_spawn.contains("agent/spawn params contains unknown field"),
        "{invalid_spawn}"
    );
}

/// Runs the primary initialize request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn primary_initialize_request() -> &'static str {
    r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#
}

/// Runs the primary control method fixture request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn primary_control_method_fixture_request(
    method: &str,
    session: &mut Session,
    primary: &ClientId,
) -> String {
    match method {
        "control/initialize" => primary_initialize_request().to_string(),
        "control/cancel" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"control/cancel","params":{"request_id":"missing"}}"#
                .to_string()
        }
        "control/shutdown" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"control/shutdown","params":{}}"#.to_string()
        }
        "session/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/list","params":{}}"#.to_string()
        }
        "session/get" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{}}"#.to_string()
        }
        "session/rename" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/rename","params":{"name":"work","idempotency_key":"session-rename"}}"#.to_string()
        }
        "session/kill" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/kill","params":{"force":true,"idempotency_key":"session-kill"}}"#.to_string()
        }
        "session/attach" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/attach","params":{"role":"observer","client":{"name":"observer","requested_role":"observer","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"idempotency_key":"session-attach"}}"#.to_string()
        }
        "client/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"client/list","params":{}}"#.to_string()
        }
        "client/detach" => format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"client/detach","params":{{"client_id":"{}","idempotency_key":"client-detach"}}}}"#,
            primary
        ),
        "client/select_primary" => format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"client/select_primary","params":{{"client_id":"{}","idempotency_key":"client-select-primary"}}}}"#,
            primary
        ),
        "observer/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/list","params":{}}"#.to_string()
        }
        "observer/inspect" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/inspect","params":{"observer_request_id":"missing-observer"}}"#.to_string()
        }
        "observer/approve" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/approve","params":{"observer_request_id":"missing-observer","idempotency_key":"observer-approve"}}"#.to_string()
        }
        "observer/reject" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/reject","params":{"observer_request_id":"missing-observer","reason":"not now","idempotency_key":"observer-reject"}}"#.to_string()
        }
        "observer/revoke" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/revoke","params":{"client_id":"missing-client","reason":"done","idempotency_key":"observer-revoke"}}"#.to_string()
        }
        "window/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/list","params":{}}"#.to_string()
        }
        "window/create" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"window-create"}}"#.to_string()
        }
        "window/rename" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/rename","params":{"target":{"window_id":"@1"},"name":"renamed","idempotency_key":"window-rename"}}"#.to_string()
        }
        "window/select" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/select","params":{"target":{"window_id":"@1"},"idempotency_key":"window-select"}}"#.to_string()
        }
        "window/close" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/close","params":{"target":{"window_id":"@1"},"force":true,"idempotency_key":"window-close"}}"#.to_string()
        }
        "pane/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/list","params":{}}"#.to_string()
        }
        "pane/create" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/create","params":{"split":"vertical","select":true,"idempotency_key":"pane-create"}}"#.to_string()
        }
        "pane/select" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/select","params":{"target":{"pane_id":"%1"},"idempotency_key":"pane-select"}}"#.to_string()
        }
        "pane/resize" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/resize","params":{"target":{"pane_id":"%1"},"size":{"mode":"cells","columns":100,"rows":30},"idempotency_key":"pane-resize"}}"#.to_string()
        }
        "pane/move" | "pane/swap" | "pane/join" => {
            session
                .split_active_pane(primary, SplitDirection::Vertical)
                .unwrap();
            match method {
                "pane/move" => {
                    r#"{"jsonrpc":"2.0","id":1,"method":"pane/move","params":{"source":{"pane_id":"%1"},"destination":{"pane_id":"%2"},"position":"horizontal","idempotency_key":"pane-move"}}"#.to_string()
                }
                "pane/swap" => {
                    r#"{"jsonrpc":"2.0","id":1,"method":"pane/swap","params":{"source":{"pane_id":"%1"},"destination":{"pane_id":"%2"},"idempotency_key":"pane-swap"}}"#.to_string()
                }
                _ => {
                    r#"{"jsonrpc":"2.0","id":1,"method":"pane/join","params":{"source":{"pane_id":"%1"},"destination":{"pane_id":"%2"},"position":"vertical","idempotency_key":"pane-join"}}"#.to_string()
                }
            }
        }
        "pane/break" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/break","params":{"target":{"pane_id":"%1"},"name":"broken","idempotency_key":"pane-break"}}"#.to_string()
        }
        "pane/close" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/close","params":{"target":{"pane_id":"%1"},"force":true,"idempotency_key":"pane-close"}}"#.to_string()
        }
        "pane/capture" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{"target":{"pane_id":"%1"},"range":{"origin":"visible","start":"start","end":"end"}}}"#.to_string()
        }
        "terminal/step" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"terminal/step","params":{"input_bytes":[],"render":false,"idempotency_key":"terminal-step"}}"#.to_string()
        }
        "terminal/view" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#.to_string()
        }
        "terminal/command" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"terminal/command","params":{"input":"list-windows","idempotency_key":"terminal-command"}}"#.to_string()
        }
        "frame/read" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"frame/read","params":{}}"#.to_string()
        }
        "agent/shell/show" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"agent-show"}}"#.to_string()
        }
        "agent/shell/hide" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"agent-hide"}}"#.to_string()
        }
        "agent/shell/command" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/command","params":{"input":"summarize","idempotency_key":"agent-command"}}"#.to_string()
        }
        "agent/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/list","params":{}}"#.to_string()
        }
        "agent/task/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/task/list","params":{}}"#.to_string()
        }
        "agent/spawn" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/spawn","params":{"parent_agent":{"agent_id":"agent-%1"},"placement":{"mode":"new-pane"},"role":"explorer","cooperation_mode":"explore-only","read_scopes":["src"],"write_scopes":[],"prompt":"inspect","idempotency_key":"agent-spawn"}}"#.to_string()
        }
        "event/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"event/list","params":{}}"#.to_string()
        }
        "config/validate" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/validate","params":{}}"#.to_string()
        }
        "config/get" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","effective":true}}"#.to_string()
        }
        "config/set" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/set","params":{"path":"history.lines","value":2048,"idempotency_key":"config-set"}}"#.to_string()
        }
        "config/unset" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/unset","params":{"path":"history.persist","idempotency_key":"config-unset"}}"#.to_string()
        }
        "config/reload" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/reload","params":{"idempotency_key":"config-reload"}}"#.to_string()
        }
        "project/trust/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/list","params":{}}"#.to_string()
        }
        "project/trust/inspect" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/inspect","params":{"project_root":"/tmp/missing-project"}}"#.to_string()
        }
        "project/trust/decide" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/decide","params":{"project_root":"/tmp/project","decision":"trust","idempotency_key":"project-trust"}}"#.to_string()
        }
        "project/trust/revoke" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/revoke","params":{"project_root":"/tmp/project","idempotency_key":"project-revoke"}}"#.to_string()
        }
        "mcp/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"mcp/list","params":{}}"#.to_string()
        }
        "mcp/retry" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"mcp/retry","params":{"server_id":"missing","idempotency_key":"mcp-retry"}}"#.to_string()
        }
        "approval/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{}}"#.to_string()
        }
        "approval/decide" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"approval/decide","params":{"approval_id":"missing-approval","decision":"approve","idempotency_key":"approval-decision"}}"#.to_string()
        }
        "snapshot/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/list","params":{}}"#.to_string()
        }
        "snapshot/create" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/create","params":{"target":{"default":true},"name":"manual","idempotency_key":"snapshot-create"}}"#.to_string()
        }
        "snapshot/resume" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/resume","params":{"snapshot_id":"missing-snapshot","idempotency_key":"snapshot-resume"}}"#.to_string()
        }
        "snapshot/delete" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/delete","params":{"snapshot_id":"missing-snapshot","idempotency_key":"snapshot-delete"}}"#.to_string()
        }
        unknown => panic!("missing primary control fixture for {unknown}"),
    }
}

/// Verifies that the advertised primary control capability set is executable
/// through the initialized connection boundary. The test does not require every
/// method to succeed without its live runtime or repository dependency; it locks
/// in that each baseline method reaches its method-specific implementation and
/// reports a structured result or domain error rather than `method_not_found`.
#[test]
fn advertised_primary_methods_do_not_fall_through_as_unknown_after_initialize() {
    for method in PRIMARY_CONTROL_METHODS {
        let (mut session, primary) = test_session();
        let mut connection = ControlConnectionState::new(true, true);
        let mut cache = ControlIdempotencyCache::default();
        let request = primary_control_method_fixture_request(method, &mut session, &primary);
        let response = if *method == "control/initialize" {
            dispatch_control_request_for_connection(
                &request,
                &mut session,
                &mut connection,
                &mut cache,
            )
        } else {
            let initialize = dispatch_control_request_for_connection(
                primary_initialize_request(),
                &mut session,
                &mut connection,
                &mut cache,
            );
            assert!(
                initialize.contains(r#""granted_role":"primary""#),
                "{method} initialize response: {initialize}"
            );
            dispatch_control_request_for_connection(
                &request,
                &mut session,
                &mut connection,
                &mut cache,
            )
        };

        assert!(
            !response.contains(r#""mezzanine_code":"method_not_found""#),
            "{method} returned method_not_found: {response}"
        );
        assert!(
            !response.contains("is not implemented"),
            "{method} returned implementation placeholder: {response}"
        );
    }
}

/// Verifies agent state dispatch persists visibility and lists turns.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn agent_state_dispatch_persists_visibility_and_lists_turns() {
    let (mut session, primary) = test_session();
    let mut store = AgentShellStore::default();
    let mut ledger = AgentTurnLedger::new(false);

    let show = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let conversation_id = store.get("%1").unwrap().session_id.clone();
    assert!(
        show.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{show}"
    );

    store.start_turn("%1", "turn-1").unwrap();
    ledger
        .start_turn(crate::agent::AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 42,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,
        })
        .unwrap();

    let list = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":2,"method":"agent/list","params":{}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(list.contains(r#""visible":true"#), "{list}");
    assert!(list.contains(r#""status":"running""#), "{list}");
    assert!(list.contains(r#""last_turn_id":"turn-1""#), "{list}");

    let targeted_list = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":20,"method":"agent/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        targeted_list.contains(r#""visible":true"#),
        "{targeted_list}"
    );

    let missing_session_list = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":21,"method":"agent/list","params":{"target":{"name":"elsewhere"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        missing_session_list.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session_list}"
    );

    let command = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":30,"method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(command.contains(r#""kind":"display""#), "{command}");
    assert!(command.contains(r#""command":"status""#), "{command}");
    assert!(command.contains(r#""turn":null"#), "{command}");

    let command_alias = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":33,"method":"agent/shell/command","params":{"idempotency_key":"agent-alias","command":"/status"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        command_alias.contains(r#""mezzanine_code":"invalid_params""#),
        "{command_alias}"
    );
    assert!(
        command_alias.contains("agent/shell/command params contains unknown field `command`"),
        "{command_alias}"
    );

    let tasks = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":3,"method":"agent/task/list","params":{"target":{"agent_id":"agent-%1"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""agent_id":"agent-%1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");
    assert!(
        tasks.contains(r#""created_at":"1970-01-01T00:00:42Z""#),
        "{tasks}"
    );
    assert!(
        tasks.contains(r#""started_at":"1970-01-01T00:00:42Z""#),
        "{tasks}"
    );
    assert!(!tasks.contains("unix:"), "{tasks}");
    assert!(
        tasks.contains(r#""prompt_preview":"user prompt""#),
        "{tasks}"
    );

    let session_tasks = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":31,"method":"agent/task/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        session_tasks.contains(r#""id":"turn-1""#),
        "{session_tasks}"
    );

    let missing_pane_tasks = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":32,"method":"agent/task/list","params":{"target":{"pane_id":"missing"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        missing_pane_tasks.contains(r#""mezzanine_code":"not_found""#),
        "{missing_pane_tasks}"
    );

    let hide = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":4,"method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");
    assert!(hide.contains(r#""status":"running""#), "{hide}");
}

/// Verifies that pane/capture returns plain text content and any supplied
/// history and visible-line style spans over the same requested range.
#[test]
fn pane_capture_uses_supplied_visible_and_history_source() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec!["visible one".to_string(), "visible two".to_string()],
        visible_line_style_spans: vec![
            vec![TerminalStyleSpan {
                start: 0,
                length: 7,
                rendition: GraphicRendition {
                    bold: true,
                    dim: false,
                    italic: false,
                    strikethrough: false,
                    double_underline: false,
                    hidden: false,
                    underline: false,
                    inverse: false,
                    foreground: Some(TerminalColor::Indexed(2)),
                    background: None,
                },
            }],
            Vec::new(),
        ],
        history_lines: vec!["history".to_string()],
        history_line_style_spans: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 7,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: true,
                inverse: false,
                foreground: None,
                background: Some(TerminalColor::Indexed(4)),
            },
        }]],
        alternate_screen_active: false,
        truncated: false,
        primary_pid: Some(1234),
        process_state: Some("running".to_string()),
        exit_status: None,
    }];
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"include_history":true,"range":{{"origin":"combined","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );

    let response =
        dispatch_control_request_with_captures(&request, &mut session, &primary, &captures);

    assert!(response.contains("history\\nvisible one\\nvisible two"));
    assert!(response.contains(r#""range":{"origin":"combined","start":0,"end":3}"#));
    assert!(response.contains(r#""line_style_spans":[[{"start":0,"length":7"#));
    assert!(response.contains(r#""foreground":{"kind":"indexed","index":2}"#));
    assert!(response.contains(r#""background":{"kind":"indexed","index":4}"#));
    assert!(response.contains(r#""source_available":true"#));
    assert!(response.contains(r#""primary_pid":1234"#));
}

/// Verifies that pane/capture includes a supplied normalized exit-status
/// object in the embedded PaneState. Capture sources are used for restored or
/// otherwise non-live pane views, so this path must not collapse a known
/// process status back to `null`.
#[test]
fn pane_capture_embeds_supplied_exit_status() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    session.set_pane_live_state(&pane_id, false).unwrap();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec!["exited".to_string()],
        visible_line_style_spans: vec![Vec::new()],
        history_lines: Vec::new(),
        history_line_style_spans: Vec::new(),
        alternate_screen_active: false,
        truncated: false,
        primary_pid: None,
        process_state: Some("exited".to_string()),
        exit_status: Some(crate::process::PaneExitStatus {
            code: Some(7),
            signal: None,
            success: false,
        }),
    }];
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"visible","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );

    let response =
        dispatch_control_request_with_captures(&request, &mut session, &primary, &captures);

    assert!(
        response.contains(r#""process_state":"exited""#),
        "{response}"
    );
    assert!(
        response.contains(r#""exit_status":{"code":7,"signal":null,"success":false}"#),
        "{response}"
    );
}

/// This regression test verifies that pane/capture treats the CaptureRange
/// object as the source selector and slice window, rather than returning
/// the full captured buffer. It covers visible, history, and combined
/// origins because each origin builds its line vector differently before
/// applying start/end bounds.
#[test]
fn pane_capture_applies_visible_history_and_combined_ranges() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec![
            "visible zero".to_string(),
            "visible one".to_string(),
            "visible two".to_string(),
        ],
        visible_line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        history_lines: vec![
            "history zero".to_string(),
            "history one".to_string(),
            "history two".to_string(),
        ],
        history_line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        alternate_screen_active: false,
        truncated: false,
        primary_pid: None,
        process_state: None,
        exit_status: None,
    }];

    let visible_request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"visible","start":1,"end":3}}}}}}"#,
        json_escape(&pane_id)
    );
    let visible_response =
        dispatch_control_request_with_captures(&visible_request, &mut session, &primary, &captures);
    assert!(visible_response.contains("visible one\\nvisible two"));
    assert!(!visible_response.contains("visible zero"));
    assert!(visible_response.contains(r#""range":{"origin":"visible","start":1,"end":3}"#));

    let history_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"history","start":"start","end":2}}}}}}"#,
        json_escape(&pane_id)
    );
    let history_response =
        dispatch_control_request_with_captures(&history_request, &mut session, &primary, &captures);
    assert!(history_response.contains("history zero\\nhistory one"));
    assert!(!history_response.contains("history two"));
    assert!(history_response.contains(r#""range":{"origin":"history","start":0,"end":2}"#));

    let combined_request = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"combined","start":2,"end":5}}}}}}"#,
        json_escape(&pane_id)
    );
    let combined_response = dispatch_control_request_with_captures(
        &combined_request,
        &mut session,
        &primary,
        &captures,
    );
    assert!(combined_response.contains("history two\\nvisible zero\\nvisible one"));
    assert!(!combined_response.contains("history one"));
    assert!(!combined_response.contains("visible two"));
    assert!(combined_response.contains(r#""range":{"origin":"combined","start":2,"end":5}"#));
}

/// Verifies pane capture excludes alternate screen from history capture.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_capture_excludes_alternate_screen_from_history_capture() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    let captures = vec![PaneCaptureSource {
        pane_id: pane_id.clone(),
        visible_lines: vec!["alternate".to_string()],
        visible_line_style_spans: vec![Vec::new()],
        history_lines: vec!["normal".to_string()],
        history_line_style_spans: vec![Vec::new()],
        alternate_screen_active: true,
        truncated: false,
        primary_pid: None,
        process_state: None,
        exit_status: None,
    }];
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"include_history":true,"range":{{"origin":"combined","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );

    let response =
        dispatch_control_request_with_captures(&request, &mut session, &primary, &captures);

    assert!(response.contains(r#""content":"normal""#));
    assert!(!response.contains("alternate\\n"));
}

/// This regression test verifies that malformed CaptureRange values are
/// rejected before capture content is returned. The endpoint must fail
/// deterministically for missing ranges, reversed bounds, unsupported
/// origins, and endpoint values that are not valid offsets or symbolic
/// bounds.
#[test]
fn pane_capture_requires_valid_range() {
    let (mut session, primary) = test_session();
    let pane_id = session.windows()[0].panes()[0].id.to_string();

    let missing_range = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}}}}}}"#,
        json_escape(&pane_id)
    );
    let missing_response =
        dispatch_control_request_with_captures(&missing_range, &mut session, &primary, &[]);
    assert!(missing_response.contains("pane/capture requires range"));

    let invalid_range = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"history","start":2,"end":1}}}}}}"#,
        json_escape(&pane_id)
    );
    let invalid_response =
        dispatch_control_request_with_captures(&invalid_range, &mut session, &primary, &[]);
    assert!(invalid_response.contains("range start must not be greater than end"));

    let invalid_origin = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"scrollback","start":"start","end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );
    let invalid_origin_response =
        dispatch_control_request_with_captures(&invalid_origin, &mut session, &primary, &[]);
    assert!(invalid_origin_response.contains("range origin must be visible"));

    let invalid_endpoint = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"pane/capture","params":{{"target":{{"pane_id":"{}"}},"range":{{"origin":"visible","start":-1,"end":"end"}}}}}}"#,
        json_escape(&pane_id)
    );
    let invalid_endpoint_response =
        dispatch_control_request_with_captures(&invalid_endpoint, &mut session, &primary, &[]);
    assert!(invalid_endpoint_response.contains("range start must be an integer"));
}

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
            .filter(|client| client.role == crate::session::ClientRole::Primary)
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

/// Verifies approval control methods list and decide blocked requests.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn approval_control_methods_list_and_decide_blocked_requests() {
    let (mut session, primary) = test_session();
    let mut queue = BlockedApprovalQueue::default();
    let approval_id = queue
        .create(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-0".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "git diff".to_string(),
            declared_effects: vec!["read_filesystem".to_string()],
            matched_rules: vec!["git diff".to_string()],
            read_scopes: vec![".".to_string()],
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();

    let list_response = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(list_response.contains(&approval_id));
    assert!(list_response.contains(&format!(r#""id":"{}""#, approval_id)));
    assert!(list_response.contains(r#""version":1"#));
    assert!(list_response.contains(r#""state":"pending""#));
    assert!(list_response.contains(r#""requester":{"agent_id":"agent-1""#));
    assert!(list_response.contains(r#""parent_agent_chain":["agent-0"]"#));
    assert!(list_response.contains(r#""action_type":"shell_command""#));
    assert!(list_response.contains(r#""created_at":""#));
    assert!(list_response.contains(r#""decided_at":null"#));
    assert!(list_response.contains(r#""decided_by_client_id":null"#));
    assert!(list_response.contains(r#""summary":"git diff""#));
    assert!(list_response.contains(r#""effects":{"reads":["."]"#));
    assert!(list_response.contains(r#""scope":{"persistence":"project""#));
    assert!(list_response.contains(r#""instruction":null"#));
    assert!(list_response.contains(r#""matched_rules":["git diff"]"#));

    let pending_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":11,"method":"approval/list","params":{"target":{"default":true},"state":"pending"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(pending_filter.contains(&approval_id), "{pending_filter}");

    let approved_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":12,"method":"approval/list","params":{"state":"approved"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(
        approved_filter.contains(r#""approvals":[]"#),
        "{approved_filter}"
    );

    let decide_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"approval/decide","params":{{"approval_id":"{}","decision":"redirect","scope":{{"persistence":"once","command_prefix":["git","diff"]}},"instruction":"show diff summary only","idempotency_key":"decision-1"}}}}"#,
        approval_id
    );
    let decide_response = dispatch_control_request_with_approvals(
        &decide_request,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(decide_response.contains(r#""state":"redirected""#));
    assert!(decide_response.contains(r#""decision":"redirect""#));
    assert!(decide_response.contains(r#""decided_at":""#));
    assert!(decide_response.contains(&format!(r#""decided_by_client_id":"{}""#, primary)));
    assert!(decide_response.contains("show diff summary only"));

    let redirected_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":13,"method":"approval/list","params":{"state":"redirected"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(
        redirected_filter.contains(&approval_id),
        "{redirected_filter}"
    );

    let invalid_filter = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":14,"method":"approval/list","params":{"state":"missing"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );
    assert!(
        invalid_filter.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_filter}"
    );
}

/// Verifies generic control dispatches empty approval state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_dispatches_empty_approval_state() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""approvals":[]"#), "{list}");

    let filtered = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":11,"method":"approval/list","params":{"target":{"default":true},"state":"cancelled"}}"#,
        &mut session,
        &primary,
    );
    assert!(filtered.contains(r#""approvals":[]"#), "{filtered}");

    let missing = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"approval/decide","params":{"approval_id":"ba-missing","decision":"approve","idempotency_key":"missing-approval"}}"#,
        &mut session,
        &primary,
    );
    assert!(missing.contains(r#""error""#), "{missing}");
    assert!(missing.contains(r#""code":-32005"#), "{missing}");
    assert!(
        missing.contains(r#""mezzanine_code":"not_found""#),
        "{missing}"
    );
    assert!(missing.contains("approval request not found"), "{missing}");
}

/// Verifies approval decision control can emit required audit records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn approval_decision_control_can_emit_required_audit_records() {
    let (mut session, primary) = test_session();
    let mut queue = BlockedApprovalQueue::default();
    let approval_id = queue
        .create(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: Vec::new(),
            action_kind: "shell_command".to_string(),
            action_summary: "git status".to_string(),
            declared_effects: vec!["read_filesystem".to_string()],
            matched_rules: vec!["git status".to_string()],
            read_scopes: vec![".".to_string()],
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let root = temp_root("approval-audit");
    let path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: false,
        required: true,
    });
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","idempotency_key":"audit-approval"}}}}"#,
        approval_id
    );

    let response = dispatch_control_request_with_approvals_and_audit(
        &request,
        &mut session,
        &primary,
        &mut queue,
        &mut audit_log,
    );

    assert!(response.contains(r#""state":"approved""#));
    let audit = fs::read_to_string(&path).unwrap();
    assert!(audit.contains(r#""event_type":"approval""#));
    assert!(audit.contains(r#""outcome":"started""#));
    assert!(audit.contains(r#""outcome":"applied""#));
    assert!(audit.contains(r#""approval_id""#));
    let _ = fs::remove_dir_all(root);
}

/// Verifies project trust control methods decide list inspect and revoke.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn project_trust_control_methods_decide_list_inspect_and_revoke() {
    let mut store = ProjectTrustStore::default();
    let root = std::env::temp_dir()
        .join(format!("mez-control-trust-{}", std::process::id()))
        .to_string_lossy()
        .to_string();
    let decide = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust"}}}}"#,
        json_escape(&root)
    );

    let decide_response = dispatch_project_trust_request(&decide, &mut store);
    assert!(decide_response.contains(r#""state":"trusted""#));
    assert!(decide_response.contains(&format!(r#""id":"{}""#, json_escape(&root))));
    assert!(decide_response.contains(r#""version":1"#));
    assert!(decide_response.contains(r#""trusted_at":""#));
    assert!(!decide_response.contains(r#""trusted_at":"unix:"#));
    assert!(decide_response.contains(r#""rejected_at":null"#));
    assert!(decide_response.contains(r#""revoked_at":null"#));
    assert!(decide_response.contains(r#""decided_by_client_id":null"#));
    assert!(decide_response.contains(r#""overlay_files":[]"#));
    assert!(decide_response.contains(r#""capability_expansion_summary":[]"#));
    assert!(decide_response.contains(r#""diagnostics":[]"#));

    let list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"project/trust/list","params":{}}"#,
        &mut store,
    );
    assert!(list_response.contains(&json_escape(&root)));

    let trusted_list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":20,"method":"project/trust/list","params":{"state":"trusted"}}"#,
        &mut store,
    );
    assert!(
        trusted_list_response.contains(&json_escape(&root)),
        "{trusted_list_response}"
    );

    let pending_list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":21,"method":"project/trust/list","params":{"state":"pending"}}"#,
        &mut store,
    );
    assert!(
        !pending_list_response.contains(&json_escape(&root)),
        "{pending_list_response}"
    );

    let invalid_state_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":22,"method":"project/trust/list","params":{"state":"unknown"}}"#,
        &mut store,
    );
    assert!(
        invalid_state_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_state_response}"
    );

    let inspect = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"project/trust/inspect","params":{{"project_root":"{}"}}}}"#,
        json_escape(&root)
    );
    let inspect_response = dispatch_project_trust_request(&inspect, &mut store);
    assert!(inspect_response.contains(r#""state":"trusted""#));

    let revoke = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"project/trust/revoke","params":{{"project_root":"{}","idempotency_key":"revoke"}}}}"#,
        json_escape(&root)
    );
    let revoke_response = dispatch_project_trust_request(&revoke, &mut store);
    assert!(revoke_response.contains(r#""state":"revoked""#));

    let revoked_list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":23,"method":"project/trust/list","params":{"state":"revoked"}}"#,
        &mut store,
    );
    assert!(
        revoked_list_response.contains(&json_escape(&root)),
        "{revoked_list_response}"
    );
}

/// Verifies generic control dispatches empty project trust state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_dispatches_empty_project_trust_state() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/list","params":{"state":null}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""projects":[]"#), "{list}");

    let invalid_state = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":10,"method":"project/trust/list","params":{"state":"unknown"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid_state.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_state}"
    );

    let inspect = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"project/trust/inspect","params":{"project_root":"/tmp/missing"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        inspect.contains(r#""mezzanine_code":"not_found""#),
        "{inspect}"
    );
    assert!(inspect.contains("project not found"), "{inspect}");

    let decide = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"project/trust/decide","params":{"project_root":"/tmp/project","decision":"trust","idempotency_key":"trust-project"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        decide.contains(r#""mezzanine_code":"invalid_state""#),
        "{decide}"
    );
    assert!(
        decide.contains("project trust store is not configured"),
        "{decide}"
    );

    let unknown = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"project/trust/list","params":{"unexpected":true}}"#,
        &mut session,
        &primary,
    );
    assert!(unknown.contains("project/trust/list params contains unknown field"));
}

/// Verifies observer cannot inspect other observer request.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn observer_cannot_inspect_other_observer_request() {
    let (mut session, _primary) = test_session();
    let (first_client, _first_request) = session.request_observer("first");
    let (_second_client, second_request) = session.request_observer("second");
    let inspect_request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"observer/inspect","params":{{"observer_request_id":"{}"}}}}"#,
        second_request
    );

    let response =
        dispatch_control_request_for_client(&inspect_request, &mut session, &first_client, None);

    assert!(response.contains(r#""mezzanine_code":"forbidden""#));
}

/// Verifies dispatches session rename and kill methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_session_rename_and_kill_methods() {
    let (mut session, primary) = test_session();

    let rename_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/rename","params":{"name":"work","idempotency_key":"rename"}}"#,
        &mut session,
        &primary,
    );
    assert!(rename_response.contains(r#""renamed":true"#));
    assert_eq!(session.name, "work");

    let kill_response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"session/kill","params":{"force":true,"idempotency_key":"kill"}}"#,
        &mut session,
        &primary,
    );
    assert!(kill_response.contains(r#""killed":true"#));
    assert!(session.windows().is_empty());
}

/// Verifies mutating control methods require idempotency keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mutating_control_methods_require_idempotency_keys() {
    let (mut session, primary) = test_session();

    let response = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work"}}"#,
        &mut session,
        &primary,
    );

    assert!(response.contains(r#""error""#));
    assert!(response.contains("idempotency_key"));
    assert_eq!(session.windows().len(), 1);
}

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

/// Verifies idempotency conflicts on reused key with different params.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn idempotency_conflicts_on_reused_key_with_different_params() {
    let (mut session, primary) = test_session();
    let mut cache = ControlIdempotencyCache::default();

    let first = dispatch_control_request_cached(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"one","idempotency_key":"same"}}"#,
        &mut session,
        &primary,
        &mut cache,
    );
    let second = dispatch_control_request_cached(
        r#"{"jsonrpc":"2.0","id":2,"method":"window/create","params":{"name":"two","idempotency_key":"same"}}"#,
        &mut session,
        &primary,
        &mut cache,
    );

    assert!(first.contains(r#""window_id":"@2""#));
    assert!(second.contains(r#""mezzanine_code":"conflict""#));
    assert_eq!(session.windows().len(), 2);
}

/// Verifies that completed JSON-RPC idempotency responses are retained within
/// explicit entry and byte budgets. Primary attach loops create many control
/// frames, so this protects the runtime from keeping arbitrarily many old
/// responses while preserving retry behavior for recent requests.
#[test]
fn idempotency_cache_evicts_oldest_entries_within_limits() {
    let mut cache = ControlIdempotencyCache::with_limits(2, 1024);

    cache.remember_response("client:a", "window/create", None, r#"{"ok":1}"#);
    cache.remember_response("client:b", "window/create", None, r#"{"ok":2}"#);
    cache.remember_response("client:c", "window/create", None, r#"{"ok":3}"#);

    assert_eq!(cache.len(), 2);
    assert!(
        cache
            .cached_response("client:a", "window/create", &None)
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .cached_response("client:b", "window/create", &None)
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .cached_response("client:c", "window/create", &None)
            .unwrap()
            .is_some()
    );
    assert!(cache.retained_bytes() <= 1024);
}

/// Verifies that oversized idempotency responses are not retained. A single
/// rendered response must not consume the entire cache because callers can
/// still reissue the operation when the bounded replay window cannot store it.
#[test]
fn idempotency_cache_skips_entries_larger_than_byte_limit() {
    let mut cache = ControlIdempotencyCache::with_limits(8, 16);

    cache.remember_response("client:large", "terminal/step", None, "x".repeat(64));

    assert!(cache.is_empty());
    assert_eq!(cache.retained_bytes(), 0);
}

/// Verifies connection idempotency replays completed error responses.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn connection_idempotency_replays_completed_error_responses() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let mut input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let invalid = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/resize","params":{"target":{"pane_id":"%1"},"size":{"mode":"cells","columns":"wide","rows":10},"idempotency_key":"bad-resize"}}"#,
    );
    input.extend_from_slice(&invalid);
    input.extend_from_slice(&invalid);

    let (output, _) =
        handle_control_frames(&input, 4096, &mut session, &mut connection, &mut cache).unwrap();
    let (_init_body, init_len) = decode_control_frame(&output, 4096).unwrap();
    let (first_body, first_len) = decode_control_frame(&output[init_len..], 4096).unwrap();
    let (second_body, _) = decode_control_frame(&output[init_len + first_len..], 4096).unwrap();

    assert_eq!(first_body, second_body);
    assert!(first_body.contains(r#""error""#), "{first_body}");
    assert!(first_body.contains(r#""code":-32602"#), "{first_body}");
    assert!(
        first_body.contains(r#""mezzanine_code":"invalid_params""#),
        "{first_body}"
    );
    assert_eq!(cache.len(), 1);
}

/// Verifies mcp list exposes registry state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mcp_list_exposes_registry_state() {
    let (mut session, primary) = test_session();
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    registry
        .mark_available(
            "fs",
            vec![McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: false,
                blacklisted: false,
                permission_required: true,
                effects: McpToolEffects {
                    reads_filesystem: true,
                    ..McpToolEffects::none()
                },
                approval: crate::mcp::McpApprovalSetting::Inherit,
                description: "Read a file".to_string(),
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            }],
        )
        .unwrap();

    let response = dispatch_control_request_with_mcp(
        r#"{"jsonrpc":"2.0","id":1,"method":"mcp/list","params":{}}"#,
        &mut session,
        &primary,
        &registry,
    );

    assert!(response.contains(r#""servers""#));
    assert!(response.contains(r#""id":"fs""#));
    assert!(response.contains(r#""version":1"#));
    assert!(response.contains(r#""server_id":"fs""#));
    assert!(response.contains(r#""state":"available""#));
    assert!(response.contains(r#""configured":true"#));
    assert!(response.contains(r#""blacklisted":false"#));
    assert!(response.contains(r#""transport":{"kind":"stdio"}"#));
    assert!(response.contains(r#""last_checked_at":""#));
    assert!(response.contains(r#""diagnostics":[]"#));
    assert!(response.contains(r#""tools""#));
    assert!(response.contains(r#""id":"fs:read_file""#));
    assert!(response.contains(r#""name":"read_file""#));
    assert!(response.contains(r#""effects":{"reads_filesystem":true"#));
    assert!(response.contains(r#""mutates_filesystem":false"#));
    assert!(response.contains(r#""executes_processes":false"#));
    assert!(response.contains(r#""accesses_credentials":false"#));
    assert!(response.contains(r#""uses_network":false"#));
    assert!(response.contains(r#""has_side_effects":false"#));
    assert!(response.contains(r#""input_schema":{"type":"object"}"#));

    let targeted = dispatch_control_request_with_mcp(
        r#"{"jsonrpc":"2.0","id":2,"method":"mcp/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
        &registry,
    );
    assert!(targeted.contains(r#""id":"fs""#), "{targeted}");

    let null_target = dispatch_control_request_with_mcp(
        r#"{"jsonrpc":"2.0","id":3,"method":"mcp/list","params":{"target":null}}"#,
        &mut session,
        &primary,
        &registry,
    );
    assert!(
        null_target.contains(r#""id":"fs:read_file""#),
        "{null_target}"
    );

    let missing_session = dispatch_control_request_with_mcp(
        r#"{"jsonrpc":"2.0","id":4,"method":"mcp/list","params":{"target":{"session_id":"missing"}}}"#,
        &mut session,
        &primary,
        &registry,
    );
    assert!(
        missing_session.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session}"
    );

    let invalid_target = dispatch_control_request_with_mcp(
        r#"{"jsonrpc":"2.0","id":5,"method":"mcp/list","params":{"target":"default"}}"#,
        &mut session,
        &primary,
        &registry,
    );
    assert!(
        invalid_target.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_target}"
    );
}

/// Exercises the authorized configuration control path for effective reads,
/// validation, and reload. The protocol defines `effective` as a Boolean, so
/// both Boolean values must be accepted while the implementation currently
/// returns the effective configuration envelope for either value.
#[test]
fn config_control_get_reload_and_validation_use_authorized_dispatch() {
    let (mut session, primary) = test_session();
    let layers = vec![
        ConfigLayer {
            name: "defaults".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10000\n".to_string(),
        },
        ConfigLayer {
            name: "primary".to_string(),
            path: Some(PathBuf::from("/home/user/.config/mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 2000\npersist = true\n[mcp_servers.fs]\nargs = [\"--root\", \".\"]\n"
                .to_string(),
        },
    ];
    let mut idempotency = ControlIdempotencyCache::default();

    let get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","effective":true}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    let get_json: serde_json::Value = serde_json::from_str(&get).unwrap();
    assert_eq!(get_json["result"]["value"], 2000);
    assert!(get.contains(r#""source":"primary""#));
    assert!(get.contains(r#""layers""#));
    assert!(get.contains(r#""id":"defaults""#));
    assert!(get.contains(r#""layer_type":"user""#));
    assert!(get.contains(r#""precedence":0"#));
    assert!(get.contains(r#""trusted":true"#));
    assert!(get.contains(r#""applied":true"#));
    assert!(get.contains(r#""schema_version":1"#));
    assert!(get.contains(r#""diagnostics":[]"#));

    let full_get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":20,"method":"config/get","params":{"effective":true}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    let full_get_json: serde_json::Value = serde_json::from_str(&full_get).unwrap();
    assert_eq!(full_get_json["result"]["value"]["history.lines"], 2000);
    assert_eq!(full_get_json["result"]["value"]["history.persist"], true);
    assert_eq!(
        full_get_json["result"]["value"]["mcp_servers.fs.args"],
        serde_json::json!(["--root", "."])
    );

    let explicit_non_effective_get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":21,"method":"config/get","params":{"path":"history.lines","effective":false}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    let explicit_non_effective_json: serde_json::Value =
        serde_json::from_str(&explicit_non_effective_get).unwrap();
    assert_eq!(explicit_non_effective_json["result"]["value"], 2000);
    assert_eq!(explicit_non_effective_json["result"]["source"], "primary");

    let validate = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":2,"method":"config/validate","params":{}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    assert!(validate.contains(r#""valid":true"#));
    assert_eq!(session.config_generation, 0);

    let reload = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":3,"method":"config/reload","params":{"idempotency_key":"reload-config"}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    assert!(reload.contains(r#""operation":"reload""#));
    assert!(reload.contains(r#""layers""#));
    assert!(reload.contains(r#""applied":true"#));
    assert!(reload.contains(r#""status":"applied""#));
    assert_eq!(session.config_generation, 1);

    let cached_reload = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":4,"method":"config/reload","params":{"idempotency_key":"reload-config"}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    assert_eq!(cached_reload, reload);
    assert_eq!(session.config_generation, 1);
}

/// Verifies that the config control surface rejects unknown request fields
/// before handler-specific parsing. Config methods support a specialized
/// dispatcher for layer state and idempotency, so unknown fields must be checked
/// there as well as in the generic control path.
#[test]
fn config_control_methods_reject_unknown_params_on_specialized_dispatch() {
    let (mut session, primary) = test_session();
    let mut idempotency = ControlIdempotencyCache::default();

    let get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","surprise":true}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(get.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(get.contains("config/get params contains unknown field"));

    let invalid_effective = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":5,"method":"config/get","params":{"path":"history.lines","effective":"false"}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(invalid_effective.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_effective.contains("config/get effective must be a boolean"));

    let validate = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":3,"method":"config/validate","params":{"scope":"project"}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(validate.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(validate.contains("config/validate params contains unknown field"));

    let set = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":2,"method":"config/set","params":{"path":"history.lines","value":2048,"idempotency_key":"set","surprise":true}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(set.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(set.contains("config/set params contains unknown field"));
}

/// Verifies config control get reports per layer diagnostics.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_get_reports_per_layer_diagnostics() {
    let (mut session, primary) = test_session();
    let layers = vec![
        ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 2000\n".to_string(),
        },
        ConfigLayer {
            name: "project".to_string(),
            path: Some(PathBuf::from("/workspace/.mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: false,
            text: "[history]\nlines = 7\n".to_string(),
        },
    ];
    let mut idempotency = ControlIdempotencyCache::default();

    let get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","effective":true}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );

    let get_json: serde_json::Value = serde_json::from_str(&get).unwrap();
    assert_eq!(get_json["result"]["value"], 2000);
    assert!(get.contains(r#""id":"project""#), "{get}");
    assert!(get.contains(r#""layer_type":"project_root""#), "{get}");
    assert!(get.contains(r#""applied":false"#), "{get}");
    assert!(get.contains(r#""state":"skipped""#), "{get}");
    assert!(
        get.contains("project overlay is pending trust and was not applied"),
        "{get}"
    );
}

/// Verifies config control mutations persist explicit targets and are cached.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_mutations_persist_explicit_targets_and_are_cached() {
    let (mut session, primary) = test_session();
    let root = temp_root("config-mutation");
    let primary_path = root.join("config.toml");
    let project_path = root.join(".mezzanine").join("config.toml");
    fs::write(&primary_path, "[history]\nlines = 10000\npersist = true\n").unwrap();
    fs::create_dir_all(project_path.parent().unwrap()).unwrap();
    fs::write(&project_path, "[history]\nlines = 50\npersist = true\n").unwrap();
    let mut idempotency = ControlIdempotencyCache::default();
    let primary_path_json = json_escape(&primary_path.to_string_lossy());
    let project_path_json = json_escape(&project_path.to_string_lossy());
    let set_request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"config/set","params":{{"path":"history.lines","value":2048,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"set-lines"}}}}"#,
        primary_path_json
    );

    let first = dispatch_control_request_for_client_with_config(
        &set_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    let second = dispatch_control_request_for_client_with_config(
        &set_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert_eq!(first, second);
    assert_eq!(idempotency.len(), 1);
    assert_eq!(session.config_generation, 1);
    assert!(first.contains(r#""applied":true"#));
    assert!(first.contains(r#""persisted":true"#));
    assert!(first.contains(r#""scope":"user""#));
    assert!(
        fs::read_to_string(&primary_path)
            .unwrap()
            .contains("lines = 2048")
    );

    let set_array_request = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"config/set","params":{{"path":"mcp_servers.fs.args","value":["--root","."],"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"set-array"}}}}"#,
        primary_path_json
    );
    let array_response = dispatch_control_request_for_client_with_config(
        &set_array_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(array_response.contains(r#""applied":true"#));
    assert_eq!(session.config_generation, 2);
    assert!(
        fs::read_to_string(&primary_path)
            .unwrap()
            .contains(r#"args = ["--root", "."]"#)
    );

    let conflict_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"config/set","params":{{"path":"history.lines","value":4096,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"set-lines"}}}}"#,
        primary_path_json
    );
    let conflict = dispatch_control_request_for_client_with_config(
        &conflict_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(conflict.contains(r#""mezzanine_code":"conflict""#));
    assert_eq!(session.config_generation, 2);
    assert!(
        fs::read_to_string(&primary_path)
            .unwrap()
            .contains("lines = 2048")
    );

    let unset_project = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"config/unset","params":{{"path":"history.persist","persist":{{"scope":"project","path":"{}"}},"idempotency_key":"unset-project"}}}}"#,
        project_path_json
    );
    let project_response = dispatch_control_request_for_client_with_config(
        &unset_project,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(project_response.contains(r#""scope":"project""#));
    assert_eq!(session.config_generation, 3);
    assert!(
        !fs::read_to_string(&project_path)
            .unwrap()
            .contains("persist")
    );

    let primary_scope_request = format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"config/set","params":{{"path":"history.lines","value":8192,"persist":{{"scope":"primary","path":"{}"}},"idempotency_key":"set-primary-scope"}}}}"#,
        primary_path_json
    );
    let primary_scope_response = dispatch_control_request_for_client_with_config(
        &primary_scope_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(primary_scope_response.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(primary_scope_response.contains("must be live, user, or project"));
    assert_eq!(session.config_generation, 3);

    let _ = fs::remove_dir_all(root);
}

/// Verifies config control mutations can emit required audit records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_mutations_can_emit_required_audit_records() {
    let (mut session, primary) = test_session();
    let root = temp_root("config-audit");
    let config_path = root.join("config.toml");
    fs::write(&config_path, "[history]\nlines = 10000\n").unwrap();
    let audit_path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    });
    let mut idempotency = ControlIdempotencyCache::default();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"config/set","params":{{"path":"history.lines","value":2048,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"config-audit"}}}}"#,
        json_escape(&config_path.to_string_lossy())
    );

    let response = dispatch_control_request_for_client_with_config_and_audit(
        &request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
        &mut audit_log,
    );

    assert!(response.contains(r#""applied":true"#));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#));
    assert!(audit.contains(r#""action":"set""#));
    assert!(audit.contains(r#""outcome":"started""#));
    assert!(audit.contains(r#""outcome":"applied""#));
    assert!(audit.contains(r#""key":"history.lines""#));
    let _ = fs::remove_dir_all(root);
}

/// Verifies config control mutations are primary only.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_mutations_are_primary_only() {
    let (mut session, _primary) = test_session();
    let (observer_client, _observer_request) = session.request_observer("observer");
    let root = temp_root("config-observer");
    let path = root.join("config.toml");
    fs::write(&path, "[history]\nlines = 10000\n").unwrap();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"config/set","params":{{"path":"history.lines","value":1,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"observer-set"}}}}"#,
        json_escape(&path.to_string_lossy())
    );
    let mut idempotency = ControlIdempotencyCache::default();

    let response = dispatch_control_request_for_client_with_config(
        &request,
        &mut session,
        &observer_client,
        &[],
        &mut idempotency,
    );

    assert!(response.contains(r#""mezzanine_code":"forbidden""#));
    assert!(fs::read_to_string(&path).unwrap().contains("lines = 10000"));
    assert!(idempotency.is_empty());

    let _ = fs::remove_dir_all(root);
}
