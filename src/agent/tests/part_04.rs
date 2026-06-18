/// Verifies that an API-key provider built from configuration expands
/// `base_url` before issuing requests. Without this regression coverage, a
/// configured value such as `https://api.openai.com/v1` can be treated as a
/// literal Responses endpoint, breaking normal requests while model listing
/// appears superficially valid.
#[test]
fn openai_provider_from_auth_store_expands_configured_base_url() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-auth-base-url-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_api_key("default", "sk-provider-test", &credential_store)
        .unwrap();
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
            body: r#"{"model":"gpt-test","output_text":"ok"}"#.to_string(),
        },
    };

    let provider = openai_provider_from_auth_store_with_options(
        &auth_store,
        Some("https://api.openai.com/v1"),
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, OPENAI_RESPONSES_ENDPOINT);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies configured OpenAI Responses-compatible providers can run without
/// stored auth metadata.
///
/// Local API servers such as LM Studio commonly accept unauthenticated
/// OpenAI-compatible requests. Missing metadata must therefore build a provider
/// that omits `Authorization` instead of failing before the HTTP request.
#[test]
fn openai_responses_compatible_provider_omits_auth_when_metadata_is_absent() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-no-auth-responses-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    let request = assemble_model_request(
        &ModelProfile {
            provider: "lmstudio".to_string(),
            model: "local-model".to_string(),
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
            body: r#"{"model":"local-model","output_text":"ok"}"#.to_string(),
        },
    };

    let provider = openai_responses_provider_from_auth_store_with_provider_options(
        &auth_store,
        "lmstudio",
        Some("http://localhost:1234/v1"),
        &std::collections::BTreeMap::new(),
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.provider, "lmstudio");
    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, "http://localhost:1234/v1/responses");
    assert_eq!(sent[0].headers.get("Authorization"), None);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies Chat Completions-compatible providers can list models without
