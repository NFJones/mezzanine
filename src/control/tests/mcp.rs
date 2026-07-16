//! Control mcp tests.

use super::*;

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
                approval: mez_agent::mcp::McpApprovalSetting::Inherit,
                description: "Read a file".to_string(),
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            }],
            1,
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
