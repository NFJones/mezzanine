//! Unit tests for MCP registry, protocol parsing, stdio transport, HTTP transport, and audit wrappers.

use super::{
    DEFAULT_MCP_TOOL_TIMEOUT_MS, McpApprovalSetting, McpRegistry, McpServerConfig, McpServerStatus,
    McpStartupTransportPlan, McpToolAuditCallContext, McpToolCallPlan, McpToolCallRequest,
    McpToolEffects, McpToolState, build_mcp_default_initialize_request,
    build_mcp_initialize_request, build_mcp_tools_call_request, build_mcp_tools_list_request,
    call_stdio_mcp_tool_with_audit, call_streamable_http_mcp_tool,
    call_streamable_http_mcp_tool_with_audit, discover_stdio_mcp_server,
    discover_stdio_mcp_server_into_registry, discover_streamable_http_mcp_server_into_registry,
    initialize_streamable_http_mcp_server, parse_mcp_initialize_response,
    parse_mcp_tools_call_response, parse_mcp_tools_list_response, read_bounded_protocol_line,
    spawn_stdio_mcp_connection,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
/// Runs the assert json eq operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn assert_json_eq(actual: &str, expected: &str) {
    let actual: Value = serde_json::from_str(actual).unwrap();
    let expected: Value = serde_json::from_str(expected).unwrap();
    assert_eq!(actual, expected);
}

/// Runs the config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn config() -> McpServerConfig {
    McpServerConfig::stdio("fs", "filesystem", "mcp-fs", Vec::new())
}

/// Runs the tool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn tool() -> McpToolState {
    McpToolState {
        server_id: String::new(),
        name: "read_file".to_string(),
        available: false,
        blacklisted: false,
        permission_required: true,
        effects: McpToolEffects {
            reads_filesystem: true,
            ..McpToolEffects::none()
        },
        approval: McpApprovalSetting::Inherit,
        description: "Read a file".to_string(),
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }
}

/// Verifies validates required server fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validates_required_server_fields() {
    let mut config = config();
    config.command = None;

    let error = config.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies available server exposes tools.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn available_server_exposes_tools() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();

    registry.mark_available("fs", vec![tool()]).unwrap();

    let tools = registry.available_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].server_id, "fs");
    assert!(tools[0].available);
    assert!(
        registry.list_servers()[0]
            .last_checked_at_unix_seconds
            .is_some()
    );
    let prompt_tools = registry.prompt_summary().available_tools;
    assert_eq!(prompt_tools.len(), 1);
    assert!(prompt_tools[0].input_schema_json.contains(r#""path""#));
}

/// Verifies unavailable server does not expose tools.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn unavailable_server_does_not_expose_tools() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()]).unwrap();

    registry
        .mark_unavailable("fs", "missing executable")
        .unwrap();

    assert!(registry.available_tools().is_empty());
    assert_eq!(
        registry.list_servers()[0].status,
        McpServerStatus::Unavailable
    );
    assert!(
        registry.list_servers()[0]
            .last_checked_at_unix_seconds
            .is_some()
    );
}

/// Verifies session blacklist hides tools.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_blacklist_hides_tools() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()]).unwrap();

    registry
        .blacklist_for_session("fs", "startup failed")
        .unwrap();

    assert!(registry.available_tools().is_empty());
    assert!(registry.list_servers()[0].tools[0].blacklisted);
}

/// Verifies retry server clears session blacklist before rediscovery.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn retry_server_clears_session_blacklist_before_rediscovery() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()]).unwrap();
    registry
        .blacklist_for_session("fs", "failed handshake")
        .unwrap();

    registry.retry_server("fs").unwrap();

    let server = registry.list_servers()[0];
    assert_eq!(server.status, McpServerStatus::Configured);
    assert!(server.blacklist_reason.is_none());
    assert!(!server.tools[0].available);
    assert!(!server.tools[0].blacklisted);
}

