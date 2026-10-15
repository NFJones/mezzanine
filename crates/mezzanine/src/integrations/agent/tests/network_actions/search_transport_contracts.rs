//! Offline bridge regressions for legacy search exchange completion, status,
//! interruption and cumulative progress. No runtime approval rules are mocked
//! or changed by these transport fixtures; existing runtime gates own authority.

use super::*;
use crate::integrations::agent::network::execute_network_action_with_transport_async_with_progress;
use crate::integrations::http::HttpCompletion;

/// Builds a query-only search, preserving the compact public action contract.
fn search() -> AgentAction {
    AgentAction { id: "search-transport".into(), payload: AgentActionPayload::WebSearch { query: "fixture".into(), domains: vec![], recency_days: None, max_results: None } }
}

#[tokio::test]
/// Pins complete-prefix progress across split scalars and invalid UTF-8: only
/// clean closed anchors may publish, while invalid bytes suppress publication.
async fn network_search_utf8_progress_preserves_legacy_prefix_rules() {
    let prefix = b"<a class=\"result__a\" href=\"https://example.test/a\">h";
    for (chunks, expected) in [
        (vec![[prefix.as_slice(), b"\xc3"].concat(), b"\xa9</a>".to_vec()], vec!["1. hé\n   https://example.test/a"]),
        (vec![[prefix.as_slice(), b"\xff</a>"].concat()], vec![]),
    ] {
        let transport = ChunkedAsyncProviderHttpTransport { chunks, completion: HttpCompletion::Complete, failure: None, response: ProviderHttpResponse { status_code: 200, headers: Default::default(), body: "<a class=\"result__a\" href=\"https://example.test/a\">hé</a>".into() } };
        let mut progress = vec![];
        execute_network_action_with_transport_async_with_progress(&turn(), &search(), &transport, &mut |value| progress.push(value)).await.unwrap();
        assert_eq!(progress, expected);
    }
}

#[tokio::test]
/// An HTTP transport's explicit truncation survives even when the retained body
/// is short. Non-2xx status mapping and transport interruption remain legacy
/// failures without publishing raw HTML or converting failures into no-results.
async fn network_search_completion_and_failure_envelopes_stay_legacy() {
    for (status, failure, expected) in [
        (200, None, None),
        (402, None, Some("network_http_error")),
        (429, None, Some("network_http_error")),
        (200, Some(mez_agent::ProviderHttpError::io("fixture interruption")), Some("network_request_failed")),
        (200, Some(mez_agent::ProviderHttpError::timeout(mez_agent::ProviderHttpTimeoutPhase::Total, 30_000, "fixture")), Some("network_request_failed")),
    ] {
        let transport = ChunkedAsyncProviderHttpTransport { chunks: vec![], completion: HttpCompletion::Truncated, failure, response: ProviderHttpResponse { status_code: status, headers: Default::default(), body: "<html>private fixture body</html>".into() } };
        let result = execute_network_action_with_transport_async(&turn(), &search(), &transport).await.unwrap();
        assert_eq!(result.error.as_ref().map(|e| e.code.as_str()), expected);
        let data: serde_json::Value = serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        if expected.is_none() { assert_eq!(data["response"]["html_truncated"], true); }
        assert!(!result.content_text().contains("<html>"));
        assert!(!result.structured_content_json.unwrap().contains("private fixture body"));
    }
}

#[tokio::test]
/// Local cap in the middle of a scalar retains a valid prefix and the exact
/// historical notice beyond the 1MiB limit, not replacement characters.
async fn network_search_body_cap_split_scalar_remains_utf8_safe() {
    let body = format!("{}é", "x".repeat(1024 * 1024 - 1));
    let transport = AsyncFakeProviderHttpTransport { requests: std::sync::Mutex::new(vec![]), response: ProviderHttpResponse { status_code: 200, headers: Default::default(), body } };
    let result = execute_network_action_with_transport_async(&turn(), &search(), &transport).await.unwrap();
    let data: serde_json::Value = serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(data["response"]["returned_bytes"], 1024 * 1024 - 1 + "\n[mez: output truncated at 1048576 bytes]".len());
    assert_eq!(data["response"]["html_truncated"], true);
    assert_eq!(result.content_text(), "No web results were found.");
}
