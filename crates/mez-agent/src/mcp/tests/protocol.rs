//! JSON-RPC construction, parsing, pagination, and plan serialization tests.

use serde_json::Value;

use super::super::{
    DEFAULT_MCP_MAX_TOOL_LIST_PAGES, McpErrorKind, McpRegistry, McpToolCallRequest,
    McpToolListPagination, build_mcp_initialize_request, build_mcp_tools_call_request,
    build_mcp_tools_list_request, parse_mcp_initialize_response, parse_mcp_tools_call_response,
    parse_mcp_tools_list_response,
};
use super::{config, tool};

const NOW: u64 = 1_700_000_000;

/// Compares JSON payloads by structure so field order remains irrelevant.
fn assert_json_eq(actual: &str, expected: &str) {
    let actual: Value = serde_json::from_str(actual).unwrap();
    let expected: Value = serde_json::from_str(expected).unwrap();
    assert_eq!(actual, expected);
}

/// Verifies initialize and tool-list builders emit MCP JSON-RPC shapes.
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

/// Verifies tool-call builders require and embed a JSON object argument payload.
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
        McpErrorKind::InvalidArgs
    );
}

/// Verifies initialize responses preserve capability and server metadata.
#[test]
fn parses_initialize_response_capabilities() {
    let response = parse_mcp_initialize_response(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fs","version":"1.2.3"},"instructions":"Use carefully"}}"#, 1).unwrap();
    assert_eq!(response.protocol_version, "2025-11-25");
    assert_eq!(response.server_name, "fs");
    assert_eq!(response.server_version, "1.2.3");
    assert_eq!(response.instructions.as_deref(), Some("Use carefully"));
    assert!(response.supports_tools);
}

/// Verifies tool-list responses preserve schemas and continuation cursors.
#[test]
fn parses_tools_list_response_with_schema_and_cursor() {
    let response = parse_mcp_tools_list_response(r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"read_file","title":"Read File","description":"Read a file","inputSchema":{"type":"object","properties":{"path":{"type":"string"}}}}],"nextCursor":"more"}}"#, 2).unwrap();
    assert_eq!(response.tools.len(), 1);
    assert_eq!(response.tools[0].name, "read_file");
    assert_eq!(response.tools[0].title.as_deref(), Some("Read File"));
    assert!(response.tools[0].input_schema_json.contains(r#""path""#));
    assert_eq!(response.next_cursor.as_deref(), Some("more"));
}

/// Verifies repeated continuation cursors terminate discovery deterministically.
#[test]
fn mcp_tool_list_pagination_rejects_repeated_cursor() {
    let mut pagination = McpToolListPagination::default();
    assert_eq!(
        pagination
            .advance("fixture", Some("again".to_string()))
            .unwrap()
            .as_deref(),
        Some("again")
    );
    let error = pagination
        .advance("fixture", Some("again".to_string()))
        .unwrap_err();
    assert_eq!(error.kind(), McpErrorKind::InvalidState);
    assert!(error.message().contains("repeated tools/list cursor"));
}

/// Verifies fresh continuation cursors remain bounded by a hard page cap.
#[test]
fn mcp_tool_list_pagination_rejects_excessive_pages() {
    let mut pagination = McpToolListPagination::default();
    for index in 0..DEFAULT_MCP_MAX_TOOL_LIST_PAGES {
        pagination
            .advance("fixture", Some(format!("cursor-{index}")))
            .unwrap();
    }
    let error = pagination
        .advance("fixture", Some("cursor-over-limit".to_string()))
        .unwrap_err();
    assert_eq!(error.kind(), McpErrorKind::InvalidState);
    assert!(error.message().contains("tools/list page limit"));
}

/// Verifies tool-call responses preserve content, structured data, and error status.
#[test]
fn parses_tools_call_response_content_and_tool_error_flag() {
    let response = parse_mcp_tools_call_response(r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"denied"}],"structuredContent":{"status":"denied"},"isError":true}}"#, 3).unwrap();
    assert!(response.content_json.contains(r#""denied""#));
    assert_eq!(
        response.structured_content_json.as_deref(),
        Some(r#"{"status":"denied"}"#)
    );
    assert!(response.is_error);
}

/// Verifies JSON-RPC error responses become typed protocol-state errors.
#[test]
fn protocol_error_response_is_rejected() {
    let error = parse_mcp_tools_call_response(
        r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Unknown tool"}}"#,
        3,
    )
    .unwrap_err();
    assert_eq!(error.kind(), McpErrorKind::InvalidState);
    assert!(error.message().contains("Unknown tool"));
}

/// Verifies a canonical tool-call plan serializes directly as JSON-RPC.
#[test]
fn planned_tool_call_can_be_serialized_as_json_rpc() {
    let mut registry = McpRegistry::default();
    registry.add_server(config()).unwrap();
    registry.mark_available("fs", vec![tool()], NOW).unwrap();
    let plan = registry
        .plan_tool_call(&McpToolCallRequest {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            arguments_json: r#"{"path":"SPEC.md"}"#.to_string(),
            timeout_ms: Some(5000),
            approval_bypass: true,
        })
        .unwrap();
    assert_json_eq(
        &plan.json_rpc_request(7).unwrap(),
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"SPEC.md"}}}"#,
    );
}