/// Verifies startup plan marks server starting without secret values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn startup_plan_marks_server_starting_without_secret_values() {
    let mut registry = McpRegistry::default();
    let mut config = config();
    config
        .env
        .insert("LOG_LEVEL".to_string(), "debug".to_string());
    config.env_vars.push("MCP_TOKEN".to_string());
    registry.add_server(config).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert("MCP_TOKEN".to_string(), "secret".to_string());

    let plan = registry.startup_plan("fs", &environment).unwrap();

    assert_eq!(registry.list_servers()[0].status, McpServerStatus::Starting);
    match plan.transport {
        McpStartupTransportPlan::Stdio {
            command,
            environment,
            ..
        } => {
            assert_eq!(command, "mcp-fs");
            assert_eq!(environment.pass, vec!["MCP_TOKEN"]);
            assert!(!format!("{:?}", environment).contains("secret"));
        }
        McpStartupTransportPlan::StreamableHttp { .. } => panic!("expected stdio plan"),
    }
}

/// Verifies startup missing environment blacklists for session.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn startup_missing_environment_blacklists_for_session() {
    let mut registry = McpRegistry::default();
    let mut config = config();
    config.env_vars.push("MCP_TOKEN".to_string());
    registry.add_server(config).unwrap();

    let error = registry.startup_plan("fs", &BTreeMap::new()).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert_eq!(
        registry.list_servers()[0].status,
        McpServerStatus::Blacklisted
    );
    assert!(registry.prompt_summary().unavailable_servers[0].retryable);
}

/// Verifies registry can blacklist all missing env servers without starting transport.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn registry_can_blacklist_all_missing_env_servers_without_starting_transport() {
    let mut registry = McpRegistry::default();
    let mut missing = config();
    missing.env_vars.push("MCP_TOKEN".to_string());
    registry.add_server(missing).unwrap();
    registry
        .add_server(McpServerConfig::stdio("ok", "ok", "mcp-ok", Vec::new()))
        .unwrap();

    let blacklisted = registry
        .blacklist_servers_with_missing_environment(&BTreeMap::new())
        .unwrap();

    assert_eq!(blacklisted, vec!["fs".to_string()]);
    assert_eq!(
        registry.server("fs").unwrap().status,
        McpServerStatus::Blacklisted
    );
    assert_eq!(
        registry.server("ok").unwrap().status,
        McpServerStatus::Configured
    );
}

/// Verifies tool call plan requires approval for risky tool.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn tool_call_plan_requires_approval_for_risky_tool() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()]).unwrap();

    let plan = registry
        .plan_tool_call(&McpToolCallRequest {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            arguments_json: "{\"path\":\"README.md\"}".to_string(),
            timeout_ms: None,
            approval_bypass: false,
        })
        .unwrap();

    assert!(plan.approval_required);
    assert_eq!(plan.audit_event_class, "external_integration");
    assert_eq!(plan.timeout_ms, DEFAULT_MCP_TOOL_TIMEOUT_MS);
}

/// Verifies bypass removes approval but not availability checks.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn bypass_removes_approval_but_not_availability_checks() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()]).unwrap();
    registry
        .blacklist_for_session("fs", "failed handshake")
        .unwrap();

    let error = registry
        .plan_tool_call(&McpToolCallRequest {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            arguments_json: "{}".to_string(),
            timeout_ms: None,
            approval_bypass: true,
        })
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies disabled tools take precedence over enabled tools.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn disabled_tools_take_precedence_over_enabled_tools() {
    let mut registry = McpRegistry::default();
    let mut config = config();
    config.enabled_tools.push("read_file".to_string());
    config.disabled_tools.push("read_file".to_string());
    registry.add_server(config).unwrap();

    registry.mark_available("fs", vec![tool()]).unwrap();

    assert!(registry.available_tools().is_empty());
}

/// Verifies secret bearing http headers are rejected.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn secret_bearing_http_headers_are_rejected() {
    let mut config = McpServerConfig::streamable_http("web", "web", "https://example.test/mcp");
    config
        .http_headers
        .insert("Authorization".to_string(), "Bearer token".to_string());

    let error = config.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies json rpc builders emit mcp initialize and list requests.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn json_rpc_builders_emit_mcp_initialize_and_list_requests() {
    let initialize = build_mcp_initialize_request(1, "mez", "0.1.0", "2025-11-25");
    let list = build_mcp_tools_list_request(2, Some("next"));

    assert_json_eq(
        &initialize,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"mez","version":"0.1.0"}}}"#,
    );
    assert_json_eq(
        &list,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"cursor":"next"}}"#,
    );
}

