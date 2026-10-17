//! Agent tests for network actions behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;
use crate::integrations::agent::network::execute_network_action_with_transport_async_with_progress;
use crate::integrations::http::{HttpCompletion, HttpExchangeResponse};

mod search_outcomes;
mod search_transport_contracts;

#[tokio::test]
/// Characterizes legacy zero/default/hard counts, exact domain conjunction and
/// absence of a transmitted recency parameter. These are extraction fixtures,
/// not new semantics: zero clips recognized results without claiming zero hits.
async fn network_search_legacy_wire_and_count_characterization() {
    for (requested, count) in [(None, 10), (Some(0), 0), (Some(99), 50)] {
        let action = AgentAction {
            id: "search-count".to_string(),
            payload: AgentActionPayload::WebSearch {
                query: "a & b".to_string(),
                domains: vec!["one.test".to_string(), "two.test/path".to_string()],
                recency_days: Some(7),
                max_results: requested,
            },
        };
        let transport = AsyncFakeProviderHttpTransport {
            requests: std::sync::Mutex::new(Vec::new()),
            response: ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: (0..60)
                    .map(|i| {
                        format!(
                            "<a class=\"result__a\" href=\"https://example.test/{i}\">Title {i}</a>"
                        )
                    })
                    .collect(),
            },
        };
        let result = execute_network_action_with_transport_async(&turn(), &action, &transport)
            .await
            .unwrap();
        assert_eq!(result.status, ActionStatus::Succeeded);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].url,
            "https://duckduckgo.com/html/?q=a%20%26%20b%20site%3Aone.test%20site%3Atwo.test%2Fpath"
        );
        assert_eq!(requests[0].headers.len(), 1);
        assert_eq!(
            requests[0].headers.get("user-agent").map(String::as_str),
            Some("mez")
        );
        assert_eq!(requests[0].max_response_bytes, Some(1024 * 1024));
        assert_eq!(
            requests[0].timeouts,
            mez_agent::ProviderHttpTimeouts::from_total(30_000)
        );
        let content = result.content_text();
        assert_eq!(
            content
                .lines()
                .filter(|line| line.starts_with("   https://"))
                .count(),
            count
        );
        assert!(content.ends_with("[mez: recency filtering is best-effort for this backend]"));
        if count == 0 {
            assert!(content.starts_with("No web result entries were requested (max_results = 0)."));
        }
        let structured: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(structured["response"]["max_results"], count);
        assert!(
            structured["policy_command"]
                .as_str()
                .unwrap()
                .contains(&requests[0].url)
        );
    }
}

#[tokio::test]
/// Pins complete-anchor title cleanup and legacy permissive nonblank links.
/// Pending, empty and incomplete responses have separate failed-outcome tests.
async fn network_search_legacy_anchor_and_empty_characterization() {
    for (status, body, expected) in [
        (
            200,
            "<a class=\"result__a\" href=\"https://example.test/a\"><b>A</b> &amp; B</a>",
            "1. A & B\n   https://example.test/a",
        ),
        (
            200,
            "<a class=\"result__a\" href=\"javascript:legacy\">Legacy</a>",
            "1. Legacy\n   javascript:legacy",
        ),
        (
            200,
            "<a class=\"result__a\" href=\"/relative\">Relative</a>",
            "1. Relative\n   /relative",
        ),
        (
            200,
            "<a class=\"result__a\" href=\"https://user:pass@example.test/\">Credentialed</a>",
            "1. Credentialed\n   https://user:pass@example.test/",
        ),
    ] {
        let action = AgentAction {
            id: "search-anchors".to_string(),
            payload: AgentActionPayload::WebSearch {
                query: "fixture".to_string(),
                domains: Vec::new(),
                recency_days: None,
                max_results: None,
            },
        };
        let transport = AsyncFakeProviderHttpTransport {
            requests: std::sync::Mutex::new(Vec::new()),
            response: ProviderHttpResponse {
                status_code: status,
                headers: Default::default(),
                body: body.to_string(),
            },
        };
        let result = execute_network_action_with_transport_async(&turn(), &action, &transport)
            .await
            .unwrap();
        assert_eq!(result.status, ActionStatus::Succeeded);
        assert_eq!(result.content_text(), expected);
    }
}

