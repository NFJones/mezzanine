//! Openai Requests tests for options behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies OpenAI Responses request bodies carry the selected reasoning effort
/// through the provider-specific `reasoning` field. This protects automatic
/// reasoning and explicit model picker selections from silently dropping the
/// configured reasoning level.
fn openai_responses_request_body_includes_reasoning_effort() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-5.1".to_string(),
            reasoning_profile: Some("high".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "debug this failing test".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.reasoning_effort = Some("high".to_string());
    request.prompt_cache_retention = Some("24h".to_string());

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(value["reasoning"]["effort"], "high");
    assert!(value.get("prompt_cache_retention").is_none());
}

#[test]
/// Verifies OpenAI Responses request bodies do not serialize the configured
/// output-token cap even when retries raise `ModelRequest.max_output_tokens`.
/// OpenAI rejects the legacy wire field, so recovery must adjust provider
/// behavior without emitting `max_output_tokens` on the Responses path.
fn openai_responses_request_body_omits_configured_max_output_tokens() {
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("max_output_tokens".to_string(), "12000".to_string());
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-5.1".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options,
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "keep the response compact".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(request.max_output_tokens, Some(12000));
    assert!(value.get("max_output_tokens").is_none());
    assert!(
        value["prompt_cache_key"]
            .as_str()
            .is_some_and(|key| key.starts_with("mez-"))
    );

    request.max_output_tokens = Some(24000);
    let retry_body = openai_responses_request_body(&request).unwrap();
    let retry_value: serde_json::Value = serde_json::from_str(&retry_body).unwrap();

    assert!(retry_value.get("max_output_tokens").is_none());
}

#[test]
/// Verifies OpenAI Responses requests omit prompt-cache retention controls.
///
/// OpenAI input caching is automatic for eligible prefixes. The Responses API
/// rejects a `prompt_cache_retention` request field, so stale profile options
/// must not leak into the provider-visible JSON body.
fn openai_responses_request_body_omits_prompt_cache_retention_option() {
    for retention in ["24h", "in_memory", "forever"] {
        let mut request = openai_prompt_cache_retention_test_request("gpt-5.5");
        request.prompt_cache_retention = Some(retention.to_string());

        let body = openai_responses_request_body(&request).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(value.get("prompt_cache_retention").is_none(), "{retention}");
        assert!(
            value["prompt_cache_key"]
                .as_str()
                .is_some_and(|key| key.starts_with("mez-")),
            "{retention}"
        );
    }
}