/// Verifies tools call builder embeds arguments as json object.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn tools_call_builder_embeds_arguments_as_json_object() {
    let request =
        build_mcp_tools_call_request(3, "read_file", r#"{"path":"src/main.rs"}"#).unwrap();

    assert_json_eq(
        &request,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"src/main.rs"}}}"#,
    );
    assert_eq!(
        build_mcp_tools_call_request(4, "read_file", r#"["not","object"]"#)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
}

/// Verifies parses initialize response capabilities.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_initialize_response_capabilities() {
    let response = parse_mcp_initialize_response(
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fs","version":"1.2.3"},"instructions":"Use carefully"}}"#,
        1,
    )
    .unwrap();

    assert_eq!(response.protocol_version, "2025-11-25");
    assert_eq!(response.server_name, "fs");
    assert_eq!(response.server_version, "1.2.3");
    assert_eq!(response.instructions.as_deref(), Some("Use carefully"));
    assert!(response.supports_tools);
}

/// Verifies parses tools list response with schema and cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_tools_list_response_with_schema_and_cursor() {
    let response = parse_mcp_tools_list_response(
        r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"read_file","title":"Read File","description":"Read a file","inputSchema":{"type":"object","properties":{"path":{"type":"string"}}}}],"nextCursor":"more"}}"#,
        2,
    )
    .unwrap();

    assert_eq!(response.tools.len(), 1);
    assert_eq!(response.tools[0].name, "read_file");
    assert_eq!(response.tools[0].title.as_deref(), Some("Read File"));
    assert!(response.tools[0].input_schema_json.contains(r#""path""#));
    assert_eq!(response.next_cursor.as_deref(), Some("more"));
}

/// Verifies parses tools call response content and tool error flag.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_tools_call_response_content_and_tool_error_flag() {
    let response = parse_mcp_tools_call_response(
        r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"denied"}],"structuredContent":{"status":"denied"},"isError":true}}"#,
        3,
    )
    .unwrap();

    assert!(response.content_json.contains(r#""denied""#));
    assert_eq!(
        response.structured_content_json.as_deref(),
        Some(r#"{"status":"denied"}"#)
    );
    assert!(response.is_error);
}

/// Verifies protocol error response is rejected.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn protocol_error_response_is_rejected() {
    let error = parse_mcp_tools_call_response(
        r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Unknown tool"}}"#,
        3,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("Unknown tool"));
}

/// Verifies tool call plan rejects unavailable server.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn tool_call_plan_rejects_unavailable_server() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()]).unwrap();
    registry.mark_unavailable("fs", "process exited").unwrap();

    let error = registry
        .plan_tool_call(&McpToolCallRequest {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            arguments_json: "{}".to_string(),
            timeout_ms: None,
            approval_bypass: false,
        })
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies planned tool call can be serialized as json rpc.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn planned_tool_call_can_be_serialized_as_json_rpc() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()]).unwrap();
    let plan = registry
        .plan_tool_call(&McpToolCallRequest {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            arguments_json: r#"{"path":"SPEC.md"}"#.to_string(),
            timeout_ms: Some(5000),
            approval_bypass: true,
        })
        .unwrap();

    let request = plan.json_rpc_request(7).unwrap();

    assert_json_eq(
        &request,
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"SPEC.md"}}}"#,
    );
}

