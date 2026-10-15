//! Runtime-owned network MAAP action execution.
//!
//! `web_search` and `fetch_url` are semantic actions, but they do not need pane
//! shell state. This module keeps their permission-facing plans and HTTP
//! execution separate from shell-backed local actions so the runtime can service
//! external content requests without emitting generated shell commands.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use crate::integrations::http::{HttpCompletion, HttpExchangeResponse};
use mez_agent::{ProviderHttpResult, network_action_structured_content_json};

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnRecord,
    AsyncProviderHttpTransport, MezError, ProviderHttpRequest, Result,
};

/// Default response bytes exposed to a `fetch_url` action when the model does
/// not request a smaller bounded body.
const DEFAULT_FETCH_URL_MAX_BYTES: usize = 16 * 1024;
/// Hard response-body cap for one `fetch_url` action, even when the model asks
/// for a larger value.
const MAX_FETCH_URL_MAX_BYTES: usize = 256 * 1024;
/// Timeout applied to runtime-owned network actions.
const NETWORK_ACTION_TIMEOUT_MS: u64 = 30_000;

/// Immutable web-search inputs retained while one request streams safely.
struct WebSearchRequest<'a> {
    query: &'a str,
    domains: &'a [String],
    recency_days: Option<u64>,
    max_results: Option<u64>,
}

/// Executes a network-backed semantic action through a runtime HTTP transport.
pub async fn execute_network_action_with_transport_async<T: AsyncProviderHttpTransport>(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    transport: &T,
) -> Result<ActionResult> {
    let mut ignore_progress = |_| {};
    execute_network_action_with_transport_async_with_progress(
        turn,
        action,
        transport,
        &mut ignore_progress,
    )
    .await
}

/// Executes a network-backed semantic action while publishing safe cumulative
/// presentation source snapshots.
///
/// Progress remains presentation-only: callers retain sole authority over its
/// attempt fencing and final action-result settlement.
pub async fn execute_network_action_with_transport_async_with_progress<
    T: AsyncProviderHttpTransport,
    F: FnMut(String) + Send,
>(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    transport: &T,
    on_progress: &mut F,
) -> Result<ActionResult> {
    match &action.payload {
        AgentActionPayload::FetchUrl { url, max_bytes, .. } => {
            execute_fetch_url_action(turn, action, transport, url, *max_bytes, on_progress).await
        }
        AgentActionPayload::WebSearch {
            query,
            domains,
            recency_days,
            max_results,
        } => {
            execute_web_search_action(
                turn,
                action,
                transport,
                WebSearchRequest {
                    query,
                    domains,
                    recency_days: *recency_days,
                    max_results: *max_results,
                },
                on_progress,
            )
            .await
        }
        _ => Err(MezError::invalid_args(
            "network executor requires a network-backed action",
        )),
    }
}

async fn execute_fetch_url_action<T: AsyncProviderHttpTransport>(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    transport: &T,
    url: &str,
    max_bytes: Option<u64>,
    on_progress: &mut (impl FnMut(String) + Send),
) -> Result<ActionResult> {
    if let Err(message) = runtime_http_url_support_error(url) {
        return network_action_failure(
            turn,
            action,
            "unsupported_url_scheme",
            message,
            serde_json::json!({
                "url": url,
                "hint": "use shell_command for local paths or file:// URLs"
            }),
        );
    }
    let requested_limit = max_bytes.and_then(|value| usize::try_from(value).ok());
    let limit = requested_limit
        .unwrap_or(DEFAULT_FETCH_URL_MAX_BYTES)
        .min(MAX_FETCH_URL_MAX_BYTES);
    let mut observed = Vec::new();
    let mut published = String::new();
    let mut on_chunk = |chunk: Vec<u8>| {
        observed.extend_from_slice(&chunk);
        let source = safe_fetch_progress_source(&observed);
        if !source.is_empty() && source != published {
            published.clone_from(&source);
            on_progress(source);
        }
        Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
    };
    let response =
        match send_network_get_with_body_chunks(transport, url, limit, &mut on_chunk).await {
            Ok(response) => response,
            Err(error) => {
                return network_action_failure(
                    turn,
                    action,
                    "network_request_failed",
                    error.message().to_string(),
                    serde_json::json!({
                        "url": url,
                        "error_kind": format!("{:?}", error.kind())
                    }),
                );
            }
        };
    let transport_truncated = response.completion == HttpCompletion::Truncated;
    let response = response.response;
    if !(200..=299).contains(&response.status_code) {
        return network_action_failure(
            turn,
            action,
            "network_http_error",
            format!("network request returned HTTP {}", response.status_code),
            serde_json::json!({
                "url": url,
                "status_code": response.status_code,
                "body_bytes": response.body.len()
            }),
        );
    }
    let (body, truncated) = truncate_text_to_bytes(&response.body, limit);
    let truncated = truncated || transport_truncated;
    let structured = network_action_structured_content_json(
        action,
        serde_json::Value::Null,
        serde_json::json!({
            "url": url,
            "status_code": response.status_code,
            "body_bytes": response.body.len(),
            "returned_bytes": body.len(),
            "requested_max_bytes": requested_limit,
            "max_bytes": limit,
            "hard_max_bytes": MAX_FETCH_URL_MAX_BYTES,
            "truncated": truncated
        }),
    )?;
    Ok(ActionResult::succeeded(
        turn,
        action,
        vec![body],
        Some(structured),
    ))
}

