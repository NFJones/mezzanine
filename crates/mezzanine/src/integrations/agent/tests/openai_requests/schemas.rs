//! Openai Requests tests for schemas behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies available MCP tools keep the unified current action surface.
///
/// A matching MCP tool should be directly callable through the regular action
/// schema and manifest, but it must not remove memory as a usable feature.
/// Provider guidance and runtime guardrails handle placeholder memory behavior
/// without hiding the action.
fn openai_available_mcp_keeps_memory_on_default_surface() {
    let mcp_tool = McpPromptTool {
        server_id: "githubcopilot".to_string(),
        tool_name: "list_ci_results".to_string(),
        description: "Read GitHub CI check results for a repository. User-configured non-authoritative server purpose: GitHub repository and CI operations.".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"repo":{"type":"string","description":"Repository owner/name"}},"required":["repo"]}"#
            .to_string(),
    };
    let context = mez_agent::append_mcp_context(
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "use @githubcopilot to pull the latest CI results".to_string(),
        }])
        .unwrap(),
        &mez_agent::McpPromptSummary {
            available_servers: vec![mez_agent::McpPromptServer {
                server_id: "githubcopilot".to_string(),
                display_name: "GitHub Copilot".to_string(),
                purpose: "GitHub repository and CI operations".to_string(),
                usage_instructions: String::new(),
                tool_count: 1,
                approval_required_tool_count: 0,
            }],
            available_tools: vec![mcp_tool.clone()],
            unavailable_servers: Vec::new(),
        },
    )
    .unwrap();
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
        &context,
    )
    .unwrap();
    mez_agent::apply_default_action_gates(&mut request, &[mcp_tool], true, false);

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mcp_tool_schema = openai_function_tool(&value, "submit_maap_action_batch");
    let description = mcp_tool_schema["description"].as_str().unwrap();
    let action_types = openai_tool_action_types(mcp_tool_schema);

    assert_eq!(value["tool_choice"]["name"], "submit_maap_action_batch");
    assert!(action_types.contains(&"mcp_call".to_string()));
    assert!(action_types.contains(&"memory_search".to_string()));
    assert!(action_types.contains(&"memory_store".to_string()));
    assert!(
        body.contains("explicit_invocation=\\\"githubcopilot\\\""),
        "{body}"
    );
    assert!(body.contains("action=mcp_call"), "{body}");
    assert!(body.contains("required_arguments=\\\"repo\\\""), "{body}");
    assert!(
        !body.contains("memory search and unrelated discovery are not substitutes"),
        "{body}"
    );
    let mcp_actions = openai_tool_action_schemas(mcp_tool_schema)
        .iter()
        .filter(|schema| schema["properties"]["type"]["enum"][0] == "mcp_call")
        .collect::<Vec<_>>();
    assert_eq!(mcp_actions.len(), 1);
    assert_eq!(mcp_actions[0]["properties"]["arguments"]["type"], "string");
    assert!(
        description.contains("The schema includes a generic mcp_call action"),
        "{description}"
    );
    assert!(
        description.contains("runtime validation rejects unavailable tools and invalid arguments"),
        "{description}"
    );
    assert!(
        description.contains("Choose the smallest action that makes concrete progress"),
        "{description}"
    );
    assert!(
        description.contains("direct inspection or execution beats placeholder setup"),
        "{description}"
    );
    assert!(
        description.contains("The function call is only the transport envelope"),
        "{description}"
    );
    assert!(
        description.contains("required-function-call"),
        "{description}"
    );
    assert!(
        description.contains("put that action in this function call now"),
        "{description}"
    );
}

