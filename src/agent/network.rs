//! Runtime-owned network MAAP action execution.
//!
//! `web_search` and `fetch_url` are semantic actions, but they do not need pane
//! shell state. This module keeps their permission-facing plans and HTTP
//! execution separate from shell-backed local actions so the runtime can service
//! external content requests without emitting generated shell commands.

use std::collections::BTreeMap;

use super::shell::shell_quote;
use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnRecord,
    AsyncProviderHttpTransport, MezError, ProviderHttpRequest, ProviderHttpResponse, Result,
};

/// Default response bytes exposed to a `fetch_url` action when the model does
/// not request a smaller bounded body.
const DEFAULT_FETCH_URL_MAX_BYTES: usize = 16 * 1024;
/// Hard response-body cap for one `fetch_url` action, even when the model asks
/// for a larger value.
const MAX_FETCH_URL_MAX_BYTES: usize = 256 * 1024;
/// Maximum response bytes used when parsing web search result HTML.
const DEFAULT_WEB_SEARCH_MAX_BYTES: usize = 1024 * 1024;
/// Timeout applied to runtime-owned network actions.
const NETWORK_ACTION_TIMEOUT_MS: u64 = 30_000;

/// Runtime-generated execution data for one network-backed semantic action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkActionPlan {
    /// Concise user-facing summary shown before the network request starts.
    pub summary: String,
    /// Classifier-friendly pseudo command representing the same network effect.
    pub policy_command: String,
}

/// Returns the runtime-owned network plan for a network-backed MAAP action.
pub fn network_action_plan(action: &AgentAction) -> Result<Option<NetworkActionPlan>> {
    match &action.payload {
        AgentActionPayload::WebSearch { query, domains, .. } => {
            let mut full_query = query.to_string();
            for domain in domains {
                full_query.push_str(" site:");
                full_query.push_str(domain);
            }
            let url = format!(
                "https://duckduckgo.com/html/?q={}",
                urlencoding::encode(&full_query)
            );
            Ok(Some(NetworkActionPlan {
                summary: format!("I’ll search the web for `{query}`."),
                policy_command: format!("curl {}", shell_quote(&url)),
            }))
        }
        AgentActionPayload::FetchUrl { url, format, .. } => {
            let mut summary = format!("I’ll fetch `{url}`.");
            if let Some(format) = format {
                summary.push_str(&format!(" Format hint: {format}."));
            }
            Ok(Some(NetworkActionPlan {
                summary,
                policy_command: format!("curl {}", shell_quote(url)),
            }))
        }
        _ => Ok(None),
    }
}

/// Returns the user-facing summary for a network-backed action.
pub fn network_action_summary(action: &AgentAction) -> Result<Option<String>> {
    Ok(network_action_plan(action)?.map(|plan| plan.summary))
}

/// Builds compact structured content for a network-backed action result.
pub fn network_action_structured_content_json(
    action: &AgentAction,
    approval: serde_json::Value,
    response: serde_json::Value,
) -> Result<String> {
    let Some(plan) = network_action_plan(action)? else {
        return Err(MezError::invalid_args(
            "network structured content requires a network-backed action",
        ));
    };
    let value = serde_json::json!({
        "kind": action.action_type(),
        "summary": plan.summary,
        "policy_command": plan.policy_command,
        "approval": approval,
        "response": response
    });
    serde_json::to_string(&value).map_err(|error| {
        MezError::invalid_state(format!(
            "network structured content encoding failed: {error}"
        ))
    })
}

