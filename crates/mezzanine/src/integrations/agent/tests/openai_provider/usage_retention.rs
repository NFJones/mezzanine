//! Incurred Responses expense survives fallible MAAP validation after decoding.

use super::*;

fn request() -> ModelRequest {
    assemble_model_request(
        &ModelProfile {
            provider: "openai".into(),
            model: "gpt-test".into(),
            model_capabilities: Default::default(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: Default::default(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "fixture".into(),
            content: "validate output".into(),
        }])
        .unwrap(),
    )
    .unwrap()
}

fn response(stream: bool) -> ProviderHttpResponse {
    let decoded = serde_json::json!({
        "model":"gpt-test", "status":"completed",
        "output_text": serde_json::json!({
            "rationale":"fixture", "actions":[{"command":"ls"}]
        }).to_string(),
        "usage":{"input_tokens":11,"output_tokens":7}
    });
    ProviderHttpResponse {
        status_code: 200,
        headers: std::collections::BTreeMap::from([(
            "content-type".into(),
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            }
            .into(),
        )]),
        body: if stream {
            format!(
                "event: response.output_text.delta\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type":"response.output_text.delta", "delta":decoded["output_text"]
                }),
                serde_json::json!({
                    "type":"response.completed", "response":decoded
                })
            )
        } else {
            decoded.to_string()
        },
    }
}

fn assert_expense(error: &crate::error::MezError) {
    assert!(error.message().contains("bare command"), "{error:?}");
    let usage = error.provider_incurred_usage();
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 7));
}

/// A rejected action batch is not a free inference. Test the real adapter
/// suffix rather than manufacturing an error with preattached usage.
#[test]
fn responses_maap_rejection_retains_usage_sync() {
    for stream in [false, true] {
        let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
            "fixture-key",
            "https://example.test/responses",
            1_000,
            Default::default(),
            stream,
            FakeProviderHttpTransport {
                requests: RefCell::new(Vec::new()),
                response: response(stream),
            },
        )
        .unwrap();
        assert_expense(&provider.send_request(&request()).unwrap_err());
        assert_eq!(provider.transport.requests.borrow().len(), 1);
    }
}

#[tokio::test]
async fn responses_maap_rejection_retains_usage_async() {
    for stream in [false, true] {
        let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
            "fixture-key",
            "https://example.test/responses",
            1_000,
            Default::default(),
            stream,
            AsyncFakeProviderHttpTransport {
                requests: std::sync::Mutex::new(Vec::new()),
                response: response(stream),
            },
        )
        .unwrap();
        use crate::integrations::agent::provider::{
            ObservedAsyncModelProvider, ProviderRequestPurpose, ProviderWireRequestObserver,
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let observer = ProviderWireRequestObserver::new("fixture-conversation", "%1", sender);
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        assert_expense(&observed.send_request_async(&request()).await.unwrap_err());
        let observation = receiver.try_recv().unwrap();
        assert!(!observation.succeeded);
        let usage = observation.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (11, 7));
        assert!(receiver.try_recv().is_err());
        assert_eq!(provider.transport.requests.lock().unwrap().len(), 1);
    }
}