#[test]
/// Verifies OpenAI MAAP tool schemas track the current allowed action surface.
///
/// A single canonical function keeps action selection simple for the model, and
/// its schema carries the request's current allowed actions. The stable prompt
/// text can remain reusable while the provider request shape reflects the live
/// action schema.
fn openai_maap_schema_is_stable_across_non_mcp_action_surfaces() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let capability = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "inspect the repo".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let mut execution = capability.clone();
    execution.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    execution.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Shell);

    let capability_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&capability).unwrap()).unwrap();
    let execution_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&execution).unwrap()).unwrap();
    let capability_diagnostics = openai_prompt_cache_diagnostics_for_request(&capability).unwrap();
    let execution_diagnostics = openai_prompt_cache_diagnostics_for_request(&execution).unwrap();

    assert!(capability_body.get("text").is_none());
    assert!(execution_body.get("text").is_none());
    assert_eq!(capability_body["tools"], execution_body["tools"]);
    assert_eq!(
        capability_body["tool_choice"]["name"],
        "submit_maap_action_batch"
    );
    assert_eq!(
        execution_body["tool_choice"]["name"],
        "submit_maap_action_batch"
    );
    assert_eq!(
        capability_diagnostics.response_format_sha256,
        execution_diagnostics.response_format_sha256
    );
    assert_eq!(
        capability_diagnostics.tools_sha256,
        execution_diagnostics.tools_sha256
    );
    assert_eq!(
        capability_diagnostics.tool_choice_sha256,
        execution_diagnostics.tool_choice_sha256
    );
    assert_eq!(
        capability_diagnostics.stable_input_sha256,
        execution_diagnostics.stable_input_sha256
    );
    assert_eq!(
        capability_diagnostics.stable_projection_sha256,
        execution_diagnostics.stable_projection_sha256
    );
    assert_eq!(
        capability_diagnostics.provider_request_shape_sha256,
        execution_diagnostics.provider_request_shape_sha256
    );
    assert_ne!(
        capability_diagnostics.volatile_input_sha256,
        execution_diagnostics.volatile_input_sha256
    );
}

#[test]
/// Verifies the model-facing memory search schema forbids startup-ritual
/// searches and repeated paraphrase retries.
///
/// This regression keeps provider-visible guidance aligned with the stricter
/// no-memory-by-default policy so models do not treat persistent memory as a
/// normal first step on non-trivial turns.
fn openai_memory_search_schema_disallows_startup_rituals_and_repeat_searches() {
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
            content: "remember durable context".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Memory);

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let memory_tool = openai_function_tool(&value, "submit_maap_action_batch");
    assert_openai_strict_schema_shape(&memory_tool["parameters"]);
    let memory_search_schema = openai_tool_action_schemas(memory_tool)
        .iter()
        .find(|schema| schema["properties"]["type"]["enum"][0] == "memory_search")
        .expect("memory capability should expose memory_search");

    let query_description = memory_search_schema["properties"]["query"]["description"]
        .as_str()
        .unwrap();
    assert!(query_description.contains("Do not use memory_search by default"));
    assert!(query_description.contains("Treat it as optional support"));
    assert!(query_description.contains("generic way to make progress"));
    assert!(query_description.contains("at most one focused search in ordinary turns"));
    assert!(
        query_description.contains("never more than two memory_search actions in one user turn")
    );
    assert!(query_description.contains("facts already present in current action results"));
    assert!(query_description.contains("identifiers, URLs, versions"));
    assert!(query_description.contains("repo owner/name"));
    assert!(query_description.contains("issue/PR numbers"));
    assert!(query_description.contains("CI targets"));
    assert!(query_description.contains("Visible MCP schema and manifest metadata"));
    assert!(query_description.contains("not a reason to search memory first"));
    assert!(query_description.contains("adjust or broaden a direct integration query"));
    assert!(query_description.contains("report a bounded blocker"));
    assert!(query_description.contains("placeholder setup before another direct action"));
    assert!(query_description.contains("If runtime skips or rejects a memory action"));
    assert!(query_description.contains("instead of searching memory again"));
    assert!(query_description.contains("startup ritual"));
    assert!(query_description.contains("paraphrase and search again"));
}