/// stored auth metadata.
///
/// OpenAI-compatible and DeepSeek-compatible local backends share the same
/// optional-auth contract: no configured credential means no bearer header,
/// not an early authentication failure.
#[test]
fn chat_completions_compatible_providers_omit_auth_when_metadata_is_absent() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-no-auth-chat-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
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
    let deepseek_provider = deepseek_chat_completions_provider_from_auth_store_with_provider_options(
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

/// Verifies generic OpenAI-compatible Chat Completions providers do not inherit
/// DeepSeek-only request fields or shim tool names.
///
/// The compatible adapter must expose the standard OpenAI-style function tool
/// surface so local OpenAI-compatible servers are not forced through DeepSeek's
/// thinking-mode, `reasoning_content`, or three-shim MAAP contract. This
/// regression sends a normal action request through the configured compatible
/// provider and checks both the outgoing request body and parsed tool-call
/// response.
#[test]
fn openai_compatible_chat_completions_provider_uses_generic_tool_surface() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
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
    .unwrap();
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
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    let body_text = sent[0].body.as_str();
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(body["tools"][0]["function"]["name"], OPENAI_MAAP_FUNCTION_TOOL_NAME);
    assert_eq!(body["parallel_tool_calls"], false);
    assert!(body.get("thinking").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert!(!body_text.contains(DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME));
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies generic OpenAI-compatible Chat Completions tool descriptions list
/// the callable MCP tools in plain language.
///
/// Some local OpenAI-compatible models choose actions from the function
/// description before inspecting nested JSON Schema variants. The selected
/// tool wrapper therefore needs to name MCP server/tool routes directly instead
/// of relying only on the `mcp_call` schema branch.
#[test]
fn openai_compatible_chat_completions_provider_describes_callable_mcp_tools() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-mcp-description-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
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
            label: "user".to_string(),
            content: "use GitLab issue operations".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions.extend([
        crate::agent::AllowedAction::McpCall,
        crate::agent::AllowedAction::MemorySearch,
        crate::agent::AllowedAction::MemoryStore,
    ]);
    request.available_mcp_tools = vec![crate::mcp::McpPromptTool {
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
        description
            .contains("Available MCP tools callable with mcp_call: gitlab/get_issue: Read one GitLab issue."),
        "{description}"
    );
    assert!(
        description.contains("The function call is the action-batch envelope"),
        "{description}"
    );
    assert!(
        description.contains("put that action in this function call now"),
        "{description}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that generic OpenAI-compatible Chat Completions can encode MAAP as
/// a structured JSON response instead of a native tool call.
///
/// LM Studio-compatible local models can obey `response_format.json_schema`
/// while failing to return real OpenAI `tool_calls`. This regression proves the
/// opt-in mode omits tool request fields, sends the active MAAP schema through
/// structured output, and parses the assistant content as the same MAAP action
/// batch payload used by native tools.
#[test]
fn openai_compatible_chat_completions_provider_supports_structured_maap_output() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-structured-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
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
            label: "user".to_string(),
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::RespondOnly);
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
        body["response_format"]["json_schema"]["schema"]["properties"]["actions"]
            ["items"]["anyOf"][0]["properties"]["type"]["enum"][0],
        "say"
    );
    assert!(body.get("thinking").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert!(!body_text.contains(DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME));
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that generic OpenAI-compatible Chat Completions can recover a
/// structured MAAP batch when an LM Studio-class backend serializes the JSON in
/// `reasoning_content` instead of `message.content` or native `tool_calls`.
///
/// Some local Qwen-backed integrations emit an empty assistant `content`
/// string, leave `tool_calls` empty, and place the valid MAAP batch JSON in a
/// provider-specific reasoning field. This regression keeps the compatibility
/// fallback narrowly scoped to empty visible-content responses.
#[test]
fn openai_compatible_chat_completions_provider_recovers_structured_maap_from_reasoning_content() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-reasoning-structured-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
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
            label: "user".to_string(),
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::RespondOnly);
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

/// Verifies generic OpenAI-compatible Chat Completions provider options tune
/// MAAP request shape without importing DeepSeek shims.
///
/// LM Studio-class backends vary in their `tool_choice`, parallel-tool, and
/// output-token field support. This regression proves the provider-level
/// compatibility options are wired into request construction while preserving
/// the single canonical `submit_maap_action_batch` tool.
#[test]
fn openai_compatible_chat_completions_provider_honors_generic_maap_options() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-generic-chat-options-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
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
    assert_eq!(body["tools"][0]["function"]["name"], OPENAI_MAAP_FUNCTION_TOOL_NAME);
    assert!(!body_text.contains(DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME));
    assert!(!body_text.contains(DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME));
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that the OpenAI provider adapter can parse the provider's model
/// catalog shape and carry provider-supplied reasoning metadata when it is
/// present. The parser also fills known OpenAI reasoning defaults for model
/// entries that do not include explicit reasoning metadata.
#[test]
fn openai_models_catalog_parser_extracts_models_and_reasoning_levels() {
    let models = parse_openai_models_http_body(
        r#"{"object":"list","data":[{"id":"gpt-5.5"},{"id":"gpt-custom","display_name":"Custom","reasoning":{"efforts":["tiny","large"]},"context_length":262144},{"id":"lmstudio-local","capabilities":["tool_use"],"structured_output":true}]}"#,
    )
    .unwrap();

    assert_eq!(models.len(), 3);
    let custom = models
        .iter()
        .find(|model| model.id == "gpt-custom")
        .unwrap();
    assert_eq!(custom.display_name.as_deref(), Some("Custom"));
    assert_eq!(custom.reasoning_levels, vec!["tiny", "large"]);
    assert_eq!(custom.context_window_tokens, Some(262_144));
    let lmstudio = models
        .iter()
        .find(|model| model.id == "lmstudio-local")
        .unwrap();
    assert_eq!(
        lmstudio.capabilities,
        vec!["tool_use".to_string(), "structured_output".to_string()]
    );
    let defaulted = models.iter().find(|model| model.id == "gpt-5.5").unwrap();
    assert_eq!(
        defaulted.reasoning_levels,
        vec!["low", "medium", "high", "xhigh"]
    );
    assert_eq!(defaulted.context_window_tokens, Some(1_050_000));
}

/// Verifies that model listing uses the sibling model-catalog endpoint for the
/// direct API-key Responses endpoint and refuses to invent an equivalent
/// endpoint for ChatGPT browser credentials. The ChatGPT Codex backend is not
/// the public OpenAI Models API and should fall back to configured models.
#[test]
fn openai_models_endpoint_derives_from_responses_endpoint() {
    assert_eq!(
        openai_models_endpoint_for_responses_endpoint(OPENAI_RESPONSES_ENDPOINT).unwrap(),
        OPENAI_MODELS_ENDPOINT
    );
    let chatgpt_error =
        openai_models_endpoint_for_responses_endpoint(CHATGPT_RESPONSES_ENDPOINT).unwrap_err();
    assert!(
        chatgpt_error
            .message()
            .contains("ChatGPT browser credentials"),
        "{}",
        chatgpt_error.message()
    );
    assert_eq!(
        openai_models_endpoint_for_responses_endpoint("https://example.test/v1/responses").unwrap(),
        "https://example.test/v1/models"
    );
}

/// Verifies that configured OpenAI provider URLs are interpreted as API base
/// URLs, not as literal request endpoints. This protects the config contract:
/// `https://api.openai.com/v1` must drive model requests through `/models` and
/// normal generation requests through `/responses`.
#[test]
fn openai_responses_endpoint_derives_from_configured_base_url() {
    assert_eq!(
        openai_responses_endpoint_for_base_url("https://api.openai.com/v1").unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
    assert_eq!(
        openai_responses_endpoint_for_base_url("https://api.openai.com/v1/").unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
    assert_eq!(
        openai_responses_endpoint_for_base_url(OPENAI_RESPONSES_ENDPOINT).unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
    assert_eq!(
        openai_responses_endpoint_for_base_url(OPENAI_MODELS_ENDPOINT).unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
}

/// Verifies that `ModelProvider::list_models` for OpenAI issues an authenticated
/// GET request and normalizes the response into a model catalog with any
/// provider-reported quota usage. This is the provider-backed path consumed by
/// the agent `/model list` runtime command.
#[test]
fn openai_provider_lists_models_through_authenticated_catalog_request() {
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: std::collections::BTreeMap::from([
                ("x-ratelimit-limit-requests".to_string(), "40".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "30".to_string(),
                ),
            ]),
            body: r#"{"data":[{"id":"gpt-5.5"}]}"#.to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::new("sk-model-list", transport).unwrap();

    let catalog = provider.list_models().unwrap();

    assert_eq!(catalog.provider, "openai");
    assert_eq!(catalog.source, "provider");
    assert_eq!(catalog.models[0].id, "gpt-5.5");
    assert_eq!(
        catalog.reasoning_levels,
        vec!["low", "medium", "high", "xhigh"]
    );
    assert_eq!(catalog.quota_usage.len(), 1);
    assert_eq!(catalog.quota_usage[0].name, "requests");
    assert_eq!(catalog.quota_usage[0].used_percent_display(), "25.00%");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].method, "GET");
    assert_eq!(sent[0].url, OPENAI_MODELS_ENDPOINT);
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer sk-model-list")
    );
}

/// Verifies that OpenAI model catalog requests include the documented
/// organization and project routing headers when configured. Multi-org and
/// project-scoped API keys depend on these headers for accurate model access,
/// usage accounting, and provider-reported rate-limit measurements.
#[test]
fn openai_provider_model_catalog_uses_documented_accounting_headers() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-openai-routing-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_api_key("default", "sk-routed", &credential_store)
        .unwrap();
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("organization_id".to_string(), "org_configured".to_string());
    provider_options.insert("project_id".to_string(), "proj_configured".to_string());
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"object":"list","data":[{"id":"gpt-routed","object":"model","created":1686935002,"owned_by":"openai"}]}"#
                .to_string(),
        },
    };
    let provider = openai_provider_from_auth_store_with_provider_options(
        &auth_store,
        Some("https://api.openai.com/v1"),
        &provider_options,
        120_000,
        transport,
    )
    .unwrap();

    let catalog = provider.list_models().unwrap();

    assert_eq!(catalog.models[0].id, "gpt-routed");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, OPENAI_MODELS_ENDPOINT);
    assert_eq!(
        sent[0]
            .headers
            .get("OpenAI-Organization")
            .map(String::as_str),
        Some("org_configured")
    );
    assert_eq!(
        sent[0].headers.get("OpenAI-Project").map(String::as_str),
        Some("proj_configured")
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies OpenAI rate-limit headers are normalized into stable percentage
/// measurements even when numeric header values contain common visual
/// separators. Provider headers are the documented live rate-limit source for
/// ordinary API-key requests.
#[test]
fn openai_rate_limit_headers_allow_grouped_numeric_values() {
    let quotas = provider_quota_usage_from_headers(&std::collections::BTreeMap::from([
        (
            "X-RateLimit-Limit-Requests".to_string(),
            "1,000".to_string(),
        ),
        (
            "X-RateLimit-Remaining-Requests".to_string(),
            "750".to_string(),
        ),
        ("X-RateLimit-Reset-Requests".to_string(), "1s".to_string()),
    ]));

    assert_eq!(quotas.len(), 1);
    assert_eq!(quotas[0].name, "requests");
    assert_eq!(quotas[0].limit, 1000);
    assert_eq!(quotas[0].remaining, 750);
    assert_eq!(quotas[0].used_percent_display(), "25.00%");
    assert_eq!(quotas[0].reset.as_deref(), Some("1s"));
}

/// Verifies that a ChatGPT browser/device login is not treated as a direct API
/// key. ChatGPT credentials must go to the ChatGPT Codex backend and include
/// the account-id header that selects the authenticated account.
#[test]
fn openai_provider_from_auth_store_routes_chatgpt_credentials_to_codex_backend() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-chatgpt-auth-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "chatgpt-access-token".to_string(),
                refresh_token: Some("refresh-token".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: None,
                token_expires_at: Some("12345".to_string()),
            },
            &credential_store,
        )
        .unwrap();
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
                "event: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "ok"}]
                    }
                }),
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "resp_1", "model": "gpt-test"}
                })
            ),
        },
    };

    let provider = openai_provider_from_auth_store_with_transport(&auth_store, transport).unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, CHATGPT_RESPONSES_ENDPOINT);
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer chatgpt-access-token")
    );
    assert_eq!(
        sent[0].headers.get("Accept").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(
        sent[0]
            .headers
            .get(CHATGPT_ACCOUNT_ID_HEADER)
            .map(String::as_str),
        Some("acct_123")
    );
    let request_body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    assert_eq!(request_body["stream"], true);
    let metadata = std::fs::read_to_string(auth_store.paths().auth_file()).unwrap();
    assert!(metadata.contains("credential_kind = \"chatgpt\""));
    assert!(!metadata.contains("chatgpt-access-token"));
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that provider HTTP failures surface the response error message.
/// This keeps auth regressions actionable instead of reducing them to an
/// undifferentiated status code such as `401`.
#[test]
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

/// Verifies provider HTTP failure sanitization redacts secret-like strings
/// even when upstream places the credential under generic fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
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

/// Verifies that streaming provider failure events preserve the structured
/// failure object for runtime audit records. ChatGPT-backed OpenAI auth uses
/// the streaming endpoint, so these diagnostics must survive SSE parsing.
#[test]
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

/// Verifies output-limit incomplete streaming responses keep structured
/// diagnostics so runtime recovery can retry compactly instead of failing the
/// turn as an opaque invalid provider state.
#[test]
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
    assert!(provider_error_is_output_limit_exceeded(
        error.message(),
        error.provider_failure_json()
    ));
    assert!(!super::provider_error_is_context_limit_exceeded(
        error.message(),
        error.provider_failure_json()
    ));
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(
        failure_json["incomplete_details"]["reason"],
        "max_output_tokens"
    );
}