/// Verifies stdio discovery initializes server and discovers tools.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn stdio_discovery_initializes_server_and_discovers_tools() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::stdio(
            "fixture",
            "fixture",
            "/bin/sh",
            vec![
                "-c".to_string(),
                stdio_fixture_script(false, false).to_string(),
            ],
        ))
        .unwrap();
    let plan = registry.startup_plan("fixture", &BTreeMap::new()).unwrap();

    let discovery = discover_stdio_mcp_server(&plan, &BTreeMap::new(), "mez", "test")
        .await
        .unwrap();

    assert_eq!(discovery.initialize.server_name, "fixture");
    assert_eq!(discovery.tools.len(), 1);
    assert_eq!(discovery.tools[0].name, "echo");
    registry
        .mark_available_from_discovered_tools("fixture", discovery.tools)
        .unwrap();
    let tools = registry.available_tools();
    assert_eq!(tools.len(), 1);
    assert!(tools[0].permission_required);
    assert!(tools[0].input_schema_json.contains(r#""message""#));
}

/// Verifies stdio connection calls permission gated tool.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn stdio_connection_calls_permission_gated_tool() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::stdio(
            "fixture",
            "fixture",
            "/bin/sh",
            vec![
                "-c".to_string(),
                stdio_fixture_script(false, false).to_string(),
            ],
        ))
        .unwrap();
    let plan = registry.startup_plan("fixture", &BTreeMap::new()).unwrap();
    let mut connection = spawn_stdio_mcp_connection(&plan, &BTreeMap::new())
        .await
        .unwrap();
    connection
        .initialize("mez", "test", plan.timeout_ms)
        .await
        .unwrap();
    connection.send_initialized_notification().await.unwrap();
    let tools = connection.list_tools(None, plan.timeout_ms).await.unwrap();
    registry
        .mark_available_from_discovered_tools("fixture", tools.tools)
        .unwrap();
    let call = registry
        .plan_tool_call(&McpToolCallRequest {
            server_id: "fixture".to_string(),
            tool_name: "echo".to_string(),
            arguments_json: r#"{"message":"hello"}"#.to_string(),
            timeout_ms: Some(1000),
            approval_bypass: true,
        })
        .unwrap();

    let response = connection.call_tool(&call).await.unwrap();

    assert!(!response.is_error);
    assert!(response.content_json.contains("hello"));
}

/// Verifies stdio connection times out waiting for response.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn stdio_connection_times_out_waiting_for_response() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::stdio(
            "fixture",
            "fixture",
            "/bin/sh",
            vec![
                "-c".to_string(),
                stdio_fixture_script(true, false).to_string(),
            ],
        ))
        .unwrap();
    let plan = registry.startup_plan("fixture", &BTreeMap::new()).unwrap();
    let mut connection = spawn_stdio_mcp_connection(&plan, &BTreeMap::new())
        .await
        .unwrap();

    let error = connection
        .send_request(
            &build_mcp_default_initialize_request(1, "mez", "test"),
            1,
            1,
        )
        .await
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("timed out"));
}

/// Verifies stdio reader rejects oversized protocol messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn stdio_reader_rejects_oversized_protocol_messages() {
    let data = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n";
    let error = read_bounded_protocol_line(&mut &data[..], 4).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("larger than the configured limit"));
}

/// Verifies stdio spawn passes only declared environment.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn stdio_spawn_passes_only_declared_environment() {
    let mut registry = McpRegistry::default();
    let mut config = McpServerConfig::stdio(
        "fixture",
        "fixture",
        "/bin/sh",
        vec![
            "-c".to_string(),
            stdio_fixture_script(false, true).to_string(),
        ],
    );
    config.env_vars.push("MCP_TOKEN".to_string());
    registry.add_server(config).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert("MCP_TOKEN".to_string(), "ok".to_string());
    environment.insert("SHOULD_NOT_PASS".to_string(), "leaked".to_string());
    let plan = registry.startup_plan("fixture", &environment).unwrap();
    let mut connection = spawn_stdio_mcp_connection(&plan, &environment)
        .await
        .unwrap();

    let response = connection
        .initialize("mez", "test", plan.timeout_ms)
        .await
        .unwrap();

    assert_eq!(response.server_name, "fixture");
}