async fn execute_web_search_action<T: AsyncProviderHttpTransport>(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    transport: &T,
    request: WebSearchRequest<'_>,
    on_progress: &mut (impl FnMut(String) + Send),
) -> Result<ActionResult> {
    let request = mez_agent::search::SearchRequest {
        query: request.query.to_string(), domains: request.domains.to_vec(),
        recency_days: request.recency_days, max_results: request.max_results,
    };
    let url = request.legacy_ddg_url();
    let full_query = request.legacy_ddg_full_query();
    let response = match super::search::execute_legacy_ddg(transport, &request, on_progress).await {
        Ok(response) => response,
        Err(super::search::SearchExchangeError::Transport(error)) => {
            return network_action_failure(
                turn,
                action,
                "network_request_failed",
                error.message().to_string(),
                serde_json::json!({
                    "url": url,
                    "error_kind": format!("{:?}", error.kind())
                }),
            );
        }
        Err(super::search::SearchExchangeError::Http { status_code, body_bytes }) => {
            return network_action_failure(turn, action, "network_http_error",
                format!("web search returned HTTP {status_code}"),
                serde_json::json!({ "url": url, "status_code": status_code, "body_bytes": body_bytes }));
        }
    };
    let results = response.result.render_text();
    let mut content = if results.trim().is_empty() {
        "No web results were found.".to_string()
    } else {
        results
    };
    if request.recency_days.is_some() {
        content.push_str("\n[mez: recency filtering is best-effort for this backend]");
    }
    let structured = network_action_structured_content_json(
        action,
        serde_json::Value::Null,
        serde_json::json!({
            "query": request.query,
            "full_query": full_query,
            "status_code": response.status_code,
            "body_bytes": response.body_bytes,
            "returned_bytes": response.returned_bytes,
            "html_truncated": response.result.truncated,
            "max_results": response.result.effective_count,
            "recency_days": request.recency_days
        }),
    )?;
    Ok(ActionResult::succeeded(
        turn,
        action,
        vec![content],
        Some(structured),
    ))
}

fn runtime_http_url_support_error(url: &str) -> std::result::Result<(), String> {
    let lowercase = url.trim().to_ascii_lowercase();
    if lowercase.starts_with("http://") || lowercase.starts_with("https://") {
        Ok(())
    } else {
        Err("fetch_url supports only http:// or https:// URLs; use shell_command for local paths or file:// URLs".to_string())
    }
}

/// Sends one bounded network request while forwarding retained body chunks.
async fn send_network_get_with_body_chunks<T: AsyncProviderHttpTransport>(
    transport: &T,
    url: &str,
    max_response_bytes: usize,
    on_chunk: &mut (dyn FnMut(Vec<u8>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send),
) -> ProviderHttpResult<HttpExchangeResponse> {
    let mut headers = BTreeMap::new();
    headers.insert("user-agent".to_string(), "mez".to_string());
    let request = ProviderHttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers,
        body: String::new(),
        timeouts: mez_agent::ProviderHttpTimeouts::from_total(NETWORK_ACTION_TIMEOUT_MS),
        max_response_bytes: Some(max_response_bytes),
    };
    transport
        .send_async_with_body_chunks(&request, on_chunk)
        .await
}

/// Returns valid text from a cumulative byte prefix without exposing HTML.
fn safe_fetch_progress_source(bytes: &[u8]) -> String {
    let valid = match std::str::from_utf8(bytes) {
        Ok(value) => value,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or("")
        }
        Err(_) => return String::new(),
    };
    let trimmed = valid.trim_start();
    if trimmed.starts_with('<') || trimmed.contains("<html") || trimmed.contains("<HTML") {
        return String::new();
    }
    valid.to_string()
}

fn network_action_failure(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    code: &str,
    message: impl Into<String>,
    response: serde_json::Value,
) -> Result<ActionResult> {
    let mut result = ActionResult::failed(turn, action, ActionStatus::Failed, code, message)?;
    result.structured_content_json = Some(network_action_structured_content_json(
        action,
        serde_json::Value::Null,
        response,
    )?);
    Ok(result)
}

/// Applies the legacy UTF-8 body cap and notice accounting shared by network
/// actions. The notice is intentionally outside the retained-byte cap.
pub(super) fn truncate_text_to_bytes(value: &str, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = value[..end].to_string();
    truncated.push_str(&format!("\n[mez: output truncated at {limit} bytes]"));
    (truncated, true)
}