/// Verifies openai response parser reports api errors and missing text.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn openai_response_parser_reports_api_errors_and_missing_text() {
    let error = parse_openai_responses_http_body(r#"{"error":{"message":"bad auth"}}"#, "gpt-test")
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("bad auth"));

    let missing =
        parse_openai_responses_http_body(r#"{"model":"gpt-test","output":[]}"#, "gpt-test")
            .unwrap_err();
    assert_eq!(missing.kind(), crate::error::MezErrorKind::InvalidState);
}

/// Verifies turn runner blocks shell actions requiring approval.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_blocks_shell_actions_requiring_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "run command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Run the requested command".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn.clone(),
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "check changes".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"state\":\"pending\"")
    );
}

/// Verifies that auto-allow only advances a prompted shell action when the
/// model supplies the explicit approval hint and rationale required for the
/// active request.
#[test]
fn turn_runner_runs_prompted_shell_actions_with_auto_allow_assertion() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "run command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Run the requested command".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::AutoAllow);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "check changes".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""state":"auto_allowed""#)
    );
}

/// Verifies config changes follow the active approval policy instead of using
/// a bespoke hard-block path.
///
/// Live configuration changes still run through the runtime config-control path,
/// but permissive approval modes should accept the action at planning time just
/// like other privileged model actions.
#[test]
fn turn_runner_accepts_config_change_with_full_access_and_bypass() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::ConfigChange,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "change the requested live setting".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![config_change_action("config-1")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let mut policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
    policy.set_approval_bypass(true);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "change my theme to kanagawa".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec!["configuration change accepted for runtime application"]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"bypassed""#), "{structured}");
    assert!(
        structured.contains(r#""status":"pending_runtime_config_change""#),
        "{structured}"
    );
}

/// Verifies memory actions plan as runtime-owned work instead of falling
/// through the shell-action planner.
///
/// Persistent memory operations execute through the runtime store after the
/// planner marks them as running. This regression ensures the planner produces
/// a pending runtime result so memory actions can continue instead of failing
/// with the shell-backed-action planning error.
#[test]
fn turn_runner_accepts_memory_store_for_runtime_execution() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Memory,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "store the requested memory".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "memory-1".to_string(),
                    rationale: "store durable project context".to_string(),
                    payload: AgentActionPayload::MemoryStore {
                        kind: "fact".to_string(),
                        priority: Some(60),
                        scope: Some("project".to_string()),
                        keywords: vec!["memory".to_string(), "regression".to_string()],
                        content: "remember this regression scenario".to_string(),
                        expires_in_days: Some(7),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: true,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "remember this for later".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        execution.action_results[0].content_texts(),
        vec!["memory action accepted for runtime execution"]
    );
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"pending_runtime_memory""#));
}

/// Verifies that auto-allow uses the model rationale as its reasonableness
/// assessment. The reduced MAAP shape no longer carries a separate approval
/// hint field.
#[test]
fn turn_runner_auto_allows_prompted_shell_actions_from_rationale() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "run command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Run the requested command".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::AutoAllow);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "check changes".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""state":"auto_allowed""#)
    );
}