#[test]
/// Verifies the model-facing memory store schema exposes only durable memory
/// kinds and excludes episodic or scratch storage categories.
///
/// This regression keeps the provider-visible schema aligned with the memory
/// policy that ordinary agent turns must not persist transcript summaries,
/// scratch notes, or other current-turn-only operational state.
fn openai_memory_store_schema_excludes_episode_and_scratch_kinds() {
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
            content: "remember durable context".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Memory);

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let memory_tool = openai_function_tool(&value, "submit_maap_action_batch");
    assert_openai_strict_schema_shape(&memory_tool["parameters"]);
    let memory_store_schema = openai_tool_action_schemas(memory_tool)
        .iter()
        .find(|schema| schema["properties"]["type"]["enum"][0] == "memory_store")
        .expect("memory capability should expose memory_store");

    assert_eq!(
        memory_store_schema["properties"]["kind"]["enum"],
        serde_json::json!([
            "preference",
            "fact",
            "procedure",
            "documentation",
            "research",
            "warning"
        ])
    );
    let kind_description = memory_store_schema["properties"]["kind"]["description"]
        .as_str()
        .unwrap();
    assert!(kind_description.contains("tool-output"));
    assert!(kind_description.contains("action-result"));
    assert!(kind_description.contains("current-turn"));
    assert!(kind_description.contains("CI-state"));
    assert!(kind_description.contains("episodic transcript"));
    assert!(kind_description.contains("scratch"));
    assert!(kind_description.contains("almost certain to help future sessions"));
    assert!(kind_description.contains("reusable reference material"));
    assert!(kind_description.contains("research findings"));
    let content_description = memory_store_schema["properties"]["content"]["description"]
        .as_str()
        .unwrap();
    assert!(content_description.contains("reusable beyond the current task"));
    assert!(content_description.contains("not already present in current context"));
    assert!(content_description.contains("not user-provided only for this task"));
    assert!(content_description.contains("almost certain to be useful in future sessions"));
    assert!(content_description.contains("current checkout repo slugs"));
    assert!(content_description.contains("owner/repo"));
    assert!(content_description.contains("CI results"));
}

#[test]
/// Verifies the provider-facing schema describes the patch formats accepted by
/// Mezzanine's shell-backed patch executor.
///
/// The JSON schema is the strongest action-specific hint available to models
/// using native function/tool calls, so it should tell them to emit the single
/// supported Mezzanine patch block format.
fn openai_responses_request_body_describes_apply_patch_format() {
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
            content: "edit a file".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Shell);

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let shell_tool = openai_function_tool(&value, "submit_maap_action_batch");
    assert_openai_strict_schema_shape(&shell_tool["parameters"]);
    let action_schemas = openai_tool_action_schemas(shell_tool);
    let apply_patch_schema = action_schemas
        .iter()
        .find(|schema| schema["properties"]["type"]["enum"][0] == "apply_patch")
        .expect("workspace-write schema should expose apply_patch");
    assert!(
        action_schemas
            .iter()
            .all(|schema| schema["properties"]["type"]["enum"][0] != "edit_file")
    );
    assert!(
        action_schemas
            .iter()
            .all(|schema| schema["properties"]["type"]["enum"][0] != "write_file")
    );
    let description = apply_patch_schema["properties"]["patch"]["description"]
        .as_str()
        .unwrap();

    assert!(description.contains("Mezzanine"), "{description}");
    assert!(
        description.contains("only semantic file-content mutation action"),
        "{description}"
    );
    assert!(
        description.contains("Direct Mezzanine patch text"),
        "{description}"
    );
    assert!(description.contains("*** Begin Patch"), "{description}");
    assert!(description.contains("*** End Patch"), "{description}");
    assert!(
        description.contains(
            "Accepted file directives are exactly *** Add File, *** Update File, *** Delete File"
        ),
        "{description}"
    );
    assert!(
        description.contains("there is no *** Replace File directive"),
        "{description}"
    );
    assert!(
        description.contains(
            "For whole-file replacement, use an Update File hunk headed @@ replace whole file"
        ),
        "{description}"
    );
    assert!(description.contains("relative safe paths"), "{description}");
    assert!(
        description.contains("paths must not be absolute"),
        "{description}"
    );
    assert!(description.contains(".. traversal"), "{description}");
    assert!(
        description.contains("distinctive @@ header"),
        "{description}"
    );
    assert!(
        description.contains("1-6 exact current old/context lines"),
        "{description}"
    );
    assert!(
        description.contains("copied verbatim from current file content"),
        "{description}"
    );
    assert!(
        description.contains("never infer or reconstruct likely code as old context"),
        "{description}"
    );
    assert!(
        description.contains("Usually one bounded owner read"),
        "{description}"
    );
    assert!(
        description.contains("multiple small hunks"),
        "{description}"
    );
    assert!(description.contains("space context"), "{description}");
    assert!(description.contains("- removed"), "{description}");
    assert!(description.contains("+ added"), "{description}");
    assert!(description.contains("*** End of File"), "{description}");
    assert!(
        description.contains("After mismatch or ambiguity"),
        "{description}"
    );
    assert!(
        description.contains("reread only missing/stale owner ranges"),
        "{description}"
    );
    assert!(
        description.contains("skip already-applied changes"),
        "{description}"
    );
    assert!(
        description.contains("smaller fresh anchored patch"),
        "{description}"
    );
    assert!(
        description.len() < 1350,
        "apply_patch schema guidance should stay compact: {description}"
    );
}

