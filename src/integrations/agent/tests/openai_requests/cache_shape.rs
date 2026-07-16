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
                label: "active repository instructions".to_string(),
                content: "Prefer deterministic request shapes.".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
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
        "mez-fcc0c3076055b2040cb8727ead0dbe7c"
    );
    assert_eq!(diagnostics.instructions_bytes, 44_490);
    assert_eq!(
        diagnostics.instructions_sha256,
        "a34dcdeef28829701e39022f7250dfba0bdac27a2c516083d574f7d8280d291b"
    );
    assert_eq!(diagnostics.response_format_bytes, 4);
    assert_eq!(
        diagnostics.response_format_sha256,
        "74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b"
    );
    assert_eq!(diagnostics.tools_bytes, 19_103);
    assert_eq!(
        diagnostics.tools_sha256,
        "8f3fc2b63137b6701dbb3e17252b786a650b524de49c60f31d307df78fa47da4"
    );
    assert_eq!(diagnostics.tool_choice_bytes, 53);
    assert_eq!(
        diagnostics.tool_choice_sha256,
        "6667323a2b74449448aad3d609d98e5288910331b10d71e6f482da3e076eab4e"
    );
    assert_eq!(diagnostics.stable_prompt_prefix_bytes, 44_653);
    assert_eq!(
        diagnostics.stable_prompt_prefix_sha256,
        "fb59fd2449bf99d2a17d9610db82a7204bc20f352ea5c06b15d000cfc1278573"
    );
    assert_eq!(diagnostics.provider_request_shape_bytes, 19_314);
    assert_eq!(
        diagnostics.provider_request_shape_sha256,
        "4d4e5347d645e36269c1e194ec03d60aa49515a21f58551aef26751a8f556bc2"
    );
}

#[test]
/// Verifies large MCP catalogs keep server-level routing context visible.
///
/// The OpenAI function-tool description is the first routing surface the model
/// sees for callable MCP integrations. When there are more callable tools than
/// the compact tool list can enumerate, the schema should still provide a
/// bounded server-level summary so overlapping tool names retain their server
/// purpose and routing context.
fn openai_responses_request_body_summarizes_large_mcp_catalog_by_server() {
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

    assert!(
        description.contains(
            "Available MCP servers callable with mcp_call: server00 (Server 00 operations; tools: search)"
        ),
        "{description}"
    );
    assert!(
        description.contains("... plus 2 more MCP servers listed in the schema"),
        "{description}"
    );
    assert!(
        description.contains("... plus 2 more MCP tools listed in the schema"),
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
    let first_prefix = openai_stable_prefix_material_for_request(&first).unwrap();
    let second_prefix = openai_stable_prefix_material_for_request(&second).unwrap();

    assert_eq!(first_prefix, second_prefix);
    assert_eq!(
        first_value["prompt_cache_key"],
        second_value["prompt_cache_key"]
    );
    assert_eq!(
        first_value["prompt_cache_key"],
        execution_value["prompt_cache_key"]
    );
}