/// Verifies that the turn planner consumes shell-resolved path scopes when
/// deciding whether a shell action may auto-run. A command whose canonical path
/// escapes the active read scope must become a blocked approval request rather
/// than a running pane write.
#[test]
fn turn_runner_blocks_shell_actions_with_canonical_scope_escape() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "read file".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Read the requested file".to_string(),
                        command: "cat link/secret.txt".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let scopes = PathScopes::shell_resolved("/repo", vec!["/repo".to_string()], Vec::new())
        .with_canonical_path("link/secret.txt", "/outside/secret.txt");
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: Some(&scopes),
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""state":"pending""#)
    );
}

/// Verifies turn runner blocks mcp actions requiring approval.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_blocks_mcp_actions_requiring_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "read through external integration".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: true,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["fs".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"kind\":\"mcp_call\"")
    );
}

/// Verifies full-access approval policy accepts MCP actions that would
/// otherwise need an explicit approval prompt.
///
/// This protects the user-selected full-access mode from being treated like
/// the default ask mode for semantic integration actions.
#[test]
fn turn_runner_full_access_accepts_mcp_actions_requiring_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "read through external integration".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: true,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["fs".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains(r#""state":"full_access""#)),
        "{execution:?}"
    );
}

/// Verifies that MCP tools with approval requirements follow the same
/// auto-allow contract as shell commands: they may run only when the model
/// supplies an explicit reasoned assertion for the active request.
#[test]
fn turn_runner_auto_allows_mcp_actions_with_model_assertion() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "read requested project file through external integration"
                        .to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "fs".to_string(),
                        tool: "read_file".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::AutoAllow);
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: true,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["fs".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "read file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""state":"auto_allowed""#)
    );
}

/// Verifies turn runner accepts mcp actions without required approval.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_accepts_mcp_actions_without_required_approval() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "inspect external state".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "state".to_string(),
                        tool: "list".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        description: "List state".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["state".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "list state".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        !execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("approval_required")
    );
}

/// Verifies that provider MAAP output is rejected before action planning when
/// it names a tool that was not advertised as available for an otherwise
/// available MCP server.
#[test]
fn turn_runner_rejects_mcp_actions_for_unavailable_tools_before_planning() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "inspect disabled external state".to_string(),
                    payload: AgentActionPayload::McpCall {
                        server: "state".to_string(),
                        tool: "write".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        description: "List state".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["state".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "write state".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.action_results.is_empty());
    assert!(
        execution
            .response
            .raw_text
            .contains("maap_validation_error"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("unavailable or disabled tool"),
        "{}",
        execution.response.raw_text
    );
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Failed);
}

/// Verifies that MAAP validation failures are repaired through a bounded ephemeral
/// provider retry before the runtime records a failed turn. The correction
/// instruction must be present only in the retry request; the returned
/// execution keeps the original request so transcripts and later context do not
/// inherit the validation error when repair succeeds.
#[test]
fn turn_runner_retries_maap_validation_error_without_persisting_repair_context() {
    let turn = turn();
    let capability = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "request mcp capability".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![capability_action("capability-1", AgentCapability::Mcp)],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
};
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid unavailable mcp action".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![AgentAction {
                id: "mcp-1".to_string(),
                rationale: "inspect unavailable state".to_string(),
                payload: AgentActionPayload::McpCall {
                    server: "missing".to_string(),
                    tool: "read".to_string(),
                    arguments_json: "{}".to_string(),
                },
            }],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
};
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected say response".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "I cannot access that MCP server.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
};
    let provider = SequencedProvider::new(vec![Ok(capability), Ok(invalid), Ok(corrected)]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        description: "List state".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["state".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn.clone(),
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect missing mcp state".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.response.raw_text, "corrected say response");
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("maap_validation_error")
                && !message.content.contains("ephemeral maap repair")),
        "{:?}",
        execution.request.messages
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .messages
            .iter()
            .find(|message| message.content.contains("ephemeral maap repair"))
            .unwrap()
            .content
            .contains("ephemeral maap repair"),
        "{:?}",
        requests[2].messages
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .find(|message| message.content.contains("ephemeral maap repair"))
            .unwrap()
            .content
            .contains("unavailable server"),
        "{:?}",
        requests[2].messages
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .find(|message| message.content.contains("ephemeral maap repair"))
            .unwrap()
            .content
            .contains("The corrected batch is the schema-valid wrapper for the next useful action"),
        "{:?}",
        requests[2].messages
    );
    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    assert!(
        entries.iter().all(|entry| {
            !entry.content.contains("ephemeral maap repair")
                && !entry.content.contains("maap_validation_error")
                && !entry.content.contains("invalid unavailable mcp action")
        }),
        "{entries:?}"
    );
}

/// Verifies heredoc shell commands are repairable MAAP validation failures.
///
/// Shell commands are exposed only after a capability request, so this test
/// first grants the shell surface and then returns a disabled heredoc command.
/// The runner should send a bounded ephemeral repair request with file-action
/// guidance, accept the corrected response, and avoid retaining the repair
/// diagnostic in durable execution context.
#[test]
fn turn_runner_repairs_shell_command_heredoc_validation_error() {
    let turn = turn();
    let capability = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "request shell capability".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![capability_action("capability-1", AgentCapability::Shell)],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
};
    let mut heredoc_action = shell_action("shell-heredoc");
    if let AgentActionPayload::ShellCommand {
        command, summary, ..
    } = &mut heredoc_action.payload
    {
        *summary = "Write a Rust file with a heredoc".to_string();
        *command = "cat > hello.rs <<'EOF'\nfn main() {}\nEOF".to_string();
    }
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid heredoc shell response".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![heredoc_action],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
};
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected file action response".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "I will use a file action instead.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
};
    let provider = SequencedProvider::new(vec![Ok(capability), Ok(invalid), Ok(corrected)]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn.clone(),
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "write a short Rust program".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(
        execution.response.raw_text,
        "corrected file action response"
    );
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("heredoc redirection is disabled")),
        "{:?}",
        execution.request.messages
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let repair_message = &requests[2]
        .messages
        .iter()
        .find(|message| message.content.contains("ephemeral maap repair"))
        .unwrap()
        .content;
    assert!(
        repair_message.contains("ephemeral maap repair"),
        "{repair_message}"
    );
    assert!(
        repair_message.contains("heredoc redirection is disabled"),
        "{repair_message}"
    );
    assert!(repair_message.contains("apply_patch"), "{repair_message}");
}