#[test]
/// Verifies the provider-facing config-change schema exposes live config
/// mutation guidance instead of leaving the model to guess free-form paths.
///
/// This matters because `config_change` applies privileged runtime settings,
/// so the model needs path patterns, value encoding, and operation constraints
/// before it can propose a valid mutation.
fn openai_responses_request_body_describes_config_change_schema() {
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
            content: "change the active theme".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::ConfigChange)
            .with_config_change_setting_path_description(
                crate::config::config_change_setting_path_description(),
            );

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let config_tool = openai_function_tool(&value, "submit_maap_action_batch");
    assert_openai_strict_schema_shape(&config_tool["parameters"]);
    let action_schemas = openai_tool_action_schemas(config_tool);
    let config_schema = action_schemas
        .iter()
        .find(|schema| schema["properties"]["type"]["enum"][0] == "config_change")
        .expect("config-change capability should expose config_change");

    assert_eq!(
        config_schema["properties"]["operation"]["enum"],
        serde_json::json!(["set", "unset", "reset"])
    );
    let path_description = config_schema["properties"]["setting_path"]["description"]
        .as_str()
        .unwrap();
    assert_eq!(
        path_description,
        "Dotted live configuration path. Use only paths advertised by the product adapter, and inspect current configuration before changing dynamic names."
    );

    let value_description = config_schema["properties"]["value"]["description"]
        .as_str()
        .unwrap();
    assert!(
        value_description.contains("JSON string"),
        "{value_description}"
    );
    assert!(
        value_description.contains("string array"),
        "{value_description}"
    );
    assert!(
        value_description.contains("reset removes the explicit override"),
        "{value_description}"
    );
    assert!(
        value_description.contains("use null"),
        "{value_description}"
    );
    let operation_description = config_schema["properties"]["operation"]["description"]
        .as_str()
        .unwrap();
    assert!(
        operation_description.contains("changing the mez theme"),
        "{operation_description}"
    );
    assert!(
        operation_description.contains("not prose or config-file edits"),
        "{operation_description}"
    );
    assert!(
        operation_description.contains("follow the active approval policy"),
        "{operation_description}"
    );
}

