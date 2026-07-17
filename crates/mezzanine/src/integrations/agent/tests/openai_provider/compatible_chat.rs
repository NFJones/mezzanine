//! Openai Provider tests for compatible chat behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies Chat Completions-compatible providers can list models without
/// stored auth metadata.
///
/// OpenAI-compatible and DeepSeek-compatible local backends share the same
/// optional-auth contract: no configured credential means no bearer header,
/// not an early authentication failure.
fn chat_completions_compatible_providers_omit_auth_when_metadata_is_absent() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-no-auth-chat-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let openai_transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"data":[{"id":"local-chat"}]}"#.to_string(),
        },
    };
    let deepseek_transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"data":[{"id":"local-deepseek"}]}"#.to_string(),
        },
    };

    let openai_provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &std::collections::BTreeMap::new(),
        120_000,
        openai_transport,
    )
    .unwrap();
    let deepseek_provider =
        deepseek_chat_completions_provider_from_auth_store_with_provider_options(
            &auth_store,
            "local-deepseek-chat",
            Some("http://localhost:4321/v1"),
            120_000,
            deepseek_transport,
        )
        .unwrap();

    let openai_catalog = openai_provider.list_models().unwrap();
    let deepseek_catalog = deepseek_provider.list_models().unwrap();

    assert_eq!(openai_catalog.models[0].id, "local-chat");
    assert_eq!(deepseek_catalog.models[0].id, "local-deepseek");
    let openai_sent = openai_provider.transport.requests.borrow();
    let deepseek_sent = deepseek_provider.transport.requests.borrow();
    assert_eq!(openai_sent[0].url, "http://localhost:1234/v1/models");
    assert_eq!(deepseek_sent[0].url, "http://localhost:4321/v1/models");
    assert_eq!(openai_sent[0].headers.get("Authorization"), None);
    assert_eq!(deepseek_sent[0].headers.get("Authorization"), None);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies duplicate OpenAI-compatible MAAP tool calls surface as malformed output.
