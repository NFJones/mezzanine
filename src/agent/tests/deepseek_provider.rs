//! Agent tests for deepseek provider behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies an explicit DeepSeek thinking disable overrides configured
/// reasoning effort before request serialization.
///
/// DeepSeek rejects forced `tool_choice` while thinking is enabled, but the
/// user-facing `/thinking off` command must let an operator prioritize strict
/// MAAP tool-call reliability without deleting the profile's reasoning level.
/// This regression keeps those controls independent: reasoning remains on the
/// profile, while the provider request disables thinking and omits
/// `reasoning_effort`.
fn deepseek_chat_completions_request_body_disables_thinking_when_profile_toggle_is_off() {
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("thinking".to_string(), "disabled".to_string());
    provider_options.insert("reasoning_effort".to_string(), "xhigh".to_string());
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_profile: Some("xhigh".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options,
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "spawn two subagents".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Subagent);

    let http_request = build_deepseek_chat_completions_http_request(
        &request,
        "deepseek-key",
        "https://api.deepseek.com/chat/completions",
        false,
        1000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&http_request.body).unwrap();

    assert_eq!(request.thinking_enabled, Some(false));
    assert_eq!(
        value["thinking"],
        serde_json::json!({
            "type": "disabled"
        })
    );
    assert!(value.get("reasoning_effort").is_none());
    assert_eq!(
        value["tool_choice"],
        serde_json::json!({
            "type": "function",
            "function": {
                "name": DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME
            }
        })
    );
}

#[test]
/// Verifies DeepSeek selected-model requests with default concrete actions use
/// the action-dispatch shim even when the interaction kind is still the
/// initial capability-decision phase.
///
/// Runtime widens the selected model's first request with default `mcp_call`
/// and memory actions after assembly. DeepSeek must serialize that concrete
/// surface instead of choosing the narrow capability selector from
/// `interaction_kind` alone, or the model cannot directly call available MCP
/// tools and may drift into no-op memory actions.
fn deepseek_chat_completions_request_body_dispatches_default_mcp_actions_on_initial_surface() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_profile: Some("xhigh".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "use the GitLab MCP server to inspect an issue".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.allowed_actions.extend([
        crate::agent::AllowedAction::McpCall,
        crate::agent::AllowedAction::MemorySearch,
        crate::agent::AllowedAction::MemoryStore,
    ]);
    request.available_mcp_tools = vec![mez_agent::McpPromptTool {
        server_id: "gitlab".to_string(),
        tool_name: "get_issue".to_string(),
        description: "Read one GitLab issue".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"iid":{"type":"integer"}}}"#
            .to_string(),
    }];

    let http_request = build_deepseek_chat_completions_http_request(
        &request,
        "deepseek-key",
        "https://api.deepseek.com/chat/completions",
        false,
        1000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&http_request.body).unwrap();
    let tool = deepseek_maap_function_tool(&value);
    let action_types = deepseek_tool_action_types(tool);
    let description = tool["function"]["description"].as_str().unwrap();

    assert_eq!(
        value["tool_choice"]["function"]["name"],
        DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME
    );
    assert_eq!(
        tool["function"]["name"],
        DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME
    );
    assert!(action_types.contains(&"mcp_call".to_string()));
    assert!(action_types.contains(&"memory_search".to_string()));
    assert!(action_types.contains(&"memory_store".to_string()));
    assert!(action_types.contains(&"request_capability".to_string()));
    assert!(
        description.contains("If this schema includes mcp_call"),
        "{description}"
    );
    assert!(
        description.contains("Do not use memory_search to decide whether visible MCP metadata"),
        "{description}"
    );
    assert!(
        description.contains("the task matches visible MCP metadata"),
        "{description}"
    );
    assert!(
        !description.contains("routing_match=available_mcp"),
        "{description}"
    );
    assert!(
        description.contains("merely to set up a useful MCP call"),
        "{description}"
    );
    assert!(
        description.contains("current action results"),
        "{description}"
    );
    assert!(
        description.contains("adjust or broaden a direct integration query"),
        "{description}"
    );
    assert!(
        description.contains("report a bounded blocker"),
        "{description}"
    );
    assert!(
        description.contains("never more than two in one user turn"),
        "{description}"
    );
    assert!(
        description.contains("safely gathered context"),
        "{description}"
    );
    assert!(description.contains("request it"), "{description}");
    assert!(
        !description.contains("routing_match=available_mcp"),
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
        description.contains("do not emit a say-only or progress batch claiming"),
        "{description}"
    );
    assert!(
        description.contains(
            "Available MCP tools callable with mcp_call: gitlab/get_issue: Read one GitLab issue."
        ),
        "{description}"
    );
    assert!(
        !description.contains("Decide the next Mezzanine capability"),
        "{description}"
    );
    assert!(
        tool["function"]["parameters"]["properties"]
            .get("actions")
            .is_some()
    );
}