#[test]
/// Verifies openai responses request body exposes the current executable
/// action schema through one canonical tool.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn openai_responses_request_body_exposes_granted_execution_actions_and_capability_routing() {
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
            content: "Create random test data".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Shell);

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let shell_tool = openai_function_tool(&value, "submit_maap_action_batch");

    assert!(value.get("text").is_none());
    assert_openai_strict_schema_shape(&shell_tool["parameters"]);
    assert_eq!(shell_tool["type"], "function");
    assert_eq!(shell_tool["strict"], true);
    assert_eq!(value["tool_choice"]["name"], "submit_maap_action_batch");
    assert_eq!(
        shell_tool["parameters"]["required"],
        serde_json::json!(["rationale", "thought", "actions"])
    );
    let shell_description = shell_tool["description"].as_str().unwrap();
    assert!(shell_description.contains("Return a function call, not prose"));
    assert!(shell_description.contains("Use only the action objects in this function schema"));
    assert!(shell_description.contains("emit request_capability for that capability"));
    assert!(shell_description.contains("Wrong: *** Replace File"));
    assert!(shell_description.contains("copy old/context lines verbatim"));
    assert_eq!(
        shell_tool["parameters"]["properties"]["rationale"]["minLength"],
        1
    );

    let action_schemas = openai_tool_action_schemas(shell_tool);
    let action_types = openai_tool_action_types(shell_tool);
    assert!(action_types.contains(&"say".to_string()));
    assert!(action_types.contains(&"shell_command".to_string()));
    assert!(action_types.contains(&"apply_patch".to_string()));
    assert!(!action_types.contains(&"request_skills".to_string()));
    assert!(!action_types.contains(&"call_skill".to_string()));
    let removed_user_input_action = ["request", "user_input"].join("_");
    assert!(!action_types.contains(&removed_user_input_action));
    assert!(action_types.contains(&"request_capability".to_string()));
    assert!(!action_types.contains(&"abort".to_string()));
    assert!(action_types.contains(&"fetch_url".to_string()));
    assert!(action_types.contains(&"web_search".to_string()));
    let request_state = value["input"].as_array().unwrap().last().unwrap()["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(request_state.contains("[OpenAI request state]"));
    assert!(request_state.contains("interaction_kind=action_execution"));
    assert!(
        request_state.contains("allowed_actions=say,request_capability,shell_command,apply_patch")
    );
    assert!(!request_state.contains("Emit only"));

    let shell_schema = action_schemas
        .iter()
        .find(|schema| schema["properties"]["type"]["enum"][0] == "shell_command")
        .unwrap();
    let shell_required = shell_schema["required"].as_array().unwrap();
    assert!(shell_required.iter().any(|field| field == "summary"));
    assert!(shell_required.iter().any(|field| field == "command"));
    assert!(!shell_required.iter().any(|field| field == "interactive"));
    assert!(!shell_required.iter().any(|field| field == "stateful"));
    assert!(!shell_required.iter().any(|field| field == "timeout_ms"));
    let shell_description = shell_schema["properties"]["command"]["description"]
        .as_str()
        .unwrap();
    assert!(
        shell_description.contains("Exact bounded, noninteractive pane shell input"),
        "{shell_description}"
    );
    assert!(
        shell_description.contains("one logical inspection"),
        "{shell_description}"
    );
    assert!(
        shell_description.contains("Prefer one focused command"),
        "{shell_description}"
    );
    assert!(
        shell_description.contains("separate shell_command actions for independent work"),
        "{shell_description}"
    );
    assert!(
        shell_description.contains("Do not run apply_patch as a shell program"),
        "{shell_description}"
    );
    assert!(
        shell_description.contains("Heredocs and here-strings are disabled"),
        "{shell_description}"
    );
}

#[test]
/// Verifies auto-sizing requests use a separate structured-output schema and
/// never expose normal action tools. The router response is an internal
/// decision object rather than a MAAP action batch.
fn openai_responses_request_body_uses_auto_sizing_schema_for_router() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-router".to_string(),
            reasoning_profile: Some("low".to_string()),
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
            content: "classify this task".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::AutoSizing;
    request.allowed_actions = mez_agent::AllowedActionSet::say_only();
    request.reasoning_effort = Some("low".to_string());

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(
        value["text"]["format"]["name"],
        "mezzanine_auto_sizing_decision"
    );
    assert_eq!(value["text"]["format"]["strict"], true);
    assert_eq!(value["tool_choice"], "none");
    assert!(value.get("tools").is_none());
    assert_eq!(value["reasoning"]["effort"], "low");
    assert_eq!(
        value["text"]["format"]["schema"]["properties"]["size"]["enum"],
        serde_json::json!(["small", "medium", "large"])
    );
    assert_eq!(
        value["text"]["format"]["schema"]["required"],
        serde_json::json!([
            "version",
            "size",
            "reasoning_effort",
            "confidence",
            "rationale"
        ])
    );
}