#[tokio::test]
/// A complete bounded body can still produce a title larger than model context.
/// Final context clipping remains independently UTF-8 safe; response truncation
/// failures are tested separately and never promote a partial result corpus.
async fn network_search_legacy_body_and_final_context_characterization() {
    let action = AgentAction {
        id: "search-bounds".to_string(),
        payload: AgentActionPayload::WebSearch {
            query: "fixture".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: None,
        },
    };
    let prefix = "<a class=\"result__a\" href=\"https://example.test/a\">";
    let body = format!(
        "{prefix}{}</a>{}",
        "é".repeat(140_000),
        "x".repeat(512 * 1024)
    );
    let body_bytes = body.len();
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body,
        },
    };
    let result = execute_network_action_with_transport_async(&turn(), &action, &transport)
        .await
        .unwrap();
    let structured: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(structured["response"]["body_bytes"], body_bytes);
    assert_eq!(structured["response"]["html_truncated"], false);
    assert_eq!(structured["response"]["returned_bytes"], body_bytes);
    let context = action_result_context_content(&result);
    assert!(context.contains("[mez: action result content truncated after 262144 bytes]"));
    assert!(!context.contains('�'));
}

/// Streams a configured sequence of retained body chunks for network progress tests.
struct ChunkedAsyncProviderHttpTransport {
    chunks: Vec<Vec<u8>>,
    response: ProviderHttpResponse,
    completion: HttpCompletion,
    failure: Option<mez_agent::ProviderHttpError>,
}

impl AsyncProviderHttpTransport for ChunkedAsyncProviderHttpTransport {
    fn send_async<'a>(
        &'a self,
        _request: &'a ProviderHttpRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            match &self.failure {
                Some(error) => Err(error.clone()),
                None => Ok(self.response.clone()),
            }
        })
    }

    fn send_async_with_body_chunks<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
        on_chunk: &'a mut (
                    dyn FnMut(
            Vec<u8>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                        + Send
                ),
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = mez_agent::ProviderHttpResult<HttpExchangeResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            for chunk in &self.chunks {
                on_chunk(chunk.clone()).await;
            }
            self.send_async(request)
                .await
                .map(|response| HttpExchangeResponse {
                    response,
                    completion: self.completion,
                })
        })
    }
}

#[tokio::test]
/// Verifies semantic URL fetch actions execute through the runtime HTTP
/// transport instead of the pane shell while still returning compact
/// model-facing action-result context. This protects external-content actions
/// from polluting shell history or waiting on pane shell readiness.
async fn network_fetch_url_action_executor_returns_output_context_for_provider() {
    let turn = turn();
    let action = AgentAction {
        id: "fetch-1".to_string(),

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
/// Verifies fetch progress withholds an incomplete UTF-8 scalar and never
/// emits replacement text while retaining the final canonical action result.
async fn network_fetch_progress_handles_split_utf8_without_raw_html() {
    let turn = turn();
    let action = AgentAction {
        id: "fetch-progress".to_string(),

        payload: AgentActionPayload::FetchUrl {
            url: "https://example.test/data.txt".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let transport = ChunkedAsyncProviderHttpTransport {
        chunks: vec![b"h\xC3".to_vec(), b"\xA9llo".to_vec()],
        completion: HttpCompletion::Complete,
        failure: None,
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: "héllo".to_string(),
        },
    };
    let mut progress = Vec::new();

    let result = execute_network_action_with_transport_async_with_progress(
        &turn,
        &action,
        &transport,
        &mut |source| progress.push(source),
    )
    .await
    .unwrap();

    assert_eq!(progress, vec!["h", "héllo"]);
    assert!(progress.iter().all(|source| !source.contains('�')));
    assert_eq!(result.content_text(), "héllo");
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

#[tokio::test]
/// Verifies search progress publishes only a complete cleaned result anchor,
/// never the partial HTML source observed before its closing tag arrives.
async fn network_search_progress_emits_complete_entries_only() {
    let turn = turn();
    let action = AgentAction {
        id: "search-progress".to_string(),

        payload: AgentActionPayload::WebSearch {
            query: "mez terminal".to_string(),
            domains: Vec::new(),
            recency_days: None,
            max_results: Some(1),
        },
    };
    let html = r#"<a class="result__a" href="/l/?uddg=https%3A%2F%2Fexample.com%2Fmez">Mez &amp; Terminal</a>"#;
    let split = html.find("</a>").unwrap();
    let transport = ChunkedAsyncProviderHttpTransport {
        chunks: vec![
            html.as_bytes()[..split].to_vec(),
            html.as_bytes()[split..].to_vec(),
        ],
        completion: HttpCompletion::Complete,
        failure: None,
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: html.to_string(),
        },
    };
    let mut progress = Vec::new();

    let result = execute_network_action_with_transport_async_with_progress(
        &turn,
        &action,
        &transport,
        &mut |source| progress.push(source),
    )
    .await
    .unwrap();

    assert_eq!(
        progress,
        vec!["1. Mez & Terminal\n   https://example.com/mez"]
    );
    assert!(progress.iter().all(|source| !source.contains("<a")));
    assert!(result.content_text().contains("1. Mez & Terminal"));
}