/// Verifies mixed capability-routing batches defer heredoc shell validation.
/// 
/// When a provider combines `request_capability` with a shell command that
/// would otherwise fail MAAP validation, the runner must treat the response as
/// mixed capability routing first, avoid executing or validating the deferred
/// shell payload, and ask the model to re-emit work on the expanded surface.
#[test]
fn turn_runner_recovers_mixed_capability_batch_before_heredoc_validation() {
    let turn = turn();
    let mut deferred_heredoc = shell_action("shell-heredoc");
    if let AgentActionPayload::ShellCommand {
        command, summary, ..
    } = &mut deferred_heredoc.payload
    {
        *summary = "Write a Rust file with a heredoc".to_string();
        *command = "cat > hello.rs <<'EOF'\nfn main() {}\nEOF".to_string();
    }
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request shell and write file".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![
                    capability_action("capability-1", AgentCapability::Shell),
                    deferred_heredoc,
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "Ready.")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "write a short Rust program".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.response.raw_text, "ready");
    assert!(execution
        .action_results
        .iter()
        .all(|result| result.action_type != "shell_command"));
    assert!(execution
        .request
        .messages
        .iter()
        .all(|message| !message.content.contains("ephemeral maap repair")));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    let recovery_context = requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[mixed capability batch recovery]"))
        .expect("missing mixed capability recovery context");
    assert!(recovery_context.content.contains("shell_command"));
    assert!(!recovery_context
        .content
        .contains("heredoc redirection is disabled"));
}

/// Verifies that malformed provider-native MAAP output can also be repaired
/// without surfacing the malformed output as a durable turn when the retry
/// returns a valid action batch.
#[test]
fn turn_runner_retries_malformed_provider_maap_output() {
    let turn = turn();
    let malformed =
        crate::MezError::invalid_args("provider MAAP output is malformed: missing required field")
            .with_provider_raw_text(
                r#"{"rationale":"test action batch rationale","actions":[{"type":"say"}]}"#,
            );
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected malformed response".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "Corrected.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
};
    let provider = SequencedProvider::new(vec![Err(malformed), Ok(corrected)]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "reply".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| {
                message.content.contains(
                    r#"{"rationale":"test action batch rationale","actions":[{"type":"say"}]}"#,
                )
            })
            .unwrap()
            .content
            .contains(r#"{"rationale":"test action batch rationale","actions":[{"type":"say"}]}"#),
        "{:?}",
        requests[1].messages
    );
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("ephemeral maap repair")),
        "{:?}",
        execution.request.messages
    );
}

/// Verifies the async turn runner applies the same ephemeral MAAP repair path
/// used by the synchronous runner so production provider workers can recover
/// from model schema mistakes without adding repair instructions to context.
#[tokio::test]
async fn async_turn_runner_retries_maap_validation_error_without_persisting_repair_context() {
    let turn = turn();
    let capability = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "request mcp capability".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![capability_action("capability-1", AgentCapability::Mcp)],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
};
    let invalid = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "invalid unavailable mcp action".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![AgentAction {
                id: "mcp-1".to_string(),
                rationale: "inspect unavailable state".to_string(),
                payload: AgentActionPayload::McpCall {
                    server: "missing".to_string(),
                    tool: "read".to_string(),
                    arguments_json: "{}".to_string(),
                },
            }],
            final_turn: false,
        }),
        provider_transcript_events: Vec::new(),
};
    let corrected = ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "corrected async response".to_string(),
        usage: Default::default(),
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "Corrected asynchronously.")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
};
    let provider = SequencedProvider::new(vec![Ok(capability), Ok(invalid), Ok(corrected)]);
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        description: "List state".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["state".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect missing mcp state".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(provider.requests().len(), 3);
    assert!(
        execution
            .request
            .messages
            .iter()
            .all(|message| !message.content.contains("ephemeral maap repair")),
        "{:?}",
        execution.request.messages
    );
}

/// Verifies mcp action executor maps tool response to action result.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mcp_action_executor_maps_tool_response_to_action_result() {
    let turn = turn();
    let action = mcp_action("mcp-1");
    let plan = mcp_plan();
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpToolCallResponse {
            content_json: r#"[{"type":"text","text":"ok"}]"#.to_string(),
            structured_content_json: Some(r#"{"items":1}"#.to_string()),
            is_error: false,
        },
    };

    let result = execute_mcp_action_through_runtime(&turn, &action, &plan, &mut executor).unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(result.content_texts(), vec!["ok"]);
    assert_eq!(executor.plans, vec![plan]);
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("\"server\":\"state\"")
    );
}

/// Verifies mcp action executor maps tool errors to failed results.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mcp_action_executor_maps_tool_errors_to_failed_results() {
    let turn = turn();
    let action = mcp_action("mcp-1");
    let plan = mcp_plan();
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpToolCallResponse {
            content_json: r#"[{"type":"text","text":"denied"}]"#.to_string(),
            structured_content_json: None,
            is_error: true,
        },
    };

    let result = execute_mcp_action_through_runtime(&turn, &action, &plan, &mut executor).unwrap();

    assert_eq!(result.status, ActionStatus::Failed);
    assert!(result.is_error);
    assert_eq!(result.error.as_ref().unwrap().code, "mcp_tool_error");
    assert_eq!(result.content_texts(), vec!["denied"]);
}

/// Verifies turn runner executes accepted mcp actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_executes_accepted_mcp_actions() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Mcp,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![mcp_action("mcp-1")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let tools = vec![McpPromptTool {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        description: "List state".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
    }];
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: vec!["state".to_string()],
        available_mcp_tools: &tools,
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };
    let mut executor = FakeMcpActionExecutor {
        plans: Vec::new(),
        response: McpToolCallResponse {
            content_json: r#"[{"type":"text","text":"ok"}]"#.to_string(),
            structured_content_json: None,
            is_error: false,
        },
    };

    let execution = runner
        .run_turn_with_mcp_executor(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "list state".to_string(),
            }])
            .unwrap(),
            &mut executor,
            |_action| Ok(mcp_plan()),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(executor.plans.len(), 1);
}

/// Verifies turn runner routes shell actions through approval policy without model effects.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_routes_shell_actions_through_approval_policy_without_model_effects() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "inspect environment variables".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect environment variables".to_string(),
                        command: "env".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect environment".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
}

/// Verifies that an unknown shell command is routed through approval policy
/// without relying on provider-declared or provider-visible effect metadata.
/// The safe behavior is a pending approval in `ask` mode.
#[test]
fn turn_runner_blocks_unknown_classified_shell_actions_without_declared_effect_failure() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "inspect with a short interpreter command".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect with a short interpreter command".to_string(),
                        command: "python3 -c 'print(1)'".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "run script".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"pending_approval""#));
    assert!(
        structured.contains(r#""command":"python3 -c 'print(1)'""#),
        "{structured}"
    );
}

