//! Control authz tests.

use super::*;

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