#[test]
/// Verifies uncommon composite capability grants still get provider-enforced
/// current-schema narrowing instead of falling back to an all-action MAAP
/// schema.
///
/// Multiple request_capability actions can be granted in one continuation. The
/// canonical function for this request must expose exactly the composite
/// surface.
fn openai_responses_request_body_uses_current_schema_for_composite_action_surface() {
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
            content: "inspect locally and fetch a URL".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    let mut allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Shell);
    allowed_actions.extend_set(&mez_agent::AllowedActionSet::for_capability(
        mez_agent::AgentCapability::NetworkFetch,
    ));
    request.allowed_actions = allowed_actions;

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let current_tool = openai_function_tool(&value, "submit_maap_action_batch");
    let action_types = openai_tool_action_types(current_tool);

    assert_eq!(value["tool_choice"]["name"], "submit_maap_action_batch");
    assert_eq!(value["tools"].as_array().unwrap().len(), 1);
    assert!(action_types.contains(&"say".to_string()));
    assert!(action_types.contains(&"request_capability".to_string()));
    assert!(action_types.contains(&"shell_command".to_string()));
    assert!(action_types.contains(&"apply_patch".to_string()));
    assert!(action_types.contains(&"fetch_url".to_string()));
    assert!(action_types.contains(&"web_search".to_string()));
    assert!(action_types.contains(&"mcp_call".to_string()));
    assert!(action_types.contains(&"spawn_agent".to_string()));
}

#[test]
/// Verifies openai responses request body uses mcp tool argument schemas.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn openai_responses_request_body_uses_mcp_tool_argument_schemas() {
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
            content: "read a file".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Mcp);
    request.available_mcp_tools = vec![
        McpPromptTool {
            server_id: "zeta".to_string(),
            tool_name: "later".to_string(),
            description: "Later tool".to_string(),
            approval_required: false,
            input_schema_json: r#"{"type":"object","properties":{"value":{"type":"string"}}}"#
                .to_string(),
        },
        McpPromptTool {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            description: "Read file".to_string(),
            approval_required: false,
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
                .to_string(),
        },
    ];

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mcp_tool = openai_function_tool(&value, "submit_maap_action_batch");
    let description = mcp_tool["description"].as_str().unwrap();
    assert!(value.get("text").is_none());
    assert_openai_strict_schema_shape(&mcp_tool["parameters"]);
    assert_eq!(value["tool_choice"]["name"], "submit_maap_action_batch");
    assert!(
        description.contains("The schema includes a generic mcp_call action"),
        "{description}"
    );
    assert!(
        description.contains("runtime validation rejects unavailable tools and invalid arguments"),
        "{description}"
    );
    let action_schemas = openai_tool_action_schemas(mcp_tool);
    let mcp_schemas = action_schemas
        .iter()
        .filter(|schema| schema["properties"]["type"]["enum"][0] == "mcp_call")
        .collect::<Vec<_>>();

    assert_eq!(action_schemas.len(), 16);
    let action_types = openai_tool_action_types(mcp_tool);
    assert!(!action_types.contains(&"request_skills".to_string()));
    assert!(!action_types.contains(&"call_skill".to_string()));
    assert_eq!(mcp_schemas.len(), 1);
    assert_eq!(mcp_schemas[0]["properties"]["server"]["type"], "string");
    assert_eq!(mcp_schemas[0]["properties"]["tool"]["type"], "string");
    assert_eq!(mcp_schemas[0]["properties"]["arguments"]["type"], "string");
    assert!(mcp_schemas[0]["properties"]["server"].get("enum").is_none());
    assert!(mcp_schemas[0]["properties"]["tool"].get("enum").is_none());
}