/// Verifies stdio spawn forwards a usable `PATH` for command lookup while
/// preserving the rest of the explicit environment boundary.
///
/// Many MCP configurations use a command name such as `everything` rather than
/// an absolute path. Clearing the child environment must not make those servers
/// unspawnable when the runtime already has a valid `PATH`.
#[tokio::test]
async fn stdio_spawn_uses_runtime_path_for_command_lookup() {
    let root = std::env::temp_dir().join(format!(
        "mez-mcp-path-{}-{}",
        std::process::id(),
        "command-lookup"
    ));
    fs::create_dir_all(&root).unwrap();
    let executable = root.join("fixture-mcp");
    fs::write(
        &executable,
        format!("#!/bin/sh\n{}\n", stdio_fixture_script(false, false)),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::stdio(
            "fixture",
            "fixture",
            "fixture-mcp",
            Vec::new(),
        ))
        .unwrap();
    let mut environment = BTreeMap::new();
    environment.insert("PATH".to_string(), root.to_string_lossy().to_string());
    environment.insert("SHOULD_NOT_PASS".to_string(), "leaked".to_string());
    let plan = registry.startup_plan("fixture", &environment).unwrap();

    let mut connection = spawn_stdio_mcp_connection(&plan, &environment)
        .await
        .unwrap();
    let response = connection
        .initialize("mez", "test", plan.timeout_ms)
        .await
        .unwrap();

    assert_eq!(response.server_name, "fixture");
    let _ = fs::remove_dir_all(root);
}

/// Verifies stdio discovery into registry blacklists failed server.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn stdio_discovery_into_registry_blacklists_failed_server() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::stdio(
            "fixture",
            "fixture",
            "/bin/sh",
            vec!["-c".to_string(), stdio_error_script().to_string()],
        ))
        .unwrap();

    let error = discover_stdio_mcp_server_into_registry(
        &mut registry,
        "fixture",
        &BTreeMap::new(),
        "mez",
        "test",
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(
        registry.list_servers()[0].status,
        McpServerStatus::Blacklisted
    );
    assert!(
        registry.list_servers()[0]
            .blacklist_reason
            .as_deref()
            .unwrap_or_default()
            .contains("startup refused")
    );
}

