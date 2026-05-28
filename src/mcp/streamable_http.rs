//! Streamable HTTP transport support for MCP servers.
//!
//! This module prepares MCP HTTP headers, executes bounded HTTP POSTs,
//! handles JSON and SSE responses, performs discovery, and calls remote tools.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::error::{MezError, Result};

use super::protocol::{
    build_mcp_default_initialize_request, build_mcp_initialized_notification,
    build_mcp_tools_list_request, json_id_matches, object_field, parse_mcp_initialize_response,
    parse_mcp_json, parse_mcp_tools_call_response, parse_mcp_tools_list_response, string_field,
};
use super::registry::McpRegistry;
use super::types::{
    DEFAULT_MCP_MAX_MESSAGE_BYTES, DEFAULT_MCP_PROTOCOL_VERSION, McpInitializeResponse,
    McpStartupPlan, McpStartupTransportPlan, McpStreamableHttpDiscovery, McpStreamableHttpResponse,
    McpToolCallPlan, McpToolCallResponse, McpToolListPagination,
};

/// Runs the execute streamable http exchange operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn execute_streamable_http_exchange(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    request_body: &str,
    expected_id: Option<u64>,
    timeout_ms: u64,
    session_id: Option<&str>,
    oauth_bearer_token: Option<&str>,
) -> Result<McpStreamableHttpResponse> {
    let McpStartupTransportPlan::StreamableHttp {
        url,
        headers,
        bearer_token_env,
    } = &plan.transport
    else {
        return Err(MezError::invalid_args(
            "MCP startup plan does not use streamable HTTP transport",
        ));
    };
    let (mcp_method, mcp_name) = mcp_standard_headers_from_body(request_body)?;
    let mut request_headers = headers.clone();
    if let Some(token_env) = bearer_token_env {
        let token = environment.get(token_env).ok_or_else(|| {
            MezError::forbidden(format!(
                "missing required MCP bearer token environment: {token_env}"
            ))
        })?;
        request_headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    } else if let Some(token) = oauth_bearer_token {
        request_headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    }

    request_headers.insert(
        "Accept".to_string(),
        "application/json, text/event-stream".to_string(),
    );
    request_headers.insert("Content-Type".to_string(), "application/json".to_string());
    request_headers.insert(
        "MCP-Protocol-Version".to_string(),
        DEFAULT_MCP_PROTOCOL_VERSION.to_string(),
    );
    request_headers.insert("Mcp-Method".to_string(), mcp_method);
    if let Some(name) = mcp_name {
        request_headers.insert("Mcp-Name".to_string(), name);
    }
    if let Some(session_id) = session_id {
        request_headers.insert("MCP-Session-Id".to_string(), session_id.to_string());
    }

    let mut response = execute_streamable_http_post(
        url,
        &request_headers,
        request_body,
        Duration::from_millis(timeout_ms),
    )
    .await?;
    let content_type = header_value(&response.headers, "content-type").unwrap_or_default();
    if !(200..300).contains(&response.status_code) {
        return Err(MezError::invalid_state(format!(
            "streamable HTTP MCP server returned status {}",
            response.status_code
        )));
    }
    if response.status_code == 202 && expected_id.is_none() {
        response.protocol_body.clear();
        return Ok(response);
    }
    if content_type.contains("text/event-stream") {
        let expected_id = expected_id.ok_or_else(|| {
            MezError::invalid_state(
                "streamable HTTP MCP SSE response requires a JSON-RPC request id",
            )
        })?;
        response.protocol_body =
            extract_sse_json_rpc_response(&response.protocol_body, expected_id)?;
    }
    Ok(response)
}

/// Runs the initialize streamable http mcp server operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn initialize_streamable_http_mcp_server(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    client_name: &str,
    client_version: &str,
) -> Result<(McpInitializeResponse, Option<String>)> {
    initialize_streamable_http_mcp_server_with_auth_token(
        plan,
        environment,
        client_name,
        client_version,
        None,
    )
    .await
}

/// Runs the initialize streamable http mcp server with auth token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn initialize_streamable_http_mcp_server_with_auth_token(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    client_name: &str,
    client_version: &str,
    oauth_bearer_token: Option<&str>,
) -> Result<(McpInitializeResponse, Option<String>)> {
    let id = 1;
    let request = build_mcp_default_initialize_request(id, client_name, client_version);
    let response = execute_streamable_http_exchange(
        plan,
        environment,
        &request,
        Some(id),
        plan.timeout_ms,
        None,
        oauth_bearer_token,
    )
    .await?;
    let initialize = parse_mcp_initialize_response(&response.protocol_body, id)?;
    Ok((initialize, response.session_id))
}

/// Runs the discover streamable http mcp server operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn discover_streamable_http_mcp_server(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    client_name: &str,
    client_version: &str,
) -> Result<McpStreamableHttpDiscovery> {
    discover_streamable_http_mcp_server_with_auth_token(
        plan,
        environment,
        client_name,
        client_version,
        None,
    )
    .await
}

