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
            model_capabilities: Default::default(),
            reasoning_profile: Some("high".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
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
    assert_eq!(value["prompt_cache_retention"], "24h");
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
            model_capabilities: Default::default(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options,
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
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
/// Verifies Responses cache controls follow the documented canonical model
/// generation rather than treating every OpenAI model as one API-wide cache
/// contract. This keeps legacy retention controls away from GPT-5.6 while
/// rejecting unsupported values before they reach the provider.
fn openai_responses_request_body_applies_generation_aware_cache_controls() {
    let mut earlier = openai_prompt_cache_retention_test_request("gpt-5.4");
    earlier.prompt_cache_retention = Some("in_memory".to_string());
    let earlier: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&earlier).unwrap()).unwrap();
    assert_eq!(earlier["prompt_cache_retention"], "in_memory");
    assert!(earlier.get("prompt_cache_options").is_none());

    let mut gpt_45 = openai_prompt_cache_retention_test_request("gpt-4.5-2025-02-27");
    gpt_45.prompt_cache_retention = Some("in_memory".to_string());
    let gpt_45: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&gpt_45).unwrap()).unwrap();
    assert_eq!(gpt_45["prompt_cache_retention"], "in_memory");

    let mut gpt_55 = openai_prompt_cache_retention_test_request("gpt-5.5-pro-2026-01-01");
    gpt_55.prompt_cache_retention = Some("24h".to_string());
    let gpt_55: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&gpt_55).unwrap()).unwrap();
    assert_eq!(gpt_55["prompt_cache_retention"], "24h");

    let mut gpt_56 = openai_prompt_cache_retention_test_request("gpt-5.6-2026-01-01");
    gpt_56.prompt_cache_retention = Some("30m".to_string());
    let gpt_56: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&gpt_56).unwrap()).unwrap();
    assert_eq!(gpt_56["prompt_cache_options"]["ttl"], "30m");
    assert!(gpt_56["prompt_cache_options"].get("mode").is_none());
    assert!(gpt_56.get("prompt_cache_retention").is_none());

    let mut explicit_gpt_56 = openai_prompt_cache_retention_test_request("gpt-5.6-2026-01-01");
    explicit_gpt_56.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Developer,
        source: mez_agent::ContextSourceKind::ProjectGuidance,
        placement: mez_agent::ContextPlacement::StablePrefix,
        content: "stable cache boundary".to_string(),
    });
    explicit_gpt_56.model_capabilities.openai_prompt_cache_mode =
        mez_agent::model_capabilities::OpenAiPromptCacheMode::Explicit;
    let explicit_diagnostics =
        openai_prompt_cache_diagnostics_for_request(&explicit_gpt_56).unwrap();
    let explicit_gpt_56: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&explicit_gpt_56).unwrap()).unwrap();
    assert_eq!(explicit_gpt_56["prompt_cache_options"]["ttl"], "30m");
    assert_eq!(explicit_gpt_56["prompt_cache_options"]["mode"], "explicit");
    let breakpoint = explicit_gpt_56["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "developer")
        .unwrap()["content"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(breakpoint["prompt_cache_breakpoint"]["mode"], "explicit");
    assert_eq!(
        explicit_diagnostics.effective_input_bytes,
        serde_json::to_string(&explicit_gpt_56["input"])
            .unwrap()
            .len()
    );

    let mut explicit_without_developer =
        openai_prompt_cache_retention_test_request("gpt-5.6-2026-01-01");
    explicit_without_developer
        .model_capabilities
        .openai_prompt_cache_mode = mez_agent::model_capabilities::OpenAiPromptCacheMode::Explicit;
    assert!(
        openai_responses_request_body(&explicit_without_developer).is_err(),
        "explicit mode must not mark the volatile user input as a cache breakpoint"
    );

    let gpt_6 = openai_prompt_cache_retention_test_request("gpt-6-astra");
    let gpt_6: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&gpt_6).unwrap()).unwrap();
    assert_eq!(gpt_6["prompt_cache_options"]["ttl"], "30m");
    assert!(gpt_6.get("prompt_cache_retention").is_none());

    let mut metadata_override =
        openai_prompt_cache_retention_test_request("custom-responses-model");
    metadata_override
        .model_capabilities
        .openai_prompt_cache_generation =
        Some(mez_agent::model_capabilities::OpenAiPromptCacheGeneration::Gpt56OrNewer);
    let metadata_override: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&metadata_override).unwrap()).unwrap();
    assert_eq!(metadata_override["prompt_cache_options"]["ttl"], "30m");

    for (model, retention) in [
        ("gpt-4.5", "24h"),
        ("gpt-5.5", "in_memory"),
        ("gpt-5.6", "24h"),
        ("custom-responses-model", "24h"),
    ] {
        let mut request = openai_prompt_cache_retention_test_request(model);
        request.prompt_cache_retention = Some(retention.to_string());
        assert!(
            openai_responses_request_body(&request).is_err(),
            "{model} unexpectedly accepted {retention}"
        );
    }
}

