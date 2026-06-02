//! MCP JSON-RPC request builders and response parsers.
//!
//! Protocol helpers construct initialize/tools JSON-RPC requests, parse server
//! responses, and provide shared JSON field utilities used by transports.

use serde_json::{Value, json};

use crate::error::{MezError, Result};

use super::types::{
    DEFAULT_MCP_PROTOCOL_VERSION, McpDiscoveredTool, McpInitializeResponse, McpToolCallPlan,
    McpToolCallResponse, McpToolsListResponse,
};

/// Runs the build mcp initialize request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_mcp_initialize_request(
    id: u64,
    client_name: &str,
    client_version: &str,
    protocol_version: &str,
) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": {
                "name": client_name,
                "version": client_version,
            },
        },
    })
    .to_string()
}

/// Runs the build mcp default initialize request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_mcp_default_initialize_request(
    id: u64,
    client_name: &str,
    client_version: &str,
) -> String {
    build_mcp_initialize_request(
        id,
        client_name,
        client_version,
        DEFAULT_MCP_PROTOCOL_VERSION,
    )
}

/// Runs the build mcp tools list request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_mcp_tools_list_request(id: u64, cursor: Option<&str>) -> String {
    let params = cursor
        .map(|cursor| json!({ "cursor": cursor }))
        .unwrap_or_else(|| json!({}));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/list",
        "params": params,
    })
    .to_string()
}

/// Runs the build mcp initialized notification operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_mcp_initialized_notification() -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    })
    .to_string()
}

/// One MCP JSON-RPC request paired with its response parser.
///
/// Transports keep connection-specific exchange policy outside this type while
/// sharing the request id, serialized body, timeout, and typed response parsing
/// that make up one protocol operation lifecycle.
pub(crate) struct McpJsonRpcOperation<T> {
    id: u64,
    request_body: String,
    timeout_ms: u64,
    parse_response: fn(&str, u64) -> Result<T>,
}

impl<T> McpJsonRpcOperation<T> {
    /// Returns the JSON-RPC id that callers should expect in the response.
    pub(crate) fn request_id(&self) -> u64 {
        self.id
    }

    /// Returns the serialized JSON-RPC request body to send over a transport.
    pub(crate) fn request_body(&self) -> &str {
        &self.request_body
    }

    /// Returns the timeout budget associated with this protocol operation.
    pub(crate) fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Parses a transport response body using this operation's expected id.
    pub(crate) fn parse_response(&self, body: &str) -> Result<T> {
        (self.parse_response)(body, self.id)
    }
}

/// Builds one typed MCP initialize JSON-RPC operation.
pub(crate) fn mcp_initialize_operation(
    id: u64,
    client_name: &str,
    client_version: &str,
    timeout_ms: u64,
) -> McpJsonRpcOperation<McpInitializeResponse> {
    McpJsonRpcOperation {
        id,
        request_body: build_mcp_default_initialize_request(id, client_name, client_version),
        timeout_ms,
        parse_response: parse_mcp_initialize_response,
    }
}

/// Builds one typed MCP tools/list JSON-RPC operation.
pub(crate) fn mcp_tools_list_operation(
    id: u64,
    cursor: Option<&str>,
    timeout_ms: u64,
) -> McpJsonRpcOperation<McpToolsListResponse> {
    McpJsonRpcOperation {
        id,
        request_body: build_mcp_tools_list_request(id, cursor),
        timeout_ms,
        parse_response: parse_mcp_tools_list_response,
    }
}

/// Builds one typed MCP tools/call JSON-RPC operation.
pub(crate) fn mcp_tools_call_operation(
    id: u64,
    plan: &McpToolCallPlan,
) -> Result<McpJsonRpcOperation<McpToolCallResponse>> {
    Ok(McpJsonRpcOperation {
        id,
        request_body: plan.json_rpc_request(id)?,
        timeout_ms: plan.timeout_ms,
        parse_response: parse_mcp_tools_call_response,
    })
}

/// Runs the build mcp tools call request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_mcp_tools_call_request(
    id: u64,
    tool_name: &str,
    arguments_json: &str,
) -> Result<String> {
    if tool_name.trim().is_empty() {
        return Err(MezError::invalid_args("MCP tool name must not be empty"));
    }
    let arguments = parse_mcp_json(arguments_json, "MCP tools/call arguments")?;
    if !arguments.is_object() {
        return Err(MezError::invalid_args(
            "MCP tools/call arguments must be a JSON object",
        ));
    }
    Ok(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments,
        },
    })
    .to_string())
}