/// Executes a network-backed semantic action through a runtime HTTP transport.
pub async fn execute_network_action_with_transport_async<T: AsyncProviderHttpTransport>(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    transport: &T,
) -> Result<ActionResult> {
    match &action.payload {
        AgentActionPayload::FetchUrl { url, max_bytes, .. } => {
            execute_fetch_url_action(turn, action, transport, url, *max_bytes).await
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
                query,
                domains,
                *recency_days,
                *max_results,
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
    let response = match send_network_get(transport, url, limit).await {
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
    let transport_truncated = network_response_was_truncated(&response);
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
    query: &str,
    domains: &[String],
    recency_days: Option<u64>,
    max_results: Option<u64>,
) -> Result<ActionResult> {
    let mut full_query = query.to_string();
    for domain in domains {
        full_query.push_str(" site:");
        full_query.push_str(domain);
    }
    let url = format!(
        "https://duckduckgo.com/html/?q={}",
        urlencoding::encode(&full_query)
    );
    let response = match send_network_get(transport, &url, DEFAULT_WEB_SEARCH_MAX_BYTES).await {
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
    if !(200..=299).contains(&response.status_code) {
        return network_action_failure(
            turn,
            action,
            "network_http_error",
            format!("web search returned HTTP {}", response.status_code),
            serde_json::json!({
                "url": url,
                "status_code": response.status_code,
                "body_bytes": response.body.len()
            }),
        );
    }
    let transport_truncated = network_response_was_truncated(&response);
    let (body, truncated) = truncate_text_to_bytes(&response.body, DEFAULT_WEB_SEARCH_MAX_BYTES);
    let truncated = truncated || transport_truncated;
    let limit = max_results
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(10)
        .min(50);
    let results = parse_duckduckgo_html_results(&body, limit);
    let mut content = if results.trim().is_empty() {
        "No web results were found.".to_string()
    } else {
        results
    };
    if recency_days.is_some() {
        content.push_str("\n[mez: recency filtering is best-effort for this backend]");
    }
    let structured = network_action_structured_content_json(
        action,
        serde_json::Value::Null,
        serde_json::json!({
            "query": query,
            "full_query": full_query,
            "status_code": response.status_code,
            "body_bytes": response.body.len(),
            "returned_bytes": body.len(),
            "html_truncated": truncated,
            "max_results": limit,
            "recency_days": recency_days
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

async fn send_network_get<T: AsyncProviderHttpTransport>(
    transport: &T,
    url: &str,
    max_response_bytes: usize,
) -> Result<ProviderHttpResponse> {
    let mut headers = BTreeMap::new();
    headers.insert("user-agent".to_string(), "mez".to_string());
    let request = ProviderHttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers,
        body: String::new(),
        timeout_ms: NETWORK_ACTION_TIMEOUT_MS,
        max_response_bytes: Some(max_response_bytes),
    };
    transport.send_async(&request).await.map_err(|error| {
        MezError::invalid_state(format!(
            "network action request failed: {}",
            error.message()
        ))
    })
}

fn network_response_was_truncated(response: &ProviderHttpResponse) -> bool {
    response
        .headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("x-mez-body-truncated") && value == "true")
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

fn truncate_text_to_bytes(value: &str, limit: usize) -> (String, bool) {
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

fn parse_duckduckgo_html_results(body: &str, max_results: usize) -> String {
    let mut output = String::new();
    let mut remainder = body;
    let mut index = 1usize;
    while index <= max_results {
        let Some(class_pos) = remainder.find("result__a") else {
            break;
        };
        remainder = &remainder[class_pos..];
        let Some(href_attr_pos) = remainder.find("href=\"") else {
            break;
        };
        let href_start = href_attr_pos + "href=\"".len();
        let Some(href_end) = remainder[href_start..].find('"') else {
            break;
        };
        let href = &remainder[href_start..href_start + href_end];
        let Some(title_start) = remainder[href_start + href_end..].find('>') else {
            break;
        };
        let title_start = href_start + href_end + title_start + 1;
        let Some(title_end) = remainder[title_start..].find("</a>") else {
            break;
        };
        let title = clean_html_text(&remainder[title_start..title_start + title_end]);
        let href = clean_duckduckgo_href(href);
        if !title.trim().is_empty() && !href.trim().is_empty() {
            output.push_str(&format!("{index}. {title}\n   {href}\n"));
            index += 1;
        }
        remainder = &remainder[title_start + title_end..];
    }
    output.trim_end().to_string()
}

fn clean_html_text(value: &str) -> String {
    let mut cleaned = String::new();
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => cleaned.push(ch),
            _ => {}
        }
    }
    decode_html_entities(&cleaned)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_duckduckgo_href(value: &str) -> String {
    let decoded = decode_html_entities(value);
    if let Some(encoded) = query_param_value(&decoded, "uddg") {
        return urlencoding::decode(encoded)
            .map(|value| value.into_owned())
            .unwrap_or_else(|_| encoded.to_string());
    }
    decoded
}

fn query_param_value<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=");
    let start = url.find(&marker)? + marker.len();
    let rest = &url[start..];
    let end = rest.find('&').unwrap_or(rest.len());
    Some(&rest[..end])
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}