/// Verifies subagent scope checks do not convert unknown shell effects into a
/// hard denial before approval policy runs. Broad interpreter commands still
/// need approval in ask mode, but full-access sessions should be able to run
/// read-only discovery scripts through the normal permission model.
#[test]
fn turn_runner_routes_subagent_unknown_shell_actions_through_approval_policy() {
    let mut turn = turn();
    turn.agent_id = "agent-%2".to_string();
    turn.pane_id = "%2".to_string();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "script action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "inspect repository metadata with a read-only script".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect repository metadata with a read-only script".to_string(),
                        command: "python3 -c 'print(\"metadata\")'".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let subagent_scope = crate::subagent::SubagentScopeDeclaration {
        cooperation_mode: crate::subagent::CooperationMode::ExploreOnly,
        current_directory: "/home/neil".to_string(),
        read_scopes: vec![
            "/home/neil/.codex".to_string(),
            "/home/neil/.cargo".to_string(),
        ],
        write_scopes: Vec::new(),
        permission_preset: None,
    };
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: Some(&subagent_scope),
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "search local repositories".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
}

/// Verifies full-access sessions do not treat subagent read scopes as hard
/// command denials.
///
/// Scope declarations still describe the child agent's intended work area, but
/// full-access mode is the user's explicit choice to avoid whitelist and scope
/// prompts. The runner must therefore route concrete read commands through the
/// normal permission policy instead of failing before policy evaluation.
#[test]
fn turn_runner_full_access_treats_subagent_read_scopes_as_advisory() {
    let mut turn = turn();
    turn.agent_id = "agent-%2".to_string();
    turn.pane_id = "%2".to_string();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "read action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "a1".to_string(),
                    rationale: "inspect local instructions".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Inspect local instructions".to_string(),
                        command: "cat AGENTS.md".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
    let approvals = SessionApprovalStore::default();
    let subagent_scope = crate::subagent::SubagentScopeDeclaration {
        cooperation_mode: crate::subagent::CooperationMode::ExploreOnly,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["/elsewhere".to_string()],
        write_scopes: Vec::new(),
        permission_preset: None,
    };
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: Some(&subagent_scope),
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "summarize local instructions".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
}

/// Verifies that model-supplied action ids are ignored at the MAAP boundary.
/// Mezzanine assigns stable local ids so downstream action results still have
/// bookkeeping keys without trusting provider-generated identifiers.
#[test]
fn parser_synthesizes_action_ids_and_ignores_model_ids() {
    let batch = parse_maap_action_batch_json(
        r#"{
          "protocol": "maap/1",
          "turn_id": "turn-1",
          "agent_id": "agent-1",
          "rationale": "test action batch rationale",
          "actions": [
            {"id":"model-picked","type":"say","status":"final","rationale":"Reply","text":"hello"},
            {"type":"say","status":"final","rationale":"Reply again","text":"again"}
          ],
          "final": true
        }"#,
    )
    .unwrap();

    assert_eq!(batch.actions[0].id, "action-1");
    assert_eq!(batch.actions[1].id, "action-2");
}

/// Verifies that the turn planner accepts the common MAAP response for listing
/// the current directory. The runtime may only know the pane cwd at this point,
/// so `ls` without path arguments must not fail as an unknown-effect action.
#[test]
fn turn_runner_accepts_ls_declared_as_current_directory_read() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![AgentAction {
                    id: "list-current-directory".to_string(),
                    rationale: "list files in the current directory".to_string(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "List files in the current directory".to_string(),
                        command: "ls".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(1000),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let path_scopes = PathScopes::unresolved("/repo", Vec::new(), Vec::new());
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: Some(&path_scopes),
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "list the files in the current directory".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap();
    assert!(structured.contains(r#""state":"pending_dispatch""#));
    assert!(structured.contains(r#""command":"ls""#), "{structured}");
}

/// Verifies turn runner accepts allowed shell actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_accepts_allowed_shell_actions() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![shell_action("a1")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "where am I".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""sent_to_pane":false"#)
    );
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""terminal_observation":{"state":"pending_dispatch"}"#)
    );
}

/// Verifies turn runner keeps final shell action running until observed.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_keeps_final_shell_action_running_until_observed() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![shell_action("a1")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "where am I".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
}

/// Verifies turn runner executes allowed shell actions and records output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn turn_runner_executes_allowed_shell_actions_and_records_output() {
    let turn = turn();
    let provider = CapabilityBatchProvider::new(
        AgentCapability::Shell,
        ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![shell_action("a1")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    );
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
    let mut ledger = AgentTurnLedger::new(false);
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: ModelProfile {
            provider: "batch".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        permissions: &policy,
        approvals: &approvals,
        path_scopes: None,
        subagent_scope: None,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
                memory_actions_enabled: false,
                issue_actions_enabled: true,
    };
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            stdout: framed_shell_output("/repo\n"),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let execution = runner
        .run_turn_with_shell_executor(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "where am I".to_string(),
            }])
            .unwrap(),
            Path::new("/bin/sh"),
            &mut executor,
            |_action| Ok(marker()),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(execution.action_results[0].content_texts(), vec!["/repo\n"]);
    assert_eq!(executor.requests.len(), 1);
    assert_eq!(executor.requests[0].action_id, "a1");
}

/// Verifies shell classification classifies by binary name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn shell_classification_classifies_by_binary_name() {
    use std::path::Path;

    assert_eq!(
        ShellClassification::classify(Path::new("/bin/bash")),
        ShellClassification::Bash
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/usr/bin/zsh")),
        ShellClassification::Zsh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/usr/local/bin/fish")),
        ShellClassification::Fish
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/bin/sh")),
        ShellClassification::PosixSh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/bin/dash")),
        ShellClassification::PosixSh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/usr/bin/ksh")),
        ShellClassification::PosixSh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/opt/custom-shell")),
        ShellClassification::UnknownUnix
    );
    assert_eq!(
        ShellClassification::classify(Path::new("")),
        ShellClassification::UnknownUnix
    );
}