/// Runs the parse mcp initialize response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_mcp_initialize_response(
    body: &str,
    expected_id: u64,
) -> Result<McpInitializeResponse> {
    let result = mcp_response_result(body, expected_id)?;
    let server_info = object_field(&result, "serverInfo")
        .ok_or_else(|| MezError::invalid_state("MCP initialize response is missing serverInfo"))?;
    let capabilities = object_field(&result, "capabilities").ok_or_else(|| {
        MezError::invalid_state("MCP initialize response is missing capabilities")
    })?;
    Ok(McpInitializeResponse {
        protocol_version: string_field(&result, "protocolVersion").ok_or_else(|| {
            MezError::invalid_state("MCP initialize response is missing protocolVersion")
        })?,
        server_name: string_field(server_info, "name").ok_or_else(|| {
            MezError::invalid_state("MCP initialize response is missing serverInfo.name")
        })?,
        server_version: string_field(server_info, "version").ok_or_else(|| {
            MezError::invalid_state("MCP initialize response is missing serverInfo.version")
        })?,
        instructions: string_field(&result, "instructions"),
        supports_tools: capabilities.get("tools").is_some(),
    })
}

/// Runs the parse mcp tools list response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_mcp_tools_list_response(body: &str, expected_id: u64) -> Result<McpToolsListResponse> {
    let result = mcp_response_result(body, expected_id)?;
    let tools_json = array_field(&result, "tools")
        .ok_or_else(|| MezError::invalid_state("MCP tools/list response is missing tools"))?;
    let mut tools = Vec::new();
    for tool_json in tools_json {
        tools.push(McpDiscoveredTool {
            name: string_field(tool_json, "name")
                .ok_or_else(|| MezError::invalid_state("MCP tools/list tool is missing name"))?,
            title: string_field(tool_json, "title"),
            description: string_field(tool_json, "description").unwrap_or_default(),
            input_schema_json: object_field(tool_json, "inputSchema")
                .ok_or_else(|| {
                    MezError::invalid_state("MCP tools/list tool is missing inputSchema")
                })?
                .to_string(),
        });
    }
    Ok(McpToolsListResponse {
        tools,
        next_cursor: string_field(&result, "nextCursor"),
    })
}

/// Runs the parse mcp tools call response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_mcp_tools_call_response(body: &str, expected_id: u64) -> Result<McpToolCallResponse> {
    let result = mcp_response_result(body, expected_id)?;
    let content_json = array_field(&result, "content")
        .ok_or_else(|| MezError::invalid_state("MCP tools/call response is missing content"))?;
    Ok(McpToolCallResponse {
        content_json: Value::Array(content_json.clone()).to_string(),
        structured_content_json: object_field(&result, "structuredContent").map(Value::to_string),
        is_error: bool_field(&result, "isError").unwrap_or(false),
    })
}

/// Runs the mcp response result operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_response_result(body: &str, expected_id: u64) -> Result<Value> {
    let body = parse_mcp_json(body, "MCP response")?;
    if let Some(error) = object_field(&body, "error") {
        let message = string_field(error, "message")
            .unwrap_or_else(|| "MCP server returned a protocol error".to_string());
        return Err(MezError::invalid_state(format!(
            "MCP protocol error: {message}"
        )));
    }
    match string_field(&body, "jsonrpc").as_deref() {
        Some("2.0") => {}
        _ => {
            return Err(MezError::invalid_state(
                "MCP response must use JSON-RPC 2.0",
            ));
        }
    }
    let id = json_value_id(&body)
        .ok_or_else(|| MezError::invalid_state("MCP response is missing JSON-RPC id"))?;
    if id != expected_id.to_string() {
        return Err(MezError::invalid_state(
            "MCP response id does not match request",
        ));
    }
    object_field(&body, "result")
        .cloned()
        .ok_or_else(|| MezError::invalid_state("MCP response is missing result"))
}

/// Runs the parse mcp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_mcp_json(body: &str, context: &str) -> Result<Value> {
    serde_json::from_str(body)
        .map_err(|error| MezError::invalid_state(format!("{context} is not valid JSON: {error}")))
}

/// Runs the string field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(ToString::to_string)
}

/// Runs the bool field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn bool_field(value: &Value, field: &str) -> Option<bool> {
    value.get(field)?.as_bool()
}

/// Runs the object field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn object_field<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    value.get(field).filter(|field| field.is_object())
}

/// Runs the array field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn array_field<'a>(value: &'a Value, field: &str) -> Option<&'a Vec<Value>> {
    value.get(field)?.as_array()
}

/// Runs the json id matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_id_matches(body: &str, expected: &str) -> bool {
    parse_mcp_json(body, "MCP JSON-RPC message")
        .ok()
        .and_then(|value| json_value_id(&value))
        .as_deref()
        == Some(expected)
}

/// Runs the json value id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_value_id(value: &Value) -> Option<String> {
    match value.get("id")? {
        Value::Number(number) => Some(number.to_string()),
        Value::String(value) => Some(value.clone()),
        _ => None,
    }
}
