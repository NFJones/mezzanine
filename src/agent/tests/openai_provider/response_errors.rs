//! Openai Provider tests for response errors behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies that provider HTTP failures surface the response error message.
/// This keeps auth regressions actionable instead of reducing them to an
/// undifferentiated status code such as `401`.
fn openai_provider_http_error_includes_provider_message() {
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
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 401,
            headers: Default::default(),
            body: r#"{"error":{"message":"invalid account token","type":"invalid_request_error","code":"bad_account","access_token":"should-redact"}}"#.to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let error = provider.send_request(&request).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("401"), "{}", error.message());
    assert!(
        error.message().contains("invalid account token"),
        "{}",
        error.message()
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["status_code"], 401);
    assert_eq!(failure_json["error"]["message"], "invalid account token");
    assert_eq!(failure_json["error"]["type"], "invalid_request_error");
    assert_eq!(failure_json["error"]["code"], "bad_account");
    assert_eq!(failure_json["error"]["access_token"], "[REDACTED]");
}

#[test]
/// Verifies provider HTTP failure sanitization redacts secret-like strings
/// even when upstream places the credential under generic fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn openai_provider_http_error_redacts_secret_like_generic_values() {
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
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 401,
            headers: Default::default(),
            body: r#"{"error":{"message":"Bearer sk-test-secret leaked","type":"invalid_request_error","code":"bad_account","details":"jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhY2N0In0.signaturex"}}"#.to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let error = provider.send_request(&request).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(!error.message().contains("sk-test-secret"));
    assert!(error.message().contains("[REDACTED]"));
    let failure_json = error.provider_failure_json().unwrap();
    assert!(!failure_json.contains("sk-test-secret"));
    assert!(!failure_json.contains("eyJhbGciOiJIUzI1NiJ9"));
    let failure_json: serde_json::Value = serde_json::from_str(failure_json).unwrap();
    assert_eq!(failure_json["error"]["message"], "[REDACTED]");
    assert_eq!(failure_json["error"]["details"], "[REDACTED]");
}

#[test]
/// Verifies that streaming provider failure events preserve the structured
/// failure object for runtime audit records. ChatGPT-backed OpenAI auth uses
/// the streaming endpoint, so these diagnostics must survive SSE parsing.
fn openai_provider_stream_failure_includes_provider_failure_object() {
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
            headers: Default::default(),
            body: format!(
                "event: response.failed\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_failed",
                        "error": {
                            "message": "stream must be set to true",
                            "type": "invalid_request_error",
                            "code": "missing_required_parameter"
                        }
                    }
                })
            ),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
        "test-key",
        "https://example.test/responses",
        10,
        std::collections::BTreeMap::new(),
        true,
        transport,
    )
    .unwrap();

    let error = provider.send_request(&request).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error.message().contains("stream must be set to true"),
        "{}",
        error.message()
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["response_id"], "resp_failed");
    assert_eq!(
        failure_json["error"]["message"],
        "stream must be set to true"
    );
    assert_eq!(failure_json["error"]["type"], "invalid_request_error");
    assert_eq!(failure_json["error"]["code"], "missing_required_parameter");
}

#[test]
/// Verifies output-limit incomplete streaming responses keep structured
/// diagnostics so runtime recovery can retry compactly instead of failing the
/// turn as an opaque invalid provider state.
fn openai_provider_stream_incomplete_output_limit_is_recoverable() {
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
            headers: Default::default(),
            body: format!(
                "event: response.incomplete\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.incomplete",
                    "response": {
                        "id": "resp_incomplete",
                        "model": "gpt-test",
                        "incomplete_details": {
                            "reason": "max_output_tokens"
                        }
                    }
                })
            ),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
        "test-key",
        "https://example.test/responses",
        10,
        std::collections::BTreeMap::new(),
        true,
        transport,
    )
    .unwrap();

    let error = provider.send_request(&request).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error.message().contains("max_output_tokens"),
        "{}",
        error.message()
    );
    assert_eq!(
        crate::agent::provider_error_retry_class(&error),
        mez_agent::ProviderErrorRetryClass::OutputLimit
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(
        failure_json["incomplete_details"]["reason"],
        "max_output_tokens"
    );
}
