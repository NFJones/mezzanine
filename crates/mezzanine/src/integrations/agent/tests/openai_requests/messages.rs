//! Openai Requests tests for messages behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies openai responses request body maps context to responses api shape.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn openai_responses_request_body_maps_context_to_responses_api_shape() {
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
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Policy,
                label: "policy".to_string(),
                content: "approval_policy=ask".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "hello".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let capability_tool = openai_function_tool(&value, "submit_maap_action_batch");

    assert_openai_strict_schema_shape(&capability_tool["parameters"]);
    assert_eq!(value["model"], "gpt-test");
    assert_eq!(value["stream"], false);
    assert_eq!(value["store"], false);
    assert!(
        value["prompt_cache_key"]
            .as_str()
            .is_some_and(|key| { key.starts_with("mez-") && key.len() == "mez-".len() + 32 })
    );
    assert_eq!(value["parallel_tool_calls"], false);
    assert!(value.get("text").is_none());
    assert_eq!(capability_tool["type"], "function");
    assert_eq!(capability_tool["strict"], true);
    assert_eq!(value["tool_choice"]["name"], "submit_maap_action_batch");
    assert_eq!(
        value["tools"].as_array().unwrap().len(),
        1,
        "OpenAI should receive one canonical MAAP function tool"
    );
    let schema_properties = capability_tool["parameters"]["properties"]
        .as_object()
        .unwrap();
    assert_eq!(
        capability_tool["parameters"]["required"],
        serde_json::json!(["rationale", "thought", "actions"])
    );
    let capability_description = capability_tool["description"].as_str().unwrap();
    assert!(capability_description.contains("Return a function call, not prose"));
    assert!(capability_description.contains("currently allowed actions"));
    assert!(capability_description.contains("transport envelope"));
    assert!(capability_description.contains("not a prerequisite task step"));
    assert!(capability_description.contains("required-function-call"));
    assert!(capability_description.contains("Choose the smallest action"));
    assert!(capability_description.contains("missing information, parameters, or identifiers"));
    assert!(
        capability_description
            .contains("request or use the relevant capability instead of asking the user")
    );
    assert!(capability_description.contains("identifiers, URLs, versions"));
    assert!(capability_description.contains("facts already present in current action results"));
    assert!(capability_description.contains("Capability map: shell=local files"));
    assert!(capability_description.contains("Wrong: say(blocked"));
    assert!(capability_description.contains("Right: request_capability(capability=\"shell\""));
    assert!(schema_properties.contains_key("rationale"));
    assert!(schema_properties.contains_key("thought"));
    assert!(!schema_properties.contains_key("protocol"));
    assert!(!schema_properties.contains_key("turn_id"));
    assert!(!schema_properties.contains_key("agent_id"));
    assert!(!schema_properties.contains_key("final"));
    assert_eq!(
        capability_tool["parameters"]["properties"]["rationale"]["minLength"],
        1
    );
    assert_eq!(
        capability_tool["parameters"]["properties"]["thought"]["type"],
        serde_json::json!(["string", "null"])
    );
    let rationale_description =
        capability_tool["parameters"]["properties"]["rationale"]["description"]
            .as_str()
            .unwrap();
    assert!(rationale_description.contains("Terse additive reason"));
    assert!(rationale_description.contains("actions are next"));
    assert!(rationale_description.contains("directly advances the user task"));
    assert!(rationale_description.contains("required function call"));
    assert!(rationale_description.contains("current-actions call"));
    assert!(rationale_description.contains("schema wrapper"));
    assert!(rationale_description.contains("Do not restate the user request"));
    assert!(rationale_description.contains("prior rationale"));
    assert!(rationale_description.contains("progress say"));
    let thought_description = capability_tool["parameters"]["properties"]["thought"]["description"]
        .as_str()
        .unwrap();
    assert!(
        thought_description.contains("Optional longer durable work note"),
        "{thought_description}"
    );
    assert!(
        thought_description.contains("Use only for substantive learning"),
        "{thought_description}"
    );
    assert!(
        thought_description.contains("future context"),
        "{thought_description}"
    );
    assert!(
        thought_description.contains("Do not include secrets"),
        "{thought_description}"
    );
    assert!(
        thought_description.contains("private chain-of-thought"),
        "{thought_description}"
    );
    assert!(
        rationale_description.len() < 420,
        "batch rationale schema should stay compact: {rationale_description}"
    );
    assert!(
        thought_description.len() < 320,
        "thought schema should stay compact: {thought_description}"
    );
    assert_eq!(
        openai_tool_action_schemas(capability_tool).len(),
        16,
        "the canonical OpenAI tool exposes a stable action superset with generic MCP"
    );
    assert_eq!(
        capability_tool["parameters"]["properties"]["actions"]["minItems"],
        1
    );
    assert!(
        capability_tool["description"]
            .as_str()
            .unwrap()
            .contains("Model-selected skill lookup/loading is disabled"),
        "{}",
        capability_tool["description"]
    );
    let action_schemas = openai_tool_action_schemas(capability_tool);
    let action_types = openai_tool_action_types(capability_tool);
    assert!(action_types.contains(&"say".to_string()));
    assert!(action_types.contains(&"request_capability".to_string()));
    assert!(action_types.contains(&"shell_command".to_string()));
    assert!(action_types.contains(&"apply_patch".to_string()));
    assert!(action_types.contains(&"web_search".to_string()));
    assert!(action_types.contains(&"fetch_url".to_string()));
    assert!(action_types.contains(&"send_message".to_string()));
    assert!(action_types.contains(&"spawn_agent".to_string()));
    assert!(action_types.contains(&"config_change".to_string()));
    assert!(action_types.contains(&"memory_search".to_string()));
    assert!(action_types.contains(&"memory_store".to_string()));
    assert!(action_types.contains(&"issue_add".to_string()));
    assert!(action_types.contains(&"issue_update".to_string()));
    assert!(action_types.contains(&"issue_query".to_string()));
    assert!(action_types.contains(&"issue_delete".to_string()));
    assert!(!action_types.contains(&"request_skills".to_string()));
    assert!(!action_types.contains(&"call_skill".to_string()));
    assert!(action_types.contains(&"mcp_call".to_string()));
    let removed_user_input_action = ["request", "user_input"].join("_");
    assert!(!action_types.contains(&removed_user_input_action));
    assert!(!action_types.contains(&"abort".to_string()));
    assert!(
        !action_types.contains(&"complete".to_string()),
        "{action_types:?}"
    );
    assert!(action_schemas.iter().all(|schema| {
        !schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("rationale")
    }));
    let say_schema = action_schemas
        .iter()
        .find(|schema| schema["properties"]["type"]["enum"][0] == "say")
        .unwrap();
    assert_eq!(
        say_schema["required"],
        serde_json::json!(["type", "status", "content_type", "text"])
    );
    assert_eq!(
        say_schema["properties"]["status"]["enum"],
        serde_json::json!(["progress", "final", "blocked"])
    );
    let say_status_description = say_schema["properties"]["status"]["description"]
        .as_str()
        .unwrap();
    assert!(
        say_status_description.contains("progress for a new sequence-point update"),
        "{say_status_description}"
    );
    assert!(say_status_description.contains("final when the user goal is complete"));
    assert!(say_status_description.contains("blocked when external input/state is required"));
    assert!(say_status_description.contains("Do not pair final or blocked"));
    assert!(
        say_status_description.len() < 320,
        "say status schema should stay compact: {say_status_description}"
    );
    let say_text_description = say_schema["properties"]["text"]["description"]
        .as_str()
        .unwrap();
    assert!(say_text_description.contains("User-visible text"));
    assert!(say_text_description.contains("Display-only"));
    assert!(say_text_description.contains("commands and patch blocks here do not execute"));
    assert!(say_text_description.contains("compact new learning"));
    assert!(say_text_description.contains("blocker delta"));
    assert!(
        say_text_description.contains("omit it if it repeats prior progress"),
        "{say_text_description}"
    );
    assert!(
        say_text_description.len() < 520,
        "say text schema should stay compact: {say_text_description}"
    );
    assert_eq!(
        say_schema["properties"]["content_type"]["enum"],
        serde_json::json!([
            "text/plain; charset=utf-8",
            "text/markdown; charset=utf-8",
            "text/x-diff; charset=utf-8"
        ])
    );
    assert_eq!(say_schema["properties"]["text"]["minLength"], 1);
    assert!(
        say_schema["properties"]["text"]["description"]
            .as_str()
            .unwrap()
            .contains("Display-only")
    );
    assert!(
        say_schema["properties"]["text"]["description"]
            .as_str()
            .unwrap()
            .contains("commands and patch blocks here do not execute")
    );
    assert!(
        say_schema["properties"]["text"]["description"]
            .as_str()
            .unwrap()
            .contains("rationale")
    );
    let capability_schema = action_schemas
        .iter()
        .find(|schema| schema["properties"]["type"]["enum"][0] == "request_capability")
        .unwrap();
    assert_eq!(
        capability_schema["properties"]["capability"]["enum"],
        serde_json::json!(mez_agent::AgentCapability::all_names())
    );
    let capability_description = capability_schema["properties"]["capability"]["description"]
        .as_str()
        .unwrap();
    assert!(
        capability_description
            .contains("Capability map: shell exposes shell_command and apply_patch"),
        "{capability_description}"
    );
    assert!(
        capability_description.contains("network_search exposes web_search"),
        "{capability_description}"
    );
    assert!(
        capability_description.contains("network_fetch exposes fetch_url"),
        "{capability_description}"
    );
    assert!(
        capability_description.contains("subagent exposes send_message and spawn_agent"),
        "{capability_description}"
    );
    assert!(
        capability_description.contains("config_change exposes config_change"),
        "{capability_description}"
    );
    assert!(
        capability_description.contains("memory exposes memory_search and memory_store"),
        "{capability_description}"
    );
    assert!(
        capability_description
            .contains("issues exposes issue_add, issue_update, issue_query, and issue_delete"),
        "{capability_description}"
    );
    let capability_reason_description = capability_schema["properties"]["reason"]["description"]
        .as_str()
        .unwrap();
    assert_eq!(capability_schema["properties"]["reason"]["minLength"], 1);
    assert!(
        capability_reason_description.contains("next concrete action or evidence needed"),
        "{capability_reason_description}"
    );
    assert!(
        capability_reason_description.contains("Do not ask the user to grant access here"),
        "{capability_reason_description}"
    );
    assert!(
        value["instructions"]
            .as_str()
            .unwrap()
            .contains("Mezzanine pane agent")
    );
    assert_eq!(value["input"].as_array().unwrap().len(), 3);
    assert_eq!(value["input"][0]["role"], "developer");
    assert!(
        value["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[policy]")
    );
    assert_eq!(value["input"][1]["role"], "user");
    assert!(
        value["input"][1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[user]")
    );
    assert_eq!(value["input"][2]["role"], "developer");
    let allowed_surface = value["input"][2]["content"][0]["text"].as_str().unwrap();
    assert!(allowed_surface.contains("[allowed action surface]"));
    assert!(allowed_surface.contains("interaction_kind="));
    assert!(allowed_surface.contains("allowed_actions=say,request_capability"));
    assert!(allowed_surface.contains("active_function_tool=submit_maap_action_batch"));
    assert!(allowed_surface.contains("Emit only action objects whose type appears"));
    assert!(
        !allowed_surface.contains("authoritative for action eligibility"),
        "{allowed_surface}"
    );
    assert!(
        !allowed_surface.contains("one canonical MAAP action-batch function"),
        "{allowed_surface}"
    );
    assert!(
        !allowed_surface.contains("Treat [executed result]"),
        "{allowed_surface}"
    );
}