/// Verifies stdio tool call writes start and completion audit records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn stdio_tool_call_writes_start_and_completion_audit_records() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::stdio(
            "fixture",
            "fixture",
            "/bin/sh",
            vec![
                "-c".to_string(),
                stdio_fixture_script(false, false).to_string(),
            ],
        ))
        .unwrap();
    let plan = registry.startup_plan("fixture", &BTreeMap::new()).unwrap();
    let mut connection = spawn_stdio_mcp_connection(&plan, &BTreeMap::new())
        .await
        .unwrap();
    connection
        .initialize("mez", "test", plan.timeout_ms)
        .await
        .unwrap();
    connection.send_initialized_notification().await.unwrap();
    let tools = connection.list_tools(None, plan.timeout_ms).await.unwrap();
    registry
        .mark_available_from_discovered_tools("fixture", tools.tools)
        .unwrap();
    let call = registry
        .plan_tool_call(&McpToolCallRequest {
            server_id: "fixture".to_string(),
            tool_name: "echo".to_string(),
            arguments_json: r#"{"message":"hello"}"#.to_string(),
            timeout_ms: Some(1000),
            approval_bypass: true,
        })
        .unwrap();
    let audit_dir =
        std::env::temp_dir().join(format!("mez-mcp-audit-{}-stdio", std::process::id()));
    let _ = std::fs::remove_dir_all(&audit_dir);
    let path = audit_dir.join("audit.jsonl");
    let mut audit_log = crate::audit::AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: false,
        required: true,
    });

    let response = call_stdio_mcp_tool_with_audit(
        &mut connection,
        &call,
        &mut audit_log,
        "$1",
        crate::audit::AuditActor {
            kind: "agent".to_string(),
            id: "agent-1".to_string(),
        },
        "call-1",
    )
    .await
    .unwrap();

    assert!(!response.is_error);
    let audit = std::fs::read_to_string(&path).unwrap();
    assert!(audit.contains(r#""outcome":"started""#));
    assert!(audit.contains(r#""outcome":"succeeded""#));
    assert!(audit.contains(r#""server_id":"fixture""#));
    let _ = std::fs::remove_dir_all(audit_dir);
}

/// Verifies streamable http initialize posts standard headers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn streamable_http_initialize_posts_standard_headers() {
    let body = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"http-fixture","version":"1.0.0"}}}"#;
    let (url, request) = spawn_http_fixture("application/json", body, Some("session-1"));
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::streamable_http(
            "http",
            "http-fixture",
            url,
        ))
        .unwrap();
    let plan = registry.startup_plan("http", &BTreeMap::new()).unwrap();

    let (initialize, session_id) =
        initialize_streamable_http_mcp_server(&plan, &BTreeMap::new(), "mez", "test")
            .await
            .unwrap();
    let request = request.join().unwrap();

    assert_eq!(initialize.server_name, "http-fixture");
    assert_eq!(session_id.as_deref(), Some("session-1"));
    assert!(fixture_request_has_header(
        &request,
        "Mcp-Method",
        "initialize"
    ));
    assert!(fixture_request_has_header(
        &request,
        "Accept",
        "application/json, text/event-stream"
    ));
    assert!(request.contains(r#""method":"initialize""#));
}

/// Verifies streamable http tool call posts name bearer and session headers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn streamable_http_tool_call_posts_name_bearer_and_session_headers() {
    let body = r#"{"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}"#;
    let (url, request) = spawn_http_fixture("application/json", body, None);
    let mut registry = McpRegistry::default();
    let mut config = McpServerConfig::streamable_http("http", "http-fixture", url);
    config.bearer_token_env = Some("MCP_TOKEN".to_string());
    registry.add_server(config).unwrap();
    let mut environment = BTreeMap::new();
    environment.insert("MCP_TOKEN".to_string(), "secret".to_string());
    let plan = registry.startup_plan("http", &environment).unwrap();
    let call = McpToolCallPlan {
        server_id: "http".to_string(),
        tool_name: "echo".to_string(),
        arguments_json: r#"{"message":"hello"}"#.to_string(),
        timeout_ms: 1000,
        approval_required: false,
        audit_event_class: "external_integration",
        effects: McpToolEffects::none(),
    };

    let response = call_streamable_http_mcp_tool(&plan, &environment, &call, 9, Some("session-1"))
        .await
        .unwrap();
    let request = request.join().unwrap();

    assert!(!response.is_error);
    assert!(fixture_request_has_header(
        &request,
        "Authorization",
        "Bearer secret"
    ));
    assert!(fixture_request_has_header(&request, "Mcp-Name", "echo"));
    assert!(fixture_request_has_header(
        &request,
        "MCP-Session-Id",
        "session-1"
    ));
}

/// Verifies streamable http discovery into registry blacklists failed server.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn streamable_http_discovery_into_registry_blacklists_failed_server() {
    let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"startup refused"}}"#;
    let (url, _request) = spawn_http_fixture("application/json", body, None);
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::streamable_http(
            "http",
            "http-fixture",
            url,
        ))
        .unwrap();

    let error = discover_streamable_http_mcp_server_into_registry(
        &mut registry,
        "http",
        &BTreeMap::new(),
        "mez",
        "test",
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(
        registry.list_servers()[0].status,
        McpServerStatus::Blacklisted
    );
    assert!(
        registry.list_servers()[0]
            .blacklist_reason
            .as_deref()
            .unwrap_or_default()
            .contains("startup refused")
    );
}

