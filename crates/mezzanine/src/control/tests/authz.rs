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
            requested_version: 2,
            requested_role: RequestedRole::Observer,
            client_version: None,
            session_intent: None,
            idempotency_key: None,
            session_target_json: None,
            detach_primary_on_disconnect: false,
            event_stream_version: None,
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

/// Verifies authenticated observer negotiation advertises the attached
/// read-only capability set without pending request state.
#[test]
fn observer_negotiation_uses_attached_read_only_capabilities() {
    let result = initialize(
        InitializeParams {
            client_name: "observer".to_string(),
            requested_version: 2,
            requested_role: RequestedRole::Observer,
            client_version: None,
            session_intent: None,
            idempotency_key: None,
            session_target_json: None,
            detach_primary_on_disconnect: false,
            event_stream_version: None,
            client: None,
            authentication: Some(AuthenticationMaterial::peer_credentials()),
        },
        InitializeContext {
            outer_authenticated: false,
            trusted_interactive_assertion: false,
        },
    )
    .unwrap();

    assert_eq!(result.granted_role, GrantedRole::Observer);
    assert_eq!(result.session, None);
    assert!(result.capabilities.methods.contains(&"terminal/view"));
    assert!(result.capabilities.methods.contains(&"event/list"));
    assert!(!result.capabilities.methods.contains(&"observer/inspect"));
    assert!(result.capabilities.features.event_replay);
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
    assert_eq!(result.server.protocol_versions, vec![2]);
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
    params.requested_version = 1;

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

/// Verifies attached observer capabilities expose read-only view and event
/// methods while excluding primary-only session and MCP authority.
#[test]
fn observer_capabilities_are_read_only_and_view_enabled() {
    let capabilities = Capabilities::observer();

    for method in ["session/get", "mcp/list", "observer/inspect"] {
        assert!(
            !capabilities.methods.contains(&method),
            "{method} must not be exposed to observers"
        );
    }
    for method in [
        "control/initialize",
        "client/detach",
        "control/cancel",
        "control/shutdown",
        "terminal/view",
        "event/list",
    ] {
        assert!(
            capabilities.methods.contains(&method),
            "{method} missing from observer capabilities"
        );
    }
}

/// Verifies an observer may detach only its own authenticated client record.
///
/// Pairing and profile checks use this narrow cleanup capability. Supplying
/// another client id must remain forbidden so the helper cannot become remote
/// client-administration authority.
#[test]
fn observer_self_detach_cannot_target_another_client() {
    let (mut session, primary) = test_session();
    let observer = session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    let own = parse_json_rpc_request(&format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"client/detach","params":{{"client_id":"{}","idempotency_key":"self-detach"}}}}"#,
        observer
    ))
    .unwrap();
    let other = parse_json_rpc_request(&format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"client/detach","params":{{"client_id":"{}","idempotency_key":"other-detach"}}}}"#,
        primary
    ))
    .unwrap();

    super::super::authorize_control_request(&session, &observer, &own).unwrap();
    let error = super::super::authorize_control_request(&session, &observer, &other).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("detach only themselves"));
}

/// Restricted-role capability advertisements must use the same method lists as
/// role authorization so clients can rely on initialization results to plan the
/// requests that will be accepted before method-specific parameter checks.
#[test]
fn restricted_role_capabilities_match_authorization_method_sets() {
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
    assert!(json.contains(r#""protocol_versions":[2]"#));
    assert!(json.contains(r#""session":{"id":"default""#));
    assert!(json.contains(r#""window_count":0"#));
    assert!(json.contains(r#""protocol_version":2"#));
    assert!(json.contains(r#""event_types":["client_attached""#));
    assert!(json.contains(r#""mcp_server_changed""#));
    assert!(json.contains(r#""roles":["primary","observer""#));
    assert!(!json.contains("pending_observer"));
    assert!(json.contains(r#""transports":["unix"]"#));
    assert!(json.contains(r#""max_frame_size":"#) || json.contains(r#""max_frame_size":1048576"#));
    assert!(json.contains(r#""approval_bypass":true"#));
}