#[test]
/// Verifies OpenAI request rendering keeps Mezzanine action results
/// provider-valid while marking them as executed evidence.
///
/// Responses input messages do not have a generic tool role for synthetic
/// Mezzanine action history, so the provider renderer must carry provenance in
/// the text instead of letting tool output look like a fresh user request.
fn openai_responses_request_body_marks_action_results_as_execution_evidence() {
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
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "verify the plan file exists".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: "action result shell".to_string(),
                content: "[action_result action-1 shell_command succeeded]\ncommand output marker"
                    .to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let input = value["input"].as_array().unwrap();
    let user_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("verify the plan file exists"))
        })
        .unwrap();
    let action_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("[executed result transcript entry]"))
        })
        .unwrap();
    let action_message = &input[action_index];
    let action_text = action_message["content"][0]["text"].as_str().unwrap();

    assert!(
        user_index < action_index,
        "action evidence should remain after the user request it answers"
    );
    assert_eq!(action_message["role"], "user");
    assert!(
        action_text.starts_with("[executed result transcript entry]\n"),
        "{action_text}"
    );
    assert!(
        action_text.contains("execution evidence, not as a user request"),
        "{action_text}"
    );
    assert!(
        action_text.contains("[action_result action-1 shell_command succeeded]"),
        "{action_text}"
    );
}