#[test]
/// Verifies an explicit DeepSeek thinking enable can activate provider
/// thinking mode without requiring a separate reasoning-effort value.
///
/// Operators may want to leave DeepSeek's effort choice at the provider
/// default while still enabling native thinking. This request shape should
/// advertise the MAAP tool in model-selected mode, omit forced `tool_choice`,
/// and avoid inventing a `reasoning_effort` field the profile did not carry.
fn deepseek_chat_completions_request_body_enables_thinking_without_reasoning_effort() {
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("thinking".to_string(), "enabled".to_string());
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
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
            content: "spawn two subagents".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Subagent);

    let http_request = build_deepseek_chat_completions_http_request(
        &request,
        "deepseek-key",
        "https://api.deepseek.com/chat/completions",
        false,
        1000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&http_request.body).unwrap();

    assert_eq!(request.thinking_enabled, Some(true));
    assert_eq!(
        value["thinking"],
        serde_json::json!({
            "type": "enabled"
        })
    );
    assert!(value.get("reasoning_effort").is_none());
    assert!(value.get("tool_choice").is_none());
    assert!(value.get("tools").is_some());
}

#[test]
/// Verifies DeepSeek capability-decision requests disable thinking before
/// forcing the MAAP tool call instead of allowing an ordinary prose response.
///
/// The DeepSeek Chat Completions API defaults to `tool_choice=auto` whenever a
/// tool list is present. Mezzanine's first provider turn still requires a
/// structured MAAP batch so the model can request the missing coarse capability
/// rather than narrating that it might try an action name. DeepSeek rejects
/// forced `tool_choice` in thinking mode, so this regression protects both the
/// explicit non-thinking toggle and the narrow say/request-capability schema
/// used by the initial turn.
fn deepseek_chat_completions_request_body_forces_maap_tool_without_thinking_for_capability_decision()
 {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_profile: Some("xhigh".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "spawn two subagents".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    let http_request = build_deepseek_chat_completions_http_request(
        &request,
        "deepseek-key",
        "https://api.deepseek.com/chat/completions",
        false,
        1000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&http_request.body).unwrap();
    let tool = deepseek_maap_function_tool(&value);

    assert_eq!(
        value["thinking"],
        serde_json::json!({
            "type": "disabled"
        })
    );
    assert!(value.get("reasoning_effort").is_none());
    assert_eq!(
        value["tool_choice"],
        serde_json::json!({
            "type": "function",
            "function": {
                "name": DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME
            }
        })
    );
    assert_eq!(value["tools"].as_array().unwrap().len(), 1);
    let description = tool["function"]["description"].as_str().unwrap();
    assert!(description.contains("Decide the next Mezzanine capability"));
    assert!(description.contains("Return a function call, not prose"));
    assert!(description.contains("Capability map: shell=local files"));
    assert!(description.contains("network_search=web_search"));
    assert!(description.contains("network_fetch=fetch_url"));
    assert!(description.contains("mcp=mcp_call"));
    assert!(description.contains("memory=memory_search or memory_store"));
    assert!(description.contains("issues=issue_add, issue_update, issue_query, or issue_delete"));
    assert!(description.contains("a missing shell, network_search, network_fetch, mcp, subagent, config_change, memory, issues, or respond_only action surface is not a blocker"));
    assert!(!description.contains("missing shell, patch, web, MCP, messaging"));
    assert!(description.contains("Wrong: say(blocked"));
    assert!(description.contains("Right: request_capability(capability=\"shell\""));
    assert!(description.contains("Wrong: *** Replace File"));
    assert!(description.contains("Right: *** Update File with anchored hunks"));
    assert!(description.contains("Wrong: inferred apply_patch old context"));
    assert!(description.contains("copy old/context lines verbatim from read file evidence"));
    let parameters = &tool["function"]["parameters"];
    assert!(parameters["properties"].get("capability").is_some());
    assert!(parameters["properties"].get("reason").is_some());
    assert!(parameters["properties"].get("actions").is_none());
    let parameters_text = serde_json::to_string(parameters).unwrap();
    assert!(!parameters_text.contains("minLength"));
    assert!(!parameters_text.contains("minItems"));
}

#[test]
/// Verifies DeepSeek subagent execution requests disable thinking before
/// forcing the MAAP tool and exposing the concrete subagent action variants.
///
/// After the controller grants subagent capability, the provider-visible schema
/// must make `spawn_agent` and `send_message` explicit while still forcing the
/// single MAAP function call. Without a forced named tool, DeepSeek can legally
/// return normal assistant text even though Mezzanine needs executable local
/// actions for the turn to progress. The request must remain in non-thinking
/// mode because DeepSeek rejects forced `tool_choice` while thinking is enabled.
fn deepseek_chat_completions_request_body_forces_maap_tool_without_thinking_for_subagent_actions() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
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
            content: "spawn two subagents".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Subagent);

    let http_request = build_deepseek_chat_completions_http_request(
        &request,
        "deepseek-key",
        "https://api.deepseek.com/chat/completions",
        false,
        1000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&http_request.body).unwrap();
    let tool = deepseek_maap_function_tool(&value);
    let action_types = deepseek_tool_action_types(tool);
    let description = tool["function"]["description"].as_str().unwrap();

    assert_eq!(
        value["thinking"],
        serde_json::json!({
            "type": "disabled"
        })
    );
    assert!(value.get("reasoning_effort").is_none());
    assert_eq!(
        value["tool_choice"],
        serde_json::json!({
            "type": "function",
            "function": {
                "name": DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME
            }
        })
    );
    assert!(action_types.contains(&"say".to_string()));
    assert!(action_types.contains(&"request_capability".to_string()));
    assert!(action_types.contains(&"send_message".to_string()));
    assert!(action_types.contains(&"spawn_agent".to_string()));
    assert!(
        description.contains(
            "Current allowed action types: say,request_capability,send_message,spawn_agent"
        )
    );
    assert!(
        description.contains("request_capability for that capability instead of say(blocked)"),
        "{description}"
    );
    assert!(
        description.contains("Capability map: shell=local files"),
        "{description}"
    );
    assert!(description.contains("Wrong: say(blocked"), "{description}");
}

