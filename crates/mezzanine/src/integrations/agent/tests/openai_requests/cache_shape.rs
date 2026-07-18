//! Openai Requests tests for cache shape behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies a representative OpenAI Responses request has stable canonical
/// request-shape fixture values for cache diagnostics.
///
/// This covers the provider-visible request pieces that affect cache affinity:
/// instructions, prompt-cache routing key, stable prefix material, tools,
/// forced tool choice, response-format shape, and the aggregate provider
/// request-shape fingerprint. Exact values are intentionally pinned so schema
/// or request-shape drift is reviewed instead of silently fragmenting cache
/// reuse.
fn openai_responses_request_body_has_canonical_cache_shape_fixture() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-5.4".to_string(),
        reasoning_profile: Some("medium".to_string()),
        latency_preference: Some("fast".to_string()),
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let mut request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "active repository instructions".to_string(),
                content: "Prefer deterministic request shapes.".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "Inspect cache stability.".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    request.prompt_cache_retention = Some("24h".to_string());
    request.max_output_tokens = Some(16_384);

    let body_text = openai_responses_request_body(&request).unwrap();
    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();

    assert_eq!(body["model"], "gpt-5.4");
    assert!(body.get("prompt_cache_retention").is_none());
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], false);
    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(body["tool_choice"]["name"], "submit_maap_action_batch");
    assert!(body["text"]["format"].is_null());
    assert!(
        body["instructions"]
            .as_str()
            .unwrap()
            .contains("Prefer deterministic request shapes.")
    );
    assert!(body["input"].as_array().unwrap().iter().any(|message| {
        message["role"] == "user"
            && message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Inspect cache stability."))
    }));
    assert!(body["tools"].as_array().unwrap().iter().any(|tool| {
        tool["name"] == "submit_maap_action_batch"
            && tool["parameters"]["properties"]["actions"]["minItems"] == 1
    }));
    assert_eq!(body["prompt_cache_key"], diagnostics.prompt_cache_key);

    eprintln!("DIAGNOSTICS {diagnostics:#?}");
    assert_eq!(
        diagnostics.prompt_cache_key,
        "mez-a4b0d51524d4da197a9dc89076e692e3"
    );
    assert_eq!(diagnostics.instructions_bytes, 41_863);
    assert_eq!(
        diagnostics.instructions_sha256,
        "89112609b4af31d688c4352c4c8f56cad21c8dd90c2814ccb2ebeeb52bfe1142"
    );
    assert_eq!(diagnostics.response_format_bytes, 4);
    assert_eq!(
        diagnostics.response_format_sha256,
        "74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b"
    );
    assert_eq!(diagnostics.tools_bytes, 20_134);
    assert_eq!(
        diagnostics.tools_sha256,
        "aea57d9da245864bc709b59e35534fd92345447c9052f600413e4b5a4f63a41f"
    );
    assert_eq!(diagnostics.tool_choice_bytes, 53);
    assert_eq!(
        diagnostics.tool_choice_sha256,
        "6667323a2b74449448aad3d609d98e5288910331b10d71e6f482da3e076eab4e"
    );
    assert_eq!(diagnostics.stable_projection_bytes, 42_284);
    assert_eq!(
        diagnostics.stable_projection_sha256,
        "a454fe685dfd8e2c6799c1aeeca32901d0393eb62e48a413a908ea540fe0795a"
    );
    assert_eq!(diagnostics.provider_request_shape_bytes, 20_345);
    assert_eq!(
        diagnostics.provider_request_shape_sha256,
        "1a29526ca4058cf424bc531b1bb920b1754804853829a4343e1dec11937d60a3"
    );
}

#[test]
/// Verifies large MCP catalogs cannot change the OpenAI function-tool bytes.
///
/// MCP routing metadata belongs in late injected context. The function schema
/// instead exposes one generic MCP variant so request-local catalogs do not
/// invalidate the provider's cached tool prefix.
fn openai_responses_request_body_excludes_large_mcp_catalog_from_tools() {
    let mut request = assemble_model_request(
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
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "use an MCP server".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Mcp);
    request.available_mcp_tools = (0..22)
        .map(|index| McpPromptTool {
            server_id: format!("server{index:02}"),
            tool_name: "search".to_string(),
            description: format!(
                "Search records. User-configured non-authoritative server purpose: Server {index:02} operations."
            ),
            approval_required: false,
            input_schema_json: r#"{"type":"object","properties":{"query":{"type":"string"}}}"#
                .to_string(),
        })
        .collect();

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mcp_tool = openai_function_tool(&value, "submit_maap_action_batch");
    let description = mcp_tool["description"].as_str().unwrap();

    assert!(description.contains("The schema includes a generic mcp_call action"));
    assert!(!description.contains("server00"), "{description}");
    assert!(
        !description.contains("Server 00 operations"),
        "{description}"
    );
}

#[test]
/// Verifies OpenAI prompt-cache routing keys stay coarse enough to avoid
/// fragmenting identical static prefixes across interaction modes.
fn openai_responses_request_body_uses_stable_derived_prompt_cache_key() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let first = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "first prompt".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let second = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "different prompt".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let mut execution = second.clone();
    execution.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    execution.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Shell);

    let first_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&first).unwrap()).unwrap();
    let second_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&second).unwrap()).unwrap();
    let execution_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&execution).unwrap()).unwrap();
    let first_prefix = openai_stable_projection_material_for_request(&first).unwrap();
    let second_prefix = openai_stable_projection_material_for_request(&second).unwrap();

    assert_ne!(first_prefix, second_prefix);
    assert_eq!(
        first_value["prompt_cache_key"],
        second_value["prompt_cache_key"]
    );
    assert_eq!(
        first_value["prompt_cache_key"],
        execution_value["prompt_cache_key"]
    );
}