#[test]
/// Verifies prior user transcript entries are marked as inactive history.
///
/// Large context windows can contain earlier user prompts that would be valid
/// standalone requests. The OpenAI renderer must keep those prompts available
/// for references while clearly separating them from the current active task.
fn openai_responses_request_body_marks_prior_user_history_inactive() {
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
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptUser,
                label: "previous user message".to_string(),
                content: "Output a large multiline JSON object".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user prompt".to_string(),
                content: "Patch the prompt context manager".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let input = value["input"].as_array().unwrap();

    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["role"], "user");
    let historical_text = input[0]["content"][0]["text"].as_str().unwrap();
    assert!(historical_text.contains("[user prompt transcript entry]"));
    assert!(historical_text.contains("ordered conversation transcript"));
    assert!(historical_text.contains("Output a large multiline JSON object"));

    assert_eq!(input[1]["role"], "user");
    let current_text = input[1]["content"][0]["text"].as_str().unwrap();
    assert!(current_text.contains("[user prompt transcript entry]"));
    assert!(current_text.contains("ordered conversation transcript"));
    assert!(current_text.contains("Patch the prompt context manager"));

    assert_eq!(input[2]["role"], "developer");
    let allowed_surface = input[2]["content"][0]["text"].as_str().unwrap();
    assert!(allowed_surface.contains("[allowed action surface]"));
    assert!(allowed_surface.contains("interaction_kind="));
    assert!(allowed_surface.contains("allowed_actions=say,request_capability"));
    assert!(allowed_surface.contains("active_function_tool=submit_maap_action_batch"));
    assert!(
        !allowed_surface.contains("latest user prompt is the active task"),
        "{allowed_surface}"
    );
}

#[test]
/// Verifies assistant transcript context is serialized with an assistant role.
///
/// Prior assistant messages are not new user instructions. The Responses
/// request body must preserve their role so follow-up references resolve
/// against chat history instead of a flattened user transcript block.
fn openai_responses_request_body_preserves_assistant_history_role() {
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
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "transcript assistant entry 2 for pane %1".to_string(),
                content: "Suggested changes:\n1. A\n2. B".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user prompt".to_string(),
                content: "Do item 2".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let input = value["input"].as_array().unwrap();

    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["role"], "assistant");
    assert_eq!(input[0]["content"][0]["type"], "output_text");
    assert!(
        input[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("2. B")
    );
    assert_eq!(input[1]["role"], "user");
    assert_eq!(input[1]["content"][0]["type"], "input_text");
    assert!(
        input[1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Do item 2")
    );
    assert_eq!(input[2]["role"], "developer");
    assert!(
        input[2]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[allowed action surface]")
    );
}