/// Runs the discover streamable http mcp server with auth token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn discover_streamable_http_mcp_server_with_auth_token(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    client_name: &str,
    client_version: &str,
    oauth_bearer_token: Option<&str>,
) -> Result<McpStreamableHttpDiscovery> {
    let (initialize, mut session_id) = initialize_streamable_http_mcp_server_with_auth_token(
        plan,
        environment,
        client_name,
        client_version,
        oauth_bearer_token,
    )
    .await?;
    let notification = build_mcp_initialized_notification();
    let initialized = execute_streamable_http_exchange(
        plan,
        environment,
        &notification,
        None,
        plan.timeout_ms,
        session_id.as_deref(),
        oauth_bearer_token,
    )
    .await?;
    if initialized.session_id.is_some() {
        session_id = initialized.session_id;
    }

    let mut tools = Vec::new();
    if initialize.supports_tools {
        let mut cursor = None;
        let mut request_id = 2;
        let mut pagination = McpToolListPagination::default();
        loop {
            let request = build_mcp_tools_list_request(request_id, cursor.as_deref());
            let response = execute_streamable_http_exchange(
                plan,
                environment,
                &request,
                Some(request_id),
                plan.timeout_ms,
                session_id.as_deref(),
                oauth_bearer_token,
            )
            .await?;
            if response.session_id.is_some() {
                session_id = response.session_id.clone();
            }
            let listed = parse_mcp_tools_list_response(&response.protocol_body, request_id)?;
            tools.extend(listed.tools);
            let Some(next_cursor) = pagination.advance(&plan.server_id, listed.next_cursor)? else {
                break;
            };
            cursor = Some(next_cursor);
            request_id += 1;
        }
    }

    Ok(McpStreamableHttpDiscovery {
        initialize,
        tools,
        session_id,
    })
}

/// Runs the discover streamable http mcp server into registry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn discover_streamable_http_mcp_server_into_registry(
    registry: &mut McpRegistry,
    server_id: &str,
    environment: &BTreeMap<String, String>,
    client_name: &str,
    client_version: &str,
) -> Result<McpStreamableHttpDiscovery> {
    let plan = registry.startup_plan(server_id, environment)?;
    match discover_streamable_http_mcp_server(&plan, environment, client_name, client_version).await
    {
        Ok(discovery) => {
            registry.mark_available_from_discovered_tools(server_id, discovery.tools.clone())?;
            Ok(discovery)
        }
        Err(error) => {
            let reason = error.message().to_string();
            let _ = registry.blacklist_for_session(server_id, reason);
            Err(error)
        }
    }
}

/// Runs the call streamable http mcp tool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn call_streamable_http_mcp_tool(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    tool_call: &McpToolCallPlan,
    request_id: u64,
    session_id: Option<&str>,
) -> Result<McpToolCallResponse> {
    let request = tool_call.json_rpc_request(request_id)?;
    let response = execute_streamable_http_exchange(
        plan,
        environment,
        &request,
        Some(request_id),
        tool_call.timeout_ms,
        session_id,
        None,
    )
    .await?;
    parse_mcp_tools_call_response(&response.protocol_body, request_id)
}

/// Runs the execute streamable http post operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn execute_streamable_http_post(
    url: &str,
    headers: &BTreeMap<String, String>,
    body: &str,
    timeout: Duration,
) -> Result<McpStreamableHttpResponse> {
    let mut request_headers = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| MezError::invalid_args("streamable HTTP MCP header name is invalid"))?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| MezError::invalid_args("streamable HTTP MCP header value is invalid"))?;
        request_headers.insert(name, value);
    }

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| {
            MezError::invalid_state(format!("streamable HTTP MCP client setup failed: {error}"))
        })?;
    let mut response = client
        .post(url)
        .headers(request_headers)
        .body(body.to_string())
        .send()
        .await
        .map_err(|error| {
            MezError::invalid_state(format!("streamable HTTP MCP request failed: {error}"))
        })?;

    let status_code = response.status().as_u16();
    let headers = response_headers(response.headers())?;
    let mut body_bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        MezError::invalid_state(format!("streamable HTTP MCP response read failed: {error}"))
    })? {
        if body_bytes.len().saturating_add(chunk.len()) > DEFAULT_MCP_MAX_MESSAGE_BYTES {
            return Err(MezError::invalid_state(
                "streamable HTTP MCP response exceeded the configured limit",
            ));
        }
        body_bytes.extend_from_slice(&chunk);
    }
    let protocol_body = String::from_utf8(body_bytes)
        .map_err(|_| MezError::invalid_state("streamable HTTP MCP response body is not UTF-8"))?;
    let session_id = header_value(&headers, "mcp-session-id");
    Ok(McpStreamableHttpResponse {
        status_code,
        headers,
        protocol_body,
        session_id,
    })
}

/// Runs the response headers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn response_headers(headers: &reqwest::header::HeaderMap) -> Result<BTreeMap<String, String>> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = value.to_str().map_err(|_| {
                MezError::invalid_state("streamable HTTP MCP response header is not UTF-8")
            })?;
            Ok((name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

/// Runs the mcp standard headers from body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_standard_headers_from_body(body: &str) -> Result<(String, Option<String>)> {
    let body = parse_mcp_json(body, "MCP JSON-RPC request")?;
    let method = string_field(&body, "method")
        .ok_or_else(|| MezError::invalid_args("MCP JSON-RPC request is missing method"))?;
    let name = object_field(&body, "params")
        .and_then(|params| string_field(params, "name").or_else(|| string_field(params, "uri")));
    Ok((method, name))
}

/// Runs the header value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn header_value(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    headers.get(&name.to_ascii_lowercase()).cloned()
}

/// Runs the extract sse json rpc response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn extract_sse_json_rpc_response(body: &str, expected_id: u64) -> Result<String> {
    let expected = expected_id.to_string();
    let mut data = String::new();
    for line in body.lines().chain(std::iter::once("")) {
        if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
            continue;
        }
        if !line.trim().is_empty() {
            continue;
        }
        if data.is_empty() {
            continue;
        }
        if json_id_matches(&data, expected.as_str()) {
            return Ok(data);
        }
        data.clear();
    }
    Err(MezError::invalid_state(
        "streamable HTTP MCP SSE response did not contain the expected JSON-RPC id",
    ))
}