#[test]
/// Verifies DeepSeek no-tool requests can use thinking mode without sending a
/// redundant `tool_choice: none` field. This matters because DeepSeek's
/// thinking mode rejects some `tool_choice` values even when Mezzanine has no
/// function tool to force for the request.
fn deepseek_chat_completions_request_body_omits_tool_choice_for_no_tool_thinking_requests() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_profile: Some("xhigh".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "classify this prompt size".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::AutoSizing;
    request.allowed_actions = crate::agent::AllowedActionSet::from_actions([]);

    let http_request = build_deepseek_chat_completions_http_request(
        &request,
        "deepseek-key",
        "https://api.deepseek.com/chat/completions",
        false,
        1000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&http_request.body).unwrap();

    assert_eq!(
        value["thinking"],
        serde_json::json!({
            "type": "enabled"
        })
    );
    assert_eq!(value["reasoning_effort"], "max");
    assert_eq!(value["response_format"]["type"], "json_object");
    assert!(value.get("tool_choice").is_none());
    assert!(value.get("tools").is_none());
}

#[test]
/// Verifies DeepSeek MAAP requests use the provider's thinking-mode tool-call
/// pattern when reasoning is configured.
///
/// DeepSeek supports tool calls in thinking mode only through model-selected
/// tool use. Mezzanine therefore advertises the MAAP function without forcing
/// `tool_choice` when a DeepSeek reasoning effort is present, preserving
/// DeepSeek reasoning without changing OpenAI's stricter forced-tool path.
fn deepseek_chat_completions_request_body_uses_auto_maap_tool_with_thinking_when_reasoning_enabled()
{
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_profile: Some("xhigh".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "spawn two subagents".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Subagent);

    let http_request = build_deepseek_chat_completions_http_request(
        &request,
        "deepseek-key",
        "https://api.deepseek.com/chat/completions",
        false,
        1000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&http_request.body).unwrap();
    let tool = deepseek_maap_function_tool(&value);
    let action_types = deepseek_tool_action_types(tool);

    assert_eq!(
        value["thinking"],
        serde_json::json!({
            "type": "enabled"
        })
    );
    assert_eq!(value["reasoning_effort"], "max");
    assert!(value.get("tool_choice").is_none());
    assert_eq!(value["tools"].as_array().unwrap().len(), 1);
    assert!(action_types.contains(&"send_message".to_string()));
    assert!(action_types.contains(&"spawn_agent".to_string()));
}