/// Verifies that shell version probe output wins over filename-derived
/// classification. The bootstrap parser receives both fields, and the probed
/// runtime shell identity is more authoritative than `$SHELL` basename text.
#[test]
fn shell_classification_probe_takes_precedence_over_reported_name() {
    assert_eq!(
        ShellClassification::classify_with_probe(Path::new("/bin/sh"), Some("fish, version 3.7.1")),
        ShellClassification::Fish
    );

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\thost\n\
env\tuser\tuser\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tsh\n\
env\tshell_version\tfish, version 3.7.1\n\
env\tcwd\t/repo\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t0\n";
    let (signature, _, _) = parse_bootstrap_env_output(output, Path::new("/bin/sh"));
    let signature = signature.unwrap();

    assert_eq!(signature.shell_classification, ShellClassification::Fish);
}

/// Verifies shell classification as str matches spec.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn shell_classification_as_str_matches_spec() {
    assert_eq!(ShellClassification::Bash.as_str(), "bash");
    assert_eq!(ShellClassification::Zsh.as_str(), "zsh");
    assert_eq!(ShellClassification::Fish.as_str(), "fish");
    assert_eq!(ShellClassification::PosixSh.as_str(), "posix-sh");
    assert_eq!(ShellClassification::UnknownUnix.as_str(), "unknown-unix");
}

/// Verifies parse bootstrap env output parses complete synthetic output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parse_bootstrap_env_output_parses_complete_synthetic_output() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\tkernel_version\t6.8.0-generic\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
env\tshell_path\t/bin/bash\n\
env\tshell_class\tbash\n\
env\tshell_version\tGNU bash, version 5.2.21\n\
env\tpath\t/usr/local/bin:/usr/bin:/bin\n\
env\tcwd\t/home/me/project\n\
env\tproject_root\t/home/me/project\n\
env\tgit_repo\t1\n\
env\tcontainer\tdocker\n\
env\tenv_manager\tvirtualenv:/home/me/.venv\n\
env\tenv_manager\trustup\n\
bootstrap\tcomplete\t1714500000\n\
tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t1714500000\n\
tool\tgrep\t1\t/usr/bin/grep\tGNU grep 3.11\tcommand -v grep\t0\t/usr/bin/grep --version\t0\t1714500000\n\
tool\tpython\t1\t/usr/bin/python3\tPython 3.12.3\tcommand -v python3 || command -v python\t0\t/usr/bin/python3 --version\t0\t1714500000\n";

    let (signature, inventory, instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/bash"));

    let sig = signature.expect("signature should be parsed");
    assert_eq!(sig.os, "Linux");
    assert_eq!(sig.arch, "x86_64");
    assert_eq!(sig.kernel_version.as_deref(), Some("6.8.0-generic"));
    assert_eq!(sig.host, "myhost");
    assert_eq!(sig.user, "me");
    assert_eq!(sig.shell_path, "/bin/bash");
    assert_eq!(sig.shell_classification, ShellClassification::Bash);
    assert_eq!(
        sig.shell_version.as_deref(),
        Some("GNU bash, version 5.2.21")
    );
    assert_eq!(sig.path.as_deref(), Some("/usr/local/bin:/usr/bin:/bin"));
    assert_eq!(sig.working_directory, "/home/me/project");
    assert_eq!(sig.project_root.as_deref(), Some("/home/me/project"));
    assert!(sig.git_repo);
    assert_eq!(sig.container.as_deref(), Some("docker"));
    assert_eq!(
        sig.environment_managers,
        vec![
            "rustup".to_string(),
            "virtualenv:/home/me/.venv".to_string()
        ]
    );

    let inv = inventory.expect("inventory should be parsed");
    assert!(inv.sed);
    assert!(inv.grep);
    assert!(inv.python);

    assert!(
        instruction_files.is_empty(),
        "no instruction lines in synthetic output"
    );
}

/// Verifies parse bootstrap env output handles empty fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parse_bootstrap_env_output_handles_empty_fields() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
bootstrap\tcomplete\t1714500000\n";

    let (signature, _inventory, _instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    let sig = signature.expect("signature should be parsed");
    assert_eq!(sig.os, "Linux");
    assert_eq!(sig.shell_classification, ShellClassification::PosixSh);
    assert_eq!(sig.shell_version, None);
    assert_eq!(sig.path, None);
    assert_eq!(sig.kernel_version, None);
    assert_eq!(sig.project_root, None);
    assert!(!sig.git_repo);
}

/// Verifies bootstrap parsing does not trust mismatched `$SHELL` metadata over
/// the resolved pane shell when choosing wrapper classification.
///
/// Async pane workers can fall back to `/bin/sh` even when the outer test or
/// launcher environment exports `SHELL=/bin/bash`. The bootstrap metadata still
/// records that environment shell, but runtime wrapper flags must stay aligned
/// with the actual resolved pane shell to avoid passing bash-only options to
/// `/bin/sh`.
#[test]
fn parse_bootstrap_env_output_prefers_resolved_shell_for_mismatched_metadata() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
env\tshell_path\t/bin/bash\n\
env\tshell_class\tbash\n\
env\tshell_version\tGNU bash, version 5.2.21\n\
env\tcwd\t/repo\n\
bootstrap\tcomplete\t1714500000\n";

    let (signature, _inventory, _instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    let sig = signature.expect("signature should be parsed");
    assert_eq!(sig.shell_path, "/bin/bash");
    assert_eq!(sig.shell_classification, ShellClassification::PosixSh);
    assert_eq!(sig.shell_version.as_deref(), Some("GNU bash, version 5.2.21"));
}

/// Verifies parse bootstrap env output returns none for empty output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parse_bootstrap_env_output_returns_none_for_empty_output() {
    use std::path::Path;

    let (signature, inventory, instruction_files) =
        parse_bootstrap_env_output("", Path::new("/bin/sh"));
    assert!(signature.is_none());
    assert!(inventory.is_none());
    assert!(instruction_files.is_empty());
}

/// Verifies environment signature known fields includes all fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn environment_signature_known_fields_includes_all_fields() {
    let sig = test_env_signature("myhost", "me", "/bin/bash", "/repo");
    let fields = sig.known_fields();

    assert!(fields.iter().any(|f| f == "os=linux"));
    assert!(fields.iter().any(|f| f == "arch=x86_64"));
    assert!(fields.iter().any(|f| f == "host=myhost"));
    assert!(fields.iter().any(|f| f == "user=me"));
    assert!(fields.iter().any(|f| f == "shell_path=/bin/bash"));
    assert!(fields.iter().any(|f| f == "shell_classification=bash"));
    assert!(fields.iter().any(|f| f == "working_directory=/repo"));
    assert!(fields.iter().any(|f| f == "git_repo=0"));
}