///
/// Duplicate MAAP function calls are a provider-output packaging error, not a
/// plain runtime invalid-state failure. The error must retain the raw tool-call
/// payload so the existing malformed-MAAP repair flow can ask the model for one
/// corrected action batch.
fn openai_compatible_chat_completions_duplicate_maap_tool_calls_are_malformed_output() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-duplicate-tools-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::RespondOnly);
    let arguments = serde_json::json!({
        "rationale": "duplicate tool call payload",
        "thought": null,
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": "text/plain; charset=utf-8",
                "text": "hello"
            }
        ]
    })
    .to_string();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "local-chat-model",
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
                                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                                        "arguments": arguments
                                    }
                                },
                                {
                                    "id": "call_2",
                                    "type": "function",
                                    "function": {
                                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                                        "arguments": arguments
                                    }
                                }
                            ]
                        }
                    }
                ]
            })
            .to_string(),
        },
    };
    let provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &std::collections::BTreeMap::new(),
        120_000,
        transport,
    )
    .unwrap();

    let error = provider.send_request(&request).unwrap_err();

    assert!(
        error
            .message()
            .contains("provider MAAP output is malformed"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("multiple MAAP tool calls"),
        "{}",
        error.message()
    );
    let raw_text = error.provider_raw_text().expect("raw provider tool calls");
    assert!(raw_text.contains("call_1"), "{raw_text}");
    assert!(raw_text.contains("call_2"), "{raw_text}");
    assert!(
        raw_text.contains(OPENAI_MAAP_FUNCTION_TOOL_NAME),
        "{raw_text}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies OpenAI-compatible length finish reasons use output-limit recovery.
///
/// A Chat Completions backend can stop in the middle of MAAP JSON when the
/// response exhausts its output-token budget. This must surface as an
/// output-limit error so the runtime can use the dedicated max-output-token
/// recovery path instead of treating the partial MAAP payload as ordinary
/// malformed provider output.
fn openai_compatible_chat_completions_length_finish_reason_is_output_limit_error() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-length-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::RespondOnly);
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "local-chat-model",
                "choices": [
                    {
                        "finish_reason": "length",
                        "message": {
                            "role": "assistant",
                            "content": "{"
                        }
                    }
                ]
            })
            .to_string(),
        },
    };
    let provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &std::collections::BTreeMap::new(),
        120_000,
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
        crate::integrations::agent::provider::provider_error_retry_class(&error),
        mez_agent::ProviderErrorRetryClass::OutputLimit
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["finish_reason"], "length");
    assert_eq!(
        failure_json["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(error.provider_raw_text(), Some("{"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies generic OpenAI-compatible Chat Completions tool descriptions list
/// the callable MCP tools in plain language.
///
/// Some local OpenAI-compatible models choose actions from the function
/// description before inspecting nested JSON Schema variants. The selected
/// tool wrapper therefore needs to name MCP server/tool routes directly instead
/// of relying only on the `mcp_call` schema branch.
fn openai_compatible_chat_completions_provider_describes_callable_mcp_tools() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-mcp-description-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
            reasoning_profile: Some("high".to_string()),
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
            content: "use GitLab issue operations".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions.extend([
        mez_agent::AllowedAction::McpCall,
        mez_agent::AllowedAction::MemorySearch,
        mez_agent::AllowedAction::MemoryStore,
    ]);
    request.available_mcp_tools = vec![mez_agent::McpPromptTool {
        server_id: "gitlab".to_string(),
        tool_name: "get_issue".to_string(),
        description: "Read one GitLab issue".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"iid":{"type":"integer"}}}"#
            .to_string(),
    }];
    let arguments = serde_json::json!({
        "rationale": "generic compatible provider called MCP",
        "thought": null,
        "actions": [
            {
                "type": "mcp_call",
                "server": "gitlab",
                "tool": "get_issue",
                "arguments": {"iid": 7}
            }
        ]
    })
    .to_string();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "local-chat-model",
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
                                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                                        "arguments": arguments
                                    }
                                }
                            ]
                        }
                    }
                ]
            })
            .to_string(),
        },
    };

    let provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &std::collections::BTreeMap::new(),
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(
        response.action_batch.unwrap().rationale,
        "generic compatible provider called MCP"
    );
    let sent = provider.transport.requests.borrow();
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    let description = body["tools"][0]["function"]["description"]
        .as_str()
        .unwrap();
    assert!(
        description.contains(
            "Available MCP tools callable with mcp_call: gitlab/get_issue: Read one GitLab issue."
        ),
        "{description}"
    );
    assert!(
        description.contains("The function call is only the transport envelope"),
        "{description}"
    );
    assert!(
        description.contains("required-function-call compliance"),
        "{description}"
    );
    assert!(
        description.contains("put that action in this function call now"),
        "{description}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies generic OpenAI-compatible Chat Completions provider options tune
/// MAAP request shape without importing DeepSeek shims.
///
/// LM Studio-class backends vary in their `tool_choice`, parallel-tool, and
/// output-token field support. This regression proves the provider-level
/// compatibility options are wired into request construction while preserving
/// the single canonical `submit_maap_action_batch` tool.
fn openai_compatible_chat_completions_provider_honors_generic_maap_options() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-options-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let mut profile_options = std::collections::BTreeMap::new();
    profile_options.insert("max_output_tokens".to_string(), "64".to_string());
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: profile_options,
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::RespondOnly);
    let arguments = serde_json::json!({
        "rationale": "generic options returned structured output",
        "thought": null,
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": "text/plain; charset=utf-8",
                "text": "hello"
            }
        ]
    })
    .to_string();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "local-chat-model",
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
                                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                                        "arguments": arguments
                                    }
                                }
                            ]
                        }
                    }
                ]
            })
            .to_string(),
        },
    };
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("tool_choice".to_string(), "required".to_string());
    provider_options.insert("parallel_tool_calls".to_string(), "enabled".to_string());
    provider_options.insert(
        "output_token_field".to_string(),
        "max_completion_tokens".to_string(),
    );

    let provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &provider_options,
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(
        response.action_batch.unwrap().rationale,
        "generic options returned structured output"
    );
    let sent = provider.transport.requests.borrow();
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    let body_text = sent[0].body.as_str();
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(body["max_completion_tokens"], 64);
    assert!(body.get("max_tokens").is_none());
    assert_eq!(
        body["tools"][0]["function"]["name"],
        OPENAI_MAAP_FUNCTION_TOOL_NAME
    );
    assert!(!body_text.contains(DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies that generic OpenAI-compatible Chat Completions can recover a
/// structured MAAP batch when an LM Studio-class backend serializes the JSON in
/// `reasoning_content` instead of `message.content` or native `tool_calls`.
///
/// Some local Qwen-backed integrations emit an empty assistant `content`
/// string, leave `tool_calls` empty, and place the valid MAAP batch JSON in a
/// provider-specific reasoning field. This regression keeps the compatibility
/// fallback narrowly scoped to empty visible-content responses.
fn openai_compatible_chat_completions_provider_recovers_structured_maap_from_reasoning_content() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-reasoning-structured-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::RespondOnly);
    let reasoning_content = serde_json::json!({
        "rationale": "generic compatible provider returned structured JSON in reasoning_content",
        "thought": null,
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": "text/plain; charset=utf-8",
                "text": "hello"
            }
        ]
    })
    .to_string();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "local-chat-model",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "",
                            "reasoning_content": reasoning_content,
                            "tool_calls": []
                        }
                    }
                ]
            })
            .to_string(),
        },
    };
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("maap_output".to_string(), "structured_json".to_string());
    provider_options.insert("structured_output".to_string(), "json_schema".to_string());

    let provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &provider_options,
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(
        response.action_batch.unwrap().rationale,
        "generic compatible provider returned structured JSON in reasoning_content"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies that generic OpenAI-compatible Chat Completions can encode MAAP as
/// a structured JSON response instead of a native tool call.
///
/// LM Studio-compatible local models can obey `response_format.json_schema`
/// while failing to return real OpenAI `tool_calls`. This regression proves the
/// opt-in mode omits tool request fields, sends the active MAAP schema through
/// structured output, and parses the assistant content as the same MAAP action
/// batch payload used by native tools.
fn openai_compatible_chat_completions_provider_supports_structured_maap_output() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-structured-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::RespondOnly);
    let content = serde_json::json!({
        "rationale": "generic compatible provider returned structured JSON",
        "thought": null,
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": "text/plain; charset=utf-8",
                "text": "hello"
            }
        ]
    })
    .to_string();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "local-chat-model",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": content,
                            "tool_calls": []
                        }
                    }
                ]
            })
            .to_string(),
        },
    };
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("maap_output".to_string(), "structured_json".to_string());
    provider_options.insert("structured_output".to_string(), "json_schema".to_string());

    let provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &provider_options,
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(
        response.action_batch.unwrap().rationale,
        "generic compatible provider returned structured JSON"
    );
    let sent = provider.transport.requests.borrow();
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    let body_text = sent[0].body.as_str();
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("parallel_tool_calls").is_none());
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        OPENAI_MAAP_FUNCTION_TOOL_NAME
    );
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["properties"]["actions"]["items"]["anyOf"]
            [0]["properties"]["type"]["enum"][0],
        "say"
    );
    assert!(body.get("thinking").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert!(!body_text.contains(DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies generic OpenAI-compatible Chat Completions providers do not inherit
/// DeepSeek-only request fields or shim tool names.
///
/// The compatible adapter must expose the standard OpenAI-style function tool
/// surface so local OpenAI-compatible servers are not forced through DeepSeek's
/// thinking-mode, `reasoning_content`, or three-shim MAAP contract. This
/// regression sends a normal action request through the configured compatible
/// provider and checks both the outgoing request body and parsed tool-call
/// response.
fn openai_compatible_chat_completions_provider_uses_generic_tool_surface() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let mut request = assemble_model_request(
        &ModelProfile {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
            reasoning_profile: Some("high".to_string()),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = mez_agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::RespondOnly);
    let arguments = serde_json::json!({
        "rationale": "generic compatible provider returned structured output",
        "thought": null,
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": "text/plain; charset=utf-8",
                "text": "hello"
            }
        ]
    })
    .to_string();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "local-chat-model",
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
                                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                                        "arguments": arguments
                                    }
                                }
                            ]
                        }
                    }
                ],
                "usage": {
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "prompt_tokens_details": {
                        "cached_tokens": 2
                    }
                }
            })
            .to_string(),
        },
    };

    let provider = openai_compatible_provider_from_auth_store_with_provider_options(
        &auth_store,
        "local-openai-chat",
        Some("http://localhost:1234/v1"),
        &std::collections::BTreeMap::new(),
        120_000,
        transport,
    )
    .unwrap()
    .with_stream(true);
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.provider, "local-openai-chat");
    assert_eq!(response.usage.input_tokens, 7);
    assert_eq!(response.usage.output_tokens, 3);
    assert_eq!(response.usage.cached_input_tokens, Some(2));
    assert_eq!(
        response.action_batch.unwrap().rationale,
        "generic compatible provider returned structured output"
    );
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, "http://localhost:1234/v1/chat/completions");
    assert_eq!(sent[0].headers.get("Authorization"), None);
    assert_eq!(
        sent[0].headers.get("Accept").map(String::as_str),
        Some("application/json")
    );
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    assert_eq!(body["stream"], false);
    let body_text = sent[0].body.as_str();
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(
        body["tools"][0]["function"]["name"],
        OPENAI_MAAP_FUNCTION_TOOL_NAME
    );
    assert_eq!(body["parallel_tool_calls"], false);
    assert!(body.get("thinking").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert!(!body_text.contains(DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME));
    let _ = std::fs::remove_dir_all(root);
}
