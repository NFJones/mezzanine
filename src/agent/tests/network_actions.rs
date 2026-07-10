//! Agent tests for network actions behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[tokio::test]
/// Verifies semantic URL fetch actions execute through the runtime HTTP
/// transport instead of the pane shell while still returning compact
/// model-facing action-result context. This protects external-content actions
/// from polluting shell history or waiting on pane shell readiness.
async fn network_fetch_url_action_executor_returns_output_context_for_provider() {
    let turn = turn();
    let action = AgentAction {
        id: "fetch-1".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::FetchUrl {
            url: "https://example.test/data.txt".to_string(),
            format: None,
            max_bytes: Some(4096),
        },
    };
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: "alpha\nbravo\n".to_string(),
        },
    };

    let result = execute_network_action_with_transport_async(&turn, &action, &transport)
        .await
        .unwrap();

    assert_eq!(result.action_type, "fetch_url");
    assert_eq!(result.status, ActionStatus::Succeeded);
    assert!(local_action_plan(&action).unwrap().is_none());
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].url, "https://example.test/data.txt");
    assert_eq!(requests[0].max_response_bytes, Some(4096));
    assert_eq!(
        requests[0].headers.get("user-agent").map(String::as_str),
        Some("mez")
    );
    let context = action_result_context_content(&result);
    assert!(context.contains("[action_result fetch-1 fetch_url succeeded]"));
    assert!(context.contains("content:\nalpha\nbravo\n"), "{context}");
}

#[tokio::test]
/// Verifies `fetch_url` applies a small default response-body cap before
/// exposing network content to the model. This keeps large HTML pages from
/// dominating the next request context when the model did not ask for a larger
/// bounded body.
async fn network_fetch_url_executor_default_bounds_response_body() {
    let turn = turn();
    let action = AgentAction {
        id: "fetch-large-default".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::FetchUrl {
            url: "https://example.test/large.html".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: format!("{}tail-marker", "a".repeat(20 * 1024)),
        },
    };

    let result = execute_network_action_with_transport_async(&turn, &action, &transport)
        .await
        .unwrap();

    let content = result.content_text();
    assert!(content.contains("[mez: output truncated at 16384 bytes]"));
    assert!(!content.contains("tail-marker"), "{content}");
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests[0].max_response_bytes, Some(16 * 1024));
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(structured.contains(r#""max_bytes":16384"#), "{structured}");
    assert!(
        structured.contains(r#""hard_max_bytes":262144"#),
        "{structured}"
    );
}

#[tokio::test]
/// Verifies the runtime network executor rejects non-HTTP(S) fetch URLs before
/// touching the transport. This is a defense-in-depth guard for action batches
/// constructed before validation or from older runtime state.
async fn network_fetch_url_executor_rejects_file_scheme_without_transport() {
    let turn = turn();
    let action = AgentAction {
        id: "fetch-file".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::FetchUrl {
            url: "file:///home/neil/Downloads/test.txt".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: "should not be read".to_string(),
        },
    };

    let result = execute_network_action_with_transport_async(&turn, &action, &transport)
        .await
        .unwrap();

    assert_eq!(result.status, ActionStatus::Failed);
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("unsupported_url_scheme")
    );
    assert!(
        result
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("use shell_command"),
        "{result:?}"
    );
    assert!(transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
/// Verifies semantic web search actions execute through the runtime HTTP
/// transport and return parsed search results rather than a shell-backed
/// scraping command. This keeps search requests independent of pane shell state
/// while preserving model-facing continuation data.
async fn network_web_search_action_executor_formats_search_results() {
    let turn = turn();
    let action = AgentAction {
        id: "search-1".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::WebSearch {
            query: "mez terminal".to_string(),
            domains: vec!["example.com".to_string()],
            recency_days: Some(7),
            max_results: Some(1),
        },
    };
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"<html><a rel="nofollow" class="result__a" href="/l/?uddg=https%3A%2F%2Fexample.com%2Fmez">Mez &amp; Terminal</a></html>"#.to_string(),
        },
    };

    let result = execute_network_action_with_transport_async(&turn, &action, &transport)
        .await
        .unwrap();

    assert_eq!(result.action_type, "web_search");
    assert_eq!(result.status, ActionStatus::Succeeded);
    assert!(local_action_plan(&action).unwrap().is_none());
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .url
            .starts_with("https://duckduckgo.com/html/?q=")
    );
    assert!(requests[0].url.contains("mez%20terminal"));
    assert!(requests[0].url.contains("site%3Aexample.com"));
    assert_eq!(requests[0].max_response_bytes, Some(1024 * 1024));
    let context = action_result_context_content(&result);
    assert!(context.contains("[action_result search-1 web_search succeeded]"));
    assert!(context.contains("1. Mez & Terminal"), "{context}");
    assert!(context.contains("https://example.com/mez"), "{context}");
    assert!(
        context.contains("recency filtering is best-effort"),
        "{context}"
    );
}