/// Verifies model-facing environment context uses a fixed-width signature hash.
///
/// Full host/user/PATH data is useful for internal caches and audit, but it is
/// not task-specific model context. The model projection should stay compact
/// and stable even when the shell environment is large.
#[test]
fn environment_signature_model_fields_use_hashed_identity() {
    let sig = EnvironmentSignature::new(
        "linux",
        "x86_64",
        Some("6.6.0".to_string()),
        "myhost",
        "me",
        "/bin/bash",
        ShellClassification::Bash,
        Some("GNU bash".to_string()),
        Some("/usr/bin:/bin:/very/long/tool/path".to_string()),
        "/repo",
        Some("/repo".to_string()),
        true,
        None,
        vec!["mise".to_string()],
    )
    .expect("test environment signature should be valid");

    let fields = sig.model_context_fields();
    let joined = fields.join("\n");

    assert!(joined.contains("env_signature=sha256:"));
    assert!(joined.contains("cwd=/repo"));
    assert!(joined.contains("shell=bash"));
    assert!(joined.contains("path_entries=3"));
    assert!(!joined.contains("host=myhost"), "{joined}");
    assert!(!joined.contains("user=me"), "{joined}");
    assert!(!joined.contains("/very/long/tool/path"), "{joined}");
    assert_eq!(sig.stable_hash().len(), 64);
}

/// Verifies bootstrap script is valid shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn bootstrap_script_is_valid_shell() {
    let script = bootstrap_script();
    assert!(!script.is_empty());
    assert!(script.contains("uname -s"));
    assert!(script.contains("uname -m"));
    assert!(script.contains("hostname"));
    assert!(script.contains("whoami"));
    assert!(script.contains("SHELL"));
    assert!(script.contains("PATH"));
    assert!(script.contains("pwd"));
    assert!(script.contains(".git"));
    assert!(script.contains("VIRTUAL_ENV"));
    assert!(script.contains("CONDA_PREFIX"));
    assert!(script.contains("bootstrap"));
    assert!(script.contains("complete"));
    assert!(script.contains("AGENTS.md"));
    assert!(script.contains("mez_inst_"));
    assert!(script.contains("mez_probe_tool"));
    assert!(script.contains("tool\\t%s"));
}

/// Verifies that Fish bootstrap discovery has a Fish-native script surface with
/// the same output markers as the POSIX bootstrap script.
#[test]
fn fish_bootstrap_script_emits_bootstrap_and_instruction_markers() {
    let script = bootstrap_script_for_classification(ShellClassification::Fish);

    assert!(script.contains("function mez_bootstrap_field"));
    assert!(script.contains("status fish-path"));
    assert!(script.contains("mez_bootstrap_field shell_class fish"));
    assert!(script.contains("AGENTS.md"));
    assert!(script.contains("instruction\\tpath=%s"));
    assert!(script.contains("bootstrap\\tcomplete"));
    assert!(script.contains("function mez_probe_tool"));
    assert!(script.contains("tool\\t%s"));
}

/// Verifies that the bootstrap output parser extracts instruction files from
/// the synthetic bootstrap output emitted by instruction discovery shell code.
#[test]
fn parse_bootstrap_env_output_extracts_instruction_files() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
bootstrap\tcomplete\t0\n\
instruction\tpath=./AGENTS.md\tscope=.\tbytes=12\ttruncated=false\tcontent=root guide\\n\n\
instruction\tpath=./src/AGENTS.md\tscope=./src\tbytes=7\ttruncated=false\tcontent=child\\n\n";

    let (_signature, _inventory, instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    assert_eq!(instruction_files.len(), 2);
    assert_eq!(instruction_files[0].path, "./AGENTS.md");
    assert_eq!(instruction_files[0].scope_root, ".");
    assert_eq!(instruction_files[0].content, "root guide\n");
    assert_eq!(instruction_files[1].path, "./src/AGENTS.md");
    assert_eq!(instruction_files[1].scope_root, "./src");
    assert_eq!(instruction_files[1].content, "child\n");
}

/// Verifies that tool discovery lines in bootstrap output do not interfere
/// with instruction file extraction and that mixed output is parsed correctly.
#[test]
fn parse_bootstrap_env_output_isolates_instructions_from_tools() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
instruction\tpath=./AGENTS.md\tscope=.\tbytes=6\ttruncated=false\tcontent=hello\\n\n\
tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t0\n\
tool\tgrep\t1\t/usr/bin/grep\tGNU grep 3.11\tcommand -v grep\t0\t/usr/bin/grep --version\t0\t0\n\
bootstrap\tcomplete\t0\n";

    let (_signature, inventory, instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    assert_eq!(instruction_files.len(), 1);
    assert_eq!(instruction_files[0].content, "hello\n");
    let inv = inventory.expect("tool inventory should be parsed");
    assert!(inv.sed);
    assert!(inv.grep);
}

/// Verifies that shell action results in the executor path include the marker
/// token in the terminal_observation JSON.
#[test]
fn shell_action_executor_result_includes_marker_in_terminal_observation() {
    let turn = turn();
    let action = shell_action("shell-marker");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let result = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""marker":"#),
        "terminal_observation in executor path should include marker: {structured}"
    );
}

/// Verifies that shell action results infer a signal from exit codes greater
/// than 128 in the POSIX convention (128 + signal number).
#[test]
fn shell_action_executor_infers_signal_from_high_exit_code() {
    let turn = turn();
    let action = shell_action("shell-signal");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(137), // 128 + 9 (SIGKILL)
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let result = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""signal":9"#) || structured.contains(r#""signal": 9"#),
        "should infer signal 9 from exit code 137: {structured}"
    );
}

/// Verifies that an interrupted shell action reports SIGINT (signal 2)
/// in the terminal_observation.
#[test]
fn shell_action_executor_reports_sigint_for_interrupted_action() {
    let turn = turn();
    let action = shell_action("shell-interrupt");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            interrupted: true,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let result = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap();

    assert_eq!(result.status, ActionStatus::Interrupted);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""signal":2"#) || structured.contains(r#""signal": 2"#),
        "should report signal 2 (SIGINT) for interrupted action: {structured}"
    );
}

/// Verifies that a normal exit code does not report a signal.
#[test]
fn shell_action_executor_reports_null_signal_for_normal_exit() {
    let turn = turn();
    let action = shell_action("shell-no-signal");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let result = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""signal":null"#) || structured.contains(r#""signal": null"#),
        "normal exit code should not produce a signal: {structured}"
    );
}