#[test]
/// Verifies named OpenAI-compatible DeepSeek providers keep their configured
/// runtime identity when sending a Chat Completions request.
///
/// The openai-compatible dispatch path reuses the DeepSeek Chat Completions
/// adapter, so the adapter must accept the configured provider name instead of
/// rejecting the request as if every compatible endpoint were the built-in
/// `deepseek` provider. This locks the regression that surfaced as
/// `DeepSeek provider received a request for a different provider` before any
/// HTTP request was sent.
fn deepseek_provider_accepts_openai_compatible_provider_identity() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek_compatible".to_string(),
            model: "deepseek-v4-pro".to_string(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let arguments = serde_json::json!({
        "rationale": "compatible provider returned structured output",
        "status": "final",
        "text": "hello"
    })
    .to_string();
    let transport = SequencedFakeProviderHttpTransport::new(vec![ProviderHttpResponse {
        status_code: 200,
        headers: Default::default(),
        body: serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [
                            {
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME,
                                    "arguments": arguments
                                }
                            }
                        ]
                    }
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4
            }
        })
        .to_string(),
    }]);
    let provider = crate::agent::DeepSeekChatCompletionsProvider::new("compatible-key", transport)
        .unwrap()
        .with_provider_id("deepseek_compatible")
        .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(provider.provider_id(), "deepseek_compatible");
    assert_eq!(provider.transport.requests.borrow().len(), 1);
    let request_body: serde_json::Value =
        serde_json::from_str(&provider.transport.requests.borrow()[0].body).unwrap();
    let description = request_body["tools"][0]["function"]["description"]
        .as_str()
        .unwrap();
    assert!(description.contains("Return a function call, not prose"));
    assert!(description.contains("Capability map: shell=local files"));
    assert!(description.contains("Wrong: say(blocked"));
    assert!(description.contains("Right: request_capability(capability=\"shell\""));
    let batch = response.action_batch.unwrap();
    assert_eq!(
        batch.rationale,
        "compatible provider returned structured output"
    );
    assert!(batch.final_turn);
}

