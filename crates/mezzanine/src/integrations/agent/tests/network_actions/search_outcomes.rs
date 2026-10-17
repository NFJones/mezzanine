//! Offline DDG outcome regressions at the semantic action boundary.
//!
//! Fixtures are deliberately synthetic and sanitized, not live provider probes.
//! HTTP 202 establishes only a noncompleted response, never its remote cause.
//! Missing result anchors cannot establish a completed empty search. These
//! tests assert action status and content-free diagnostics, not HTML authority.

use super::*;

mod loopback;

/// Builds an ordinary query-only request without provider/configuration changes.
fn search() -> AgentAction {
    AgentAction {
        id: "ddg-outcome".into(),
        payload: AgentActionPayload::WebSearch {
            query: "offline fixture".into(),
            domains: vec![],
            recency_days: None,
            max_results: None,
        },
    }
}

/// Pending, challenge-like, unrelated and incomplete pages must not masquerade
/// as successful zero-hit searches. No body is published as diagnostics, no
/// unverified CAPTCHA/blocking cause is asserted, and no retry is dispatched.
#[tokio::test]
async fn network_search_nonresult_pages_fail_without_fabricating_empty_results() {
    for (status, body) in [
        (202, "<html><body>Pending fixture HTML_CANARY</body></html>"),
        (
            200,
            "<html><form action=\"/challenge\">Verify fixture HTML_CANARY</form></html>",
        ),
        (
            200,
            "<html><body>Unrelated fixture HTML_CANARY</body></html>",
        ),
        (
            200,
            "<a class=\"result__a\" href=\"https://example.test/\">unfinished HTML_CANARY",
        ),
        (200, ""),
    ] {
        let action = search();
        let transport = AsyncFakeProviderHttpTransport {
            requests: std::sync::Mutex::new(vec![]),
            response: ProviderHttpResponse {
                status_code: status,
                headers: Default::default(),
                body: body.into(),
            },
        };
        let result = execute_network_action_with_transport_async(&turn(), &action, &transport)
            .await
            .unwrap();
        assert_eq!(
            result.status,
            ActionStatus::Failed,
            "HTTP {status}: {result:?}"
        );
        assert_eq!(
            result.error.as_ref().unwrap().code,
            "network_search_unusable_response"
        );
        assert!(
            result
                .error
                .as_ref()
                .unwrap()
                .message
                .contains("usable completed results")
        );
        assert!(!result.content_text().contains("No web results were found."));
        assert!(mez_agent::outcome::runtime_action_result_is_feedback_candidate(&result));
        let structured = result.structured_content_json.as_deref().unwrap();
        let data: serde_json::Value = serde_json::from_str(structured).unwrap();
        assert_eq!(data["response"]["status_code"], status);
        assert_eq!(data["response"]["body_bytes"], body.len());
        assert_eq!(data["response"]["html_truncated"], false);
        assert!(!structured.contains("HTML_CANARY"));
        assert!(!structured.contains("<html>"));
        let context = action_result_context_content(&result);
        assert!(!context.contains("HTML_CANARY"));
        assert!(!context.to_ascii_lowercase().contains("captcha"));
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }
}

/// A class substring, comment/script text, or another tag's attributes cannot
/// manufacture a completed hit. The no-results-looking fixture has no locally
/// established provider-format oracle, so honest unrecognized failure is safer
/// than asserting a genuine empty search from invented markup.
#[tokio::test]
async fn network_search_nonanchors_and_unqualified_empty_shapes_are_not_results() {
    for body in [
        "<!-- <a class=\"result__a\" href=\"https://example.test/\">Fake</a> -->",
        "<script>const fixture = '<a class=\"result__a\" href=\"https://example.test/\">Fake</a>';</script>",
        "<a class=\"not-result__a\" href=\"https://example.test/\">Fake</a>",
        "<div data-note=\"result__a\"><a href=\"https://example.test/\">Fake</a></div>",
        "<a class=\"result__a\">missing href<a href=\"https://example.test/\">borrowed</a>",
        "<a class=\"result__a\" href=\"https://example.test/\">nested<a href=\"https://example.test/b\">borrowed</a>",
        "<html><body><div class=\"no-results\">No results found for offline fixture</div></body></html>",
    ] {
        let transport = AsyncFakeProviderHttpTransport {
            requests: Default::default(),
            response: ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: body.into(),
            },
        };
        let result = execute_network_action_with_transport_async(&turn(), &search(), &transport)
            .await
            .unwrap();
        assert_eq!(result.status, ActionStatus::Failed, "{result:?}");
        assert_eq!(
            result.error.as_ref().unwrap().code,
            "network_search_unusable_response"
        );
        assert!(!result.content_text().contains("Fake"));
        assert!(!result.content_text().contains("No web results were found."));
    }
}

