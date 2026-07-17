//! Openai Provider tests for transport behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[tokio::test]
/// Verifies that the async OpenAI provider path issues the same Responses API
/// request shape while awaiting the async HTTP transport instead of using the
/// blocking transport trait.
async fn openai_provider_async_posts_responses_request_and_parses_output_text() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"model":"gpt-test","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello async"}]}]}"#
                .to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let response = provider.send_request_async(&request).await.unwrap();

    assert_eq!(response.provider, "openai");
    assert_eq!(response.model, "gpt-test");
    assert_eq!(response.raw_text, "hello async");
    let sent = provider.transport.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].url, "https://example.test/responses");
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer test-key")
    );
}

#[test]
/// Verifies openai provider posts responses request, parses output text, and
/// exposes provider token and quota usage metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn openai_provider_posts_responses_request_and_parses_output_text() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: std::collections::BTreeMap::from([
                ("x-ratelimit-limit-requests".to_string(), "100".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "75".to_string(),
                ),
                ("x-ratelimit-reset-requests".to_string(), "10s".to_string()),
                ("x-ratelimit-limit-tokens".to_string(), "200".to_string()),
                (
                    "x-ratelimit-remaining-tokens".to_string(),
                    "100".to_string(),
                ),
            ]),
            body: serde_json::json!({
                "model": "gpt-test",
                "usage": {
                    "input_tokens": 42,
                    "output_tokens": 11,
                    "input_tokens_details": {
                        "cached_tokens": 30
                    },
                    "output_tokens_details": {
                        "reasoning_tokens": 7
                    }
                },
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "hello back"
                    }]
                }]
            })
            .to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.provider, "openai");
    assert_eq!(response.model, "gpt-test");
    assert_eq!(response.raw_text, "hello back");
    assert_eq!(response.usage.input_tokens, 42);
    assert_eq!(response.usage.output_tokens, 11);
    assert_eq!(response.usage.reasoning_tokens, 7);
    assert_eq!(response.usage.cached_input_tokens, Some(30));
    assert_eq!(response.quota_usage.len(), 2);
    let requests_quota = response
        .quota_usage
        .iter()
        .find(|quota| quota.name == "requests")
        .unwrap();
    assert_eq!(requests_quota.used_percent_display(), "25.00%");
    assert_eq!(requests_quota.reset.as_deref(), Some("10s"));
    let tokens_quota = response
        .quota_usage
        .iter()
        .find(|quota| quota.name == "tokens")
        .unwrap();
    assert_eq!(tokens_quota.used_percent_display(), "50.00%");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].url, "https://example.test/responses");
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer test-key")
    );
}