/// Verifies streamable http tool call writes start and completion audit records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn streamable_http_tool_call_writes_start_and_completion_audit_records() {
    let body = r#"{"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}"#;
    let (url, _request) = spawn_http_fixture("application/json", body, None);
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::streamable_http(
            "http",
            "http-fixture",
            url,
        ))
        .unwrap();
    let plan = registry.startup_plan("http", &BTreeMap::new()).unwrap();
    let call = McpToolCallPlan {
        server_id: "http".to_string(),
        tool_name: "echo".to_string(),
        arguments_json: r#"{"message":"hello"}"#.to_string(),
        timeout_ms: 1000,
        approval_required: false,
        audit_event_class: "external_integration",
        effects: McpToolEffects::none(),
    };
    let audit_dir = std::env::temp_dir().join(format!("mez-mcp-audit-{}-http", std::process::id()));
    let _ = std::fs::remove_dir_all(&audit_dir);
    let path = audit_dir.join("audit.jsonl");
    let mut audit_log = crate::audit::AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: false,
        required: true,
    });

    let response = call_streamable_http_mcp_tool_with_audit(
        &plan,
        &BTreeMap::new(),
        &call,
        9,
        Some("session-1"),
        McpToolAuditCallContext {
            audit_log: &mut audit_log,
            mezzanine_session_id: "$1",
            actor: crate::audit::AuditActor {
                kind: "agent".to_string(),
                id: "agent-1".to_string(),
            },
            call_id: "call-1",
        },
    )
    .await
    .unwrap();

    assert!(!response.is_error);
    let audit = std::fs::read_to_string(&path).unwrap();
    assert!(audit.contains(r#""outcome":"started""#));
    assert!(audit.contains(r#""outcome":"succeeded""#));
    assert!(audit.contains(r#""server_id":"http""#));
    let _ = std::fs::remove_dir_all(audit_dir);
}

/// Verifies streamable http extracts sse json rpc response.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn streamable_http_extracts_sse_json_rpc_response() {
    let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"sse\",\"version\":\"1.0.0\"}}}\n\n";
    let (url, _request) = spawn_http_fixture("text/event-stream", body, None);
    let mut registry = McpRegistry::default();
    registry
        .add_server(McpServerConfig::streamable_http("http", "sse", url))
        .unwrap();
    let plan = registry.startup_plan("http", &BTreeMap::new()).unwrap();

    let (initialize, _session_id) =
        initialize_streamable_http_mcp_server(&plan, &BTreeMap::new(), "mez", "test")
            .await
            .unwrap();

    assert_eq!(initialize.server_name, "sse");
}

/// Runs the stdio fixture script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn stdio_fixture_script(sleep_forever: bool, assert_environment: bool) -> &'static str {
    if sleep_forever {
        return "while IFS= read -r line; do sleep 1; done";
    }
    if assert_environment {
        return r#"while IFS= read -r line; do
case "$line" in
  *initialize*)
if [ "${MCP_TOKEN:-}" = "ok" ] && [ -z "${SHOULD_NOT_PASS:-}" ]; then
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1.0.0"}}}'
else
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"bad environment"}}'
fi
;;
esac
done"#;
    }
    r#"while IFS= read -r line; do
case "$line" in
  *initialize*)
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1.0.0"},"instructions":"fixture server"}}'
;;
  *notifications/initialized*)
;;
  *tools/list*)
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo a message","inputSchema":{"type":"object","properties":{"message":{"type":"string"}}}}]}}'
;;
  *tools/call*)
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hello"}],"isError":false}}'
;;
esac
done"#
}

/// Runs the stdio error script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn stdio_error_script() -> &'static str {
    r#"while IFS= read -r line; do
case "$line" in
  *initialize*)
printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"startup refused"}}'
;;
esac
done"#
}

/// Runs the spawn http fixture operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_http_fixture(
    content_type: &'static str,
    body: &'static str,
    session_id: Option<&'static str>,
) -> (String, std::thread::JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_fixture_http_request(&mut stream);
        let session = session_id
            .map(|value| format!("MCP-Session-Id: {value}\r\n"))
            .unwrap_or_default();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        request
    });
    (url, handle)
}

/// Runs the read fixture http request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn read_fixture_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if fixture_http_request_complete(&bytes) {
            break;
        }
    }
    String::from_utf8(bytes).unwrap()
}

/// Runs the fixture http request complete operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn fixture_http_request_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let header_text = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}

/// Runs the fixture request has header operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn fixture_request_has_header(request: &str, name: &str, expected_value: &str) -> bool {
    request.lines().any(|line| {
        let Some((header_name, value)) = line.split_once(':') else {
            return false;
        };
        header_name.eq_ignore_ascii_case(name) && value.trim() == expected_value
    })
}