/// Response completion is checked before result-count clipping. Neither zero
/// requested hits nor a satisfied count permits pending/partial responses or
/// malformed later result anchors to settle success; earlier progress remains
/// provisional until the runtime consumes this failed terminal result.
#[tokio::test]
async fn network_search_count_limits_do_not_hide_pending_or_incomplete_results() {
    let complete = "<a class=\"result__a\" href=\"https://example.test/a\">First</a>";
    for count in [0, 1] {
        for (status, completion, body) in [
            (202, HttpCompletion::Complete, complete.to_string()),
            (204, HttpCompletion::Complete, complete.to_string()),
            (206, HttpCompletion::Complete, complete.to_string()),
            (200, HttpCompletion::Truncated, complete.to_string()),
            (200, HttpCompletion::HeadersOnly, complete.to_string()),
            (200, HttpCompletion::StreamDecoded, complete.to_string()),
            (
                200,
                HttpCompletion::Complete,
                format!(
                    "{complete}<a class=\"result__a\" href=\"https://example.test/b\">unfinished"
                ),
            ),
        ] {
            let mut action = search();
            let AgentActionPayload::WebSearch { max_results, .. } = &mut action.payload else {
                unreachable!()
            };
            *max_results = Some(count);
            let transport = ChunkedAsyncProviderHttpTransport {
                chunks: vec![body.as_bytes().to_vec()],
                completion,
                failure: None,
                response: ProviderHttpResponse {
                    status_code: status,
                    headers: Default::default(),
                    body,
                },
            };
            let mut progress = vec![];
            let result = execute_network_action_with_transport_async_with_progress(
                &turn(),
                &action,
                &transport,
                &mut |source| progress.push(source),
            )
            .await
            .unwrap();
            assert_eq!(
                result.status,
                ActionStatus::Failed,
                "count={count} status={status} {completion:?}: {result:?}"
            );
            assert_eq!(
                result.error.as_ref().unwrap().code,
                "network_search_unusable_response"
            );
            assert!(progress.iter().all(|source| !source.contains('<')));
            if count == 1 {
                assert_eq!(progress, vec!["1. First\n   https://example.test/a"]);
            } else {
                assert!(progress.is_empty());
            }
            let data: serde_json::Value =
                serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
            assert_eq!(data["response"]["status_code"], status);
            assert_eq!(
                data["response"]["html_truncated"],
                completion == HttpCompletion::Truncated
            );
        }
    }
}

/// Actual closed anchors with exact class tokens retain legacy title/link
/// formatting. Attribute order and quote style are HTML syntax, not evidence
/// for a different provider, and nested title formatting remains cleaned.
#[tokio::test]
async fn network_search_recognizes_complete_result_anchor_controls() {
    for body in [
        "<a class=\"result__a\" href=\"https://example.test/a\"><b>A</b> &amp; B</a>",
        "<html><body><a href='https://example.test/a' class='result__a extra'><b>A</b> &amp; B</a></body></html>",
    ] {
        let transport = AsyncFakeProviderHttpTransport {
            requests: Default::default(),
            response: ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: body.into(),
            },
        };
        let result = execute_network_action_with_transport_async(&turn(), &search(), &transport)
            .await
            .unwrap();
        assert_eq!(result.status, ActionStatus::Succeeded);
        assert_eq!(result.content_text(), "1. A & B\n   https://example.test/a");
    }
}