#[test]
/// Verifies DeepSeek does not return a successful provider response when the
/// strict fallback still omits the required MAAP batch.
///
/// The runtime can repair provider malformed-output errors, but it cannot
/// repair a response that was accepted as successful with `action_batch=None`.
/// This locks the adapter to converting missing DeepSeek MAAP output into a
/// provider diagnostic that preserves the bad response text.
fn deepseek_provider_rejects_missing_maap_after_strict_retry() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::RespondOnly);
    let transport = SequencedFakeProviderHttpTransport::new(vec![
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "deepseek-v4-pro",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "reasoning_content": "I should answer somehow.",
                            "content": "I can help with that."
                        }
                    }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 4,
                    "reasoning_tokens": 3
                }
            })
            .to_string(),
        },
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "deepseek-v4-pro",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "Still no tool call."
                        }
                    }
                ],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 6,
                    "reasoning_tokens": 0
                }
            })
            .to_string(),
        },
    ]);
    let provider =
        crate::agent::DeepSeekChatCompletionsProvider::new("deepseek-key", transport).unwrap();

    let error = provider.send_request(&request).unwrap_err();

    let requests = provider.transport.requests.borrow();
    assert_eq!(requests.len(), 2);
    let second_body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(second_body["thinking"]["type"], "disabled");
    assert_eq!(
        second_body["tool_choice"]["function"]["name"],
        DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME
    );
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert_eq!(error.provider_raw_text(), Some("Still no tool call."));
    assert!(
        error
            .message()
            .contains("DeepSeek response did not call a Mezzanine DeepSeek shim tool"),
        "{}",
        error.message()
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["type"], "malformed_model_output");
}

#[test]
/// Verifies DeepSeek thinking MAAP requests fall back to strict non-thinking
/// MAAP when the provider returns prose instead of a tool call.
///
/// The first request follows DeepSeek's thinking-mode pattern by advertising
/// tools without forced `tool_choice`. If the model declines to call the MAAP
/// function, the adapter retries once with thinking disabled and a forced MAAP
/// function so the runtime still receives a structured action batch.
fn deepseek_provider_retries_strict_maap_when_thinking_auto_tool_returns_prose() {
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::RespondOnly);
    let arguments = serde_json::json!({
        "rationale": "fallback produced structured output",
        "status": "final",
        "text": "hello"
    })
    .to_string();
    let transport = SequencedFakeProviderHttpTransport::new(vec![
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "deepseek-v4-pro",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "reasoning_content": "I should answer somehow.",
                            "content": "I can help with that."
                        }
                    }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 4,
                    "reasoning_tokens": 3
                }
            })
            .to_string(),
        },
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "deepseek-v4-pro",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "",
                            "reasoning_content": "Now I will call the function.",
                            "tool_calls": [
                                {
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME,
                                        "arguments": arguments
                                    }
                                }
                            ]
                        }
                    }
                ],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 6,
                    "reasoning_tokens": 0
                }
            })
            .to_string(),
        },
    ]);
    let provider =
        crate::agent::DeepSeekChatCompletionsProvider::new("deepseek-key", transport).unwrap();

    let response = provider.send_request(&request).unwrap();

    let requests = provider.transport.requests.borrow();
    assert_eq!(requests.len(), 2);
    let first_body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    let second_body: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(first_body["thinking"]["type"], "enabled");
    assert!(first_body.get("tool_choice").is_none());
    assert_eq!(second_body["thinking"]["type"], "disabled");
    assert_eq!(
        second_body["tool_choice"]["function"]["name"],
        DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME
    );
    assert_eq!(response.usage.input_tokens, 22);
    assert_eq!(response.usage.output_tokens, 10);
    assert_eq!(response.usage.reasoning_tokens, 3);
    assert_eq!(
        response.latest_request_usage,
        Some(crate::agent::ModelTokenUsage {
            input_tokens: 12,
            output_tokens: 6,
            reasoning_tokens: 0,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        })
    );
    let batch = response.action_batch.unwrap();
    assert_eq!(batch.rationale, "fallback produced structured output");
    assert!(batch.final_turn);
}