/// Returns the rendered input positions carrying an explicit cache breakpoint.
fn explicit_prompt_cache_breakpoint_positions(
    request: &mez_agent::ModelRequest,
) -> Vec<(usize, usize)> {
    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(request).unwrap()).unwrap();
    let input = body["input"].as_array().unwrap();
    let mut positions = Vec::new();
    for (message_index, message) in input.iter().enumerate() {
        let Some(content) = message.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for (block_index, block) in content.iter().enumerate() {
            if block.get("prompt_cache_breakpoint").is_some() {
                positions.push((message_index, block_index));
            }
        }
    }
    positions
}

/// Verifies explicit cache checkpoints retain the stationary prefix while
/// advancing across settled durable chronology.
///
/// GPT-5.6 supports four explicit writes. The oldest stable checkpoint remains
/// available while up to three newest settled transcript messages advance the
/// reusable prefix; current request-local context remains unmarked.
#[test]
fn openai_explicit_prompt_cache_checkpoints_follow_settled_chronology() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-5.6-2026-01-01");
    request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Developer,
        source: mez_agent::ContextSourceKind::ProjectGuidance,
        placement: mez_agent::ContextPlacement::StablePrefix,
        content: "stable cache boundary".to_string(),
    });
    request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Developer,
        source: mez_agent::ContextSourceKind::ProjectGuidance,
        placement: mez_agent::ContextPlacement::StablePrefix,
        content: "latest stable cache boundary".to_string(),
    });
    request.model_capabilities.openai_prompt_cache_mode =
        mez_agent::model_capabilities::OpenAiPromptCacheMode::Explicit;
    let stable_positions = explicit_prompt_cache_breakpoint_positions(&request);
    assert_eq!(
        stable_positions.len(),
        1,
        "the stationary prefix needs one initial checkpoint: {stable_positions:?}"
    );

    for index in 0..5 {
        request.messages.push(mez_agent::ModelMessage {
            role: mez_agent::ModelMessageRole::User,
            source: mez_agent::ContextSourceKind::TranscriptUser,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            content: format!("settled user turn {index}"),
        });
    }
    request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Context,
        source: mez_agent::ContextSourceKind::RuntimeHint,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        content: "current request state is volatile".to_string(),
    });

    let checkpoints = explicit_prompt_cache_breakpoint_positions(&request);
    assert_eq!(
        checkpoints.len(),
        4,
        "cache writes must stay bounded: {checkpoints:?}"
    );
    assert_eq!(checkpoints[0], stable_positions[0]);
    assert_eq!(
        checkpoints,
        vec![(2, 0), (5, 0), (6, 0), (7, 0)],
        "the final stable-prefix boundary and newest three settled messages should remain lookup candidates"
    );
}

/// Verifies a stable non-developer block can carry an explicit checkpoint.
///
/// SPEC.md permits one to four semantic `input_text` checkpoints and does not
/// restrict their roles. A minimal session can have no stable developer block
/// at all, so the stationary boundary may be any stable-prefix block while
/// volatile developer blocks must still be ignored.
#[test]
fn openai_explicit_prompt_cache_breakpoint_marks_a_stable_non_developer_block() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-5.6-2026-01-01");
    request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::User,
        source: mez_agent::ContextSourceKind::UserInstruction,
        placement: mez_agent::ContextPlacement::StablePrefix,
        content: "stable operator instruction".to_string(),
    });
    request.model_capabilities.openai_prompt_cache_mode =
        mez_agent::model_capabilities::OpenAiPromptCacheMode::Explicit;
    let stable_positions = explicit_prompt_cache_breakpoint_positions(&request);
    assert_eq!(
        stable_positions.len(),
        1,
        "explicit mode must emit exactly one breakpoint: {stable_positions:?}"
    );

    request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Context,
        source: mez_agent::ContextSourceKind::Policy,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        content: "volatile runtime hint".to_string(),
    });
    assert_eq!(
        explicit_prompt_cache_breakpoint_positions(&request),
        stable_positions,
        "volatile developer-rendered blocks must not become the breakpoint"
    );
}

/// Verifies explicit mode still fails closed without a stable input block.
///
/// The breakpoint is mandatory in explicit mode and may only attach to an
/// `input_text` block, so a request whose every candidate block is volatile
/// chronology has no legal boundary: assembly must refuse rather than send an
/// unmarked explicit-mode request or mark the volatile tail.
#[test]
fn openai_explicit_prompt_cache_breakpoint_requires_a_stable_input_block() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-5.6-2026-01-01");
    request.model_capabilities.openai_prompt_cache_mode =
        mez_agent::model_capabilities::OpenAiPromptCacheMode::Explicit;
    request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Context,
        source: mez_agent::ContextSourceKind::Policy,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        content: "volatile runtime hint".to_string(),
    });
    let error = openai_responses_request_body(&request)
        .expect_err("explicit mode without a stable-prefix input_text block must fail closed");
    assert!(
        error
            .to_string()
            .contains("requires a stable-prefix input_text content block"),
        "the guard must name the stable-prefix requirement: {error}"
    );
}
