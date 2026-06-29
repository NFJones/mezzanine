/// Verifies DeepSeek thinking MAAP requests fall back to strict non-thinking
/// MAAP when the provider returns prose instead of a tool call.
///
/// The first request follows DeepSeek's thinking-mode pattern by advertising
/// tools without forced `tool_choice`. If the model declines to call the MAAP
/// function, the adapter retries once with thinking disabled and a forced MAAP
/// function so the runtime still receives a structured action batch.
#[test]
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
    let provider = crate::agent::DeepSeekChatCompletionsProvider::new("deepseek-key", transport)
        .unwrap();

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

/// Verifies named OpenAI-compatible DeepSeek providers keep their configured
/// runtime identity when sending a Chat Completions request.
///
/// The openai-compatible dispatch path reuses the DeepSeek Chat Completions
/// adapter, so the adapter must accept the configured provider name instead of
/// rejecting the request as if every compatible endpoint were the built-in
/// `deepseek` provider. This locks the regression that surfaced as
/// `DeepSeek provider received a request for a different provider` before any
/// HTTP request was sent.
#[test]
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


/// Verifies DeepSeek does not return a successful provider response when the
/// strict fallback still omits the required MAAP batch.
///
/// The runtime can repair provider malformed-output errors, but it cannot
/// repair a response that was accepted as successful with `action_batch=None`.
/// This locks the adapter to converting missing DeepSeek MAAP output into a
/// provider diagnostic that preserves the bad response text.
#[test]
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
    let provider = crate::agent::DeepSeekChatCompletionsProvider::new("deepseek-key", transport)
        .unwrap();

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

/// Recursively validates strict-schema object requirements with a path that
/// makes provider 400 regressions diagnosable from test failures.
fn assert_openai_strict_schema_shape_at(schema: &serde_json::Value, path: &str) {
    if let Some(object) = schema.as_object() {
        if let Some(properties) = object
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            let required = object
                .get("required")
                .unwrap_or_else(|| panic!("schema object at {path} is missing required"))
                .as_array()
                .unwrap_or_else(|| panic!("schema required at {path} is not an array"));
            let mut property_names = properties.keys().cloned().collect::<Vec<_>>();
            let mut required_names = required
                .iter()
                .map(|field| {
                    field
                        .as_str()
                        .unwrap_or_else(|| panic!("schema required field at {path} is not string"))
                        .to_string()
                })
                .collect::<Vec<_>>();
            property_names.sort();
            required_names.sort();
            assert_eq!(
                required_names, property_names,
                "strict schema object at {path} must require every property"
            );
            assert_eq!(
                object.get("additionalProperties"),
                Some(&serde_json::Value::Bool(false)),
                "strict schema object at {path} must deny additional properties"
            );
        }
        for (key, child) in object {
            assert_openai_strict_schema_shape_at(child, &format!("{path}.{key}"));
        }
    } else if let Some(items) = schema.as_array() {
        for (index, child) in items.iter().enumerate() {
            assert_openai_strict_schema_shape_at(child, &format!("{path}[{index}]"));
        }
    }
}

/// Verifies that runtime-discovered MCP tool schemas are attached to the
/// provider request rather than being used only for post-response MAAP
/// validation. Provider adapters need this metadata to constrain native
/// structured output before the model proposes an MCP action.
#[test]
fn turn_runner_passes_mcp_tool_schemas_to_provider_request() {
    let turn = turn();
    let provider = RequestCapturingProvider {
        response: ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
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
                    id: "complete".to_string(),
                    rationale: "done".to_string(),
                    payload: AgentActionPayload::Complete,
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }];
    let policy = PermissionPolicy::default();
    let approvals = SessionApprovalStore::default();
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
    let mut ledger = AgentTurnLedger::new(false);
    runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "finish".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    let request = provider
        .last_request
        .borrow()
        .clone()
        .expect("provider should receive request");
    assert_eq!(request.available_mcp_tools, tools);
}

/// Verifies direct turn starts reject duplicate identifiers across all ledger
/// states so lifecycle recovery cannot create orphaned records. The regression
/// covers a previously reported defense-in-depth gap where `start_turn` could
/// have appended a reused turn id while later lifecycle APIs updated only the
/// first matching record.
#[test]
fn agent_turn_ledger_start_turn_rejects_duplicate_turn_id() {
    let mut ledger = AgentTurnLedger::new(true);
    let mut duplicate = turn();
    duplicate.agent_id = "agent-other".to_string();

    ledger.start_turn(turn()).unwrap();

    let error = ledger.start_turn(duplicate).unwrap_err();

    assert_eq!(error.message(), "agent turn id already exists");
    assert_eq!(ledger.turns().len(), 1);
}

/// Verifies terminal turn states are immutable once recorded in the ledger. A
/// failed, completed, or interrupted turn must not later be reclassified by a
/// duplicate finish path because scheduler, transcript, and metrics callers all
/// rely on the first terminal result as the authoritative turn outcome.
#[test]
fn agent_turn_ledger_rejects_duplicate_terminal_finish() {
    let mut ledger = AgentTurnLedger::new(false);
    ledger.start_turn(turn()).unwrap();
    ledger
        .finish_turn("turn-1", AgentTurnState::Failed)
        .unwrap();

    let error = ledger
        .finish_turn("turn-1", AgentTurnState::Completed)
        .unwrap_err();

    assert_eq!(error.message(), "agent turn is already terminal");
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Failed);
}

/// Verifies that executable action surfaces are only exposed after the model
/// asks for a coarse capability. This protects the state-machine boundary that
/// keeps a greeting or other simple request from starting with shell or
/// network actions before the model opts into those broader capabilities.
#[test]
fn turn_runner_exposes_shell_actions_only_after_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request shell capability".to_string(),
            usage: ModelTokenUsage {
                input_tokens: 900,
                output_tokens: 20,
                reasoning_tokens: 5,
                cached_input_tokens: Some(300),
                cache_write_input_tokens: None,
            },
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
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: ModelTokenUsage {
                input_tokens: 251,
                output_tokens: 30,
                reasoning_tokens: 7,
                cached_input_tokens: Some(80),
                cache_write_input_tokens: None,
            },
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![shell_action("shell-1")],
                final_turn: false,
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
                content: "where am I".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.response.usage.input_tokens, 1151);
    assert_eq!(execution.response.usage.output_tokens, 50);
    assert_eq!(execution.response.usage.reasoning_tokens, 12);
    assert_eq!(execution.latest_response_usage.input_tokens, 251);
    assert_eq!(execution.latest_response_usage.output_tokens, 30);
    assert_eq!(execution.latest_response_usage.reasoning_tokens, 7);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].interaction_kind,
        crate::agent::ModelInteractionKind::CapabilityDecision
    );
    let initial_actions = requests[0].allowed_actions.action_type_names();
    assert!(initial_actions.contains(&"request_capability"));
    assert!(!initial_actions.contains(&"shell_command"));
    assert!(!initial_actions.contains(&"fetch_url"));
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    let execution_actions = requests[1].allowed_actions.action_type_names();
    assert!(execution_actions.contains(&"shell_command"));
    assert!(execution_actions.contains(&"request_capability"));
    assert!(!execution_actions.contains(&"fetch_url"));
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[capability granted]"))
            .unwrap()
            .content
            .contains("[capability granted]"),
        "{:?}",
        requests[1].messages
    );
}

/// Verifies mixed capability-routing and executable batches recover without effects.
///
/// A model may request a missing capability and optimistically include the
/// action that needs it in the same response. The controller must not execute
/// that invalid mixed batch, but it should still honor the capability request
/// and ask the model to re-emit deferred work on the expanded action surface.
#[test]
fn turn_runner_recovers_mixed_capability_and_execution_batch_without_effects() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request shell and run it".to_string(),
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
                    shell_action("shell-1"),
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
                content: "inspect the repository".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert!(execution
        .action_results
        .iter()
        .all(|result| result.action_type != "shell_command"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    let execution_actions = requests[1].allowed_actions.action_type_names();
    assert!(execution_actions.contains(&"shell_command"));
    assert!(execution_actions.contains(&"apply_patch"));
    assert!(execution_actions.contains(&"request_capability"));
    let recovery_context = requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[mixed capability batch recovery]"))
        .expect("missing mixed capability recovery context");
    assert!(recovery_context.content.contains("shell_command"));
}

/// Verifies disabled local issue tracking denies issue capability before the
/// provider-visible issue action surface can be exposed.
///
/// This protects the action-surface contract documented in `SPEC.md`: when
/// `issues.enabled` is false, models may ask for the capability but the
/// controller must keep them on the non-effecting capability-decision surface
/// instead of revealing `issue_add`, `issue_query`, or related actions.
#[test]
fn turn_runner_denies_issues_capability_when_issue_tracking_disabled() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request issues capability".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![capability_action("capability-1", AgentCapability::Issues)],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "finish after denied capability".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "issue tracking is disabled")],
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
        issue_actions_enabled: false,
    };

    let execution = runner
        .run_turn(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "list project issues".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::CapabilityDecision
    );
    assert_eq!(
        requests[1].allowed_actions.action_type_names(),
        vec!["say", "request_capability"]
    );
    let capability_context = requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[capability denied]"))
        .expect("missing denied capability context");
    assert!(
        capability_context
            .content
            .contains("issues capability requires local issue tracking to be enabled"),
        "{}",
        capability_context.content
    );
    assert!(!requests[1]
        .allowed_actions
        .action_type_names()
        .contains(&"issue_query"));
}

/// Verifies capability negotiation does not reintroduce skill lookup actions
/// after an explicit `$skill` prompt has already loaded the workflow.
///
/// The original failure mode repeatedly asked for `request_skills` after the
/// runtime reported that `$create-skill` was already loaded. This locks the
/// suppression to both the initial capability-decision request and the
/// post-capability execution request.
#[test]
fn turn_runner_keeps_skill_actions_suppressed_after_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
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
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "finish after capability grant".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "done")],
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
            AgentContext::new(vec![
                ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    label: "explicit skill create-skill".to_string(),
                    content: "# Skill: create-skill\n\nCreate or update skills.".to_string(),
                },
                ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    label: "user prompt".to_string(),
                    content: "$create-skill create a review skill".to_string(),
                },
            ])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].allowed_actions.action_type_names(),
        vec!["say", "request_capability"]
    );
    assert_eq!(
        requests[1].allowed_actions.action_type_names(),
        vec!["say", "request_capability", "shell_command", "apply_patch"]
    );
    let capability_context = requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[capability granted]"))
        .expect("missing capability context");
    assert!(
        capability_context
            .content
            .contains("allowed_actions=say,request_capability,shell_command,apply_patch"),
        "{}",
        capability_context.content
    );
}

/// Verifies enabled persistent memory is exposed on the main model's initial
/// action surface instead of requiring a separate capability request.
///
/// Memory lookup and storage are intended to be routine context actions for the
/// main model when runtime memory is enabled. This regression ensures the first
/// provider request can call `memory_search` or `memory_store` directly while
/// still retaining `request_capability` for shell, network, MCP, and other
/// coarse effects.
#[test]
fn turn_runner_exposes_memory_actions_on_initial_surface_when_enabled() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![Ok(ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "done".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "finish after inspecting memory".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "done")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    })]);
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
                content: "use any helpful memory before answering".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].interaction_kind,
        crate::agent::ModelInteractionKind::CapabilityDecision
    );
    let allowed_actions = requests[0].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"memory_search"));
    assert!(allowed_actions.contains(&"memory_store"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert!(!allowed_actions.contains(&"shell_command"));
}

/// Verifies the model-facing memory store schema exposes only durable memory
/// kinds and excludes episodic or scratch storage categories.
///
/// This regression keeps the provider-visible schema aligned with the memory
/// policy that ordinary agent turns must not persist transcript summaries,
/// scratch notes, or other current-turn-only operational state.
#[test]
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
            label: "user".to_string(),
            content: "remember durable context".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Memory);

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
        serde_json::json!(["preference", "fact", "procedure", "warning"])
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
    let content_description = memory_store_schema["properties"]["content"]["description"]
        .as_str()
        .unwrap();
    assert!(content_description.contains("reusable beyond the current task"));
    assert!(content_description.contains("not already present in current context"));
    assert!(content_description.contains("not user-provided only for this task"));
    assert!(content_description.contains("almost certain to be useful in future sessions"));
    assert!(content_description.contains("Emit at most one memory_store action in one user turn"));
    assert!(content_description.contains("current checkout repo slugs"));
    assert!(content_description.contains("owner/repo"));
    assert!(content_description.contains("CI results"));
}

/// Verifies the model-facing memory search schema forbids startup-ritual
/// searches and repeated paraphrase retries.
///
/// This regression keeps provider-visible guidance aligned with the stricter
/// no-memory-by-default policy so models do not treat persistent memory as a
/// normal first step on non-trivial turns.
#[test]
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
            label: "user".to_string(),
            content: "remember durable context".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Memory);

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
    assert!(query_description.contains("never more than two memory_search actions in one user turn"));
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

/// Verifies available MCP tools are exposed on the main model's initial
/// action surface instead of requiring a separate capability request.
///
/// MCP-backed integrations should be callable immediately when the runtime has
/// already surfaced concrete tools for the turn. This regression ensures the
/// first provider request can emit `mcp_call` directly while still retaining
/// `request_capability` for shell, network, and other coarse effects.
#[test]
fn turn_runner_exposes_mcp_actions_on_initial_surface_when_available() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![Ok(ModelResponse {
        provider: "batch".to_string(),
        model: "test".to_string(),
        raw_text: "done".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "finish after checking MCP tools".to_string(),
            thought: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            actions: vec![say_action("say-1", "done")],
            final_turn: true,
        }),
        provider_transcript_events: Vec::new(),
    })]);
    let tools = vec![McpPromptTool {
        server_id: "fs".to_string(),
        tool_name: "read_file".to_string(),
        description: "Read file".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_string(),
    }];
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
                content: "use any helpful MCP integration before answering".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].interaction_kind,
        crate::agent::ModelInteractionKind::CapabilityDecision
    );
    let allowed_actions = requests[0].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"mcp_call"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert!(!allowed_actions.contains(&"shell_command"));
}

/// Verifies the shared default action-gate helper exposes the same concrete
/// MCP and memory actions that the selected-model runner adds before provider
/// submission.
///
/// Runtime request-shape diagnostics use this helper without executing a full
/// turn. This regression keeps those diagnostics aligned with the live runner
/// and the SPEC-defined mixed default surface so an initial selected-model
/// request with MCP tools is not reported as a capability-only or memory-only
/// surface.
#[test]
fn default_action_gates_expose_mcp_and_memory_for_diagnostic_request_shapes() {
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
            content: "use any helpful MCP integration before answering".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let tools = vec![McpPromptTool {
        server_id: "gitlab".to_string(),
        tool_name: "get_issue".to_string(),
        description: "Read one GitLab issue".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object"}"#.to_string(),
    }];

    super::apply_default_action_gates(&mut request, &tools, true, false);

    let allowed_actions = request.allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"mcp_call"));
    assert!(allowed_actions.contains(&"memory_search"));
    assert!(allowed_actions.contains(&"memory_store"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert_eq!(request.available_mcp_tools, tools);
    assert!(request.memory_actions_enabled);
    assert!(!request.issue_actions_enabled);
}

/// Verifies available MCP tools do not suppress the persistent-memory surface.
///
/// MCP availability is not a global reason to hide other enabled capabilities.
/// This keeps memory usable for turns that legitimately need durable prior
/// context even when MCP servers are configured.
#[test]
fn default_action_gates_keep_memory_when_mcp_is_available() {
    let mcp_tool = McpPromptTool {
        server_id: "githubcopilot".to_string(),
        tool_name: "list_ci_results".to_string(),
        description: "Read GitHub CI check results for a repository. User-configured non-authoritative server purpose: GitHub repository and CI operations.".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object"}"#.to_string(),
    };
    let context = crate::agent::append_mcp_context(
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "use the githubcopilot mcp server to inspect CI".to_string(),
        }])
        .unwrap(),
        &crate::mcp::McpPromptSummary {
            available_servers: vec![crate::mcp::McpPromptServer {
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

    super::apply_default_action_gates(&mut request, std::slice::from_ref(&mcp_tool), true, false);

    let allowed_actions = request.allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"mcp_call"));
    assert!(allowed_actions.contains(&"memory_search"));
    assert!(allowed_actions.contains(&"memory_store"));
    assert!(allowed_actions.contains(&"request_capability"));
    assert_eq!(request.available_mcp_tools, vec![mcp_tool]);
    assert!(request.memory_actions_enabled);
}


/// Verifies model-authored aborts are repaired instead of treated as a valid
/// way to end recoverable turns. A model that merely needs more repository
/// context must continue by requesting capability or performing available
/// actions rather than converting a solvable task into a terminal abort.
#[test]
fn turn_runner_repairs_model_authored_abort_during_capability_decision() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"rationale":"test action batch rationale","actions":[{"reason":"need more repository context","type":"abort"}]}"#
                .to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![abort_action("abort-1", "need more repository context")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request workspace-read capability".to_string(),
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
                content: "inspect the workspace".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::Repair
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| {
                message
                    .content
                    .contains("abort is not part of the provider action surface")
            })
            .unwrap()
            .content
            .contains("abort is not part of the provider action surface"),
        "{:?}",
        requests[1].messages
    );
    assert!(
        !requests[0]
            .allowed_actions
            .action_type_names()
            .contains(&"abort")
    );
}

/// Verifies legacy model-authored completion actions are rejected when omitted
/// from the active allowed-action surface.
///
/// `complete` is not exposed by the current provider schema, so a legacy
/// provider response that injects it must go through the normal action-surface
/// validation and repair path instead of bypassing execution checks.
#[test]
fn turn_runner_repairs_legacy_complete_during_capability_decision() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"rationale":"test action batch rationale","actions":[{"type":"complete"}]}"#
                .to_string(),
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
                    id: "complete-1".to_string(),
                    rationale: "legacy completion".to_string(),
                    payload: AgentActionPayload::Complete,
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request workspace-write capability".to_string(),
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
                content: "inspect the workspace".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::Repair
    );
    assert!(
        requests[1].messages.iter().any(|message| message
            .content
            .contains("complete is not part of the provider action surface")),
        "{:?}",
        requests[1].messages
    );
    assert!(
        !requests[0]
            .allowed_actions
            .action_type_names()
            .contains(&"complete")
    );
}

/// Verifies Mezzanine `apply_patch` content remains accepted for
/// action planning.
///
/// A provider can request workspace-write capability and then emit the patch
/// block format that Codex commonly uses. The runner must plan the patch as a
/// shell-backed local action instead of sending repair feedback.
#[test]
fn turn_runner_plans_codex_style_apply_patch_after_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request workspace-write capability".to_string(),
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
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"rationale":"test action batch rationale","actions":[{"type":"apply_patch","patch":"*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"}]}"#
                .to_string(),
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
                    id: "patch-1".to_string(),
                    rationale: String::new(),
                    payload: AgentActionPayload::ApplyPatch {
                        patch:
                            "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                                .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
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
                content: "edit a file".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].action_type, "apply_patch");
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
}

/// Verifies that capability negotiation accepts an accompanying visible `say`
/// action. Provider schemas expose both actions during the initial
/// non-executing phase, so the runner must not fail when the model emits a
/// short status line with the capability request.
#[test]
fn turn_runner_accepts_say_with_capability_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "say and request shell capability".to_string(),
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
                    say_action("say-1", "I will inspect the shell state."),
                    capability_action("capability-1", AgentCapability::Shell),
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![shell_action("shell-1")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
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
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[capability granted]"))
            .unwrap()
            .content
            .contains("[capability granted]"),
        "{:?}",
        requests[1].messages
    );
}

/// Verifies that one capability-decision response can request multiple coarse
/// capabilities. Multi-agent analysis commonly needs workspace inspection plus
/// subagent coordination, and the controller should expose the union of those
/// granted surfaces instead of failing the batch as invalid.
#[test]
fn turn_runner_accepts_multiple_capability_requests_in_one_batch() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request read and subagent capability".to_string(),
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
                    say_action("say-1", "I will inspect and subdivide the work."),
                    capability_action("capability-1", AgentCapability::Shell),
                    capability_action("capability-2", AgentCapability::Subagent),
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
                actions: vec![say_action("say-2", "Ready to proceed.")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
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
                content: "compare mezzanine to codex using agents".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    let allowed_actions = requests[1].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"shell_command"));
    assert!(allowed_actions.contains(&"apply_patch"));
    assert!(allowed_actions.contains(&"spawn_agent"));
    assert!(allowed_actions.contains(&"send_message"));
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[capability decisions]"))
            .unwrap()
            .content
            .contains("[capability decisions]"),
        "{:?}",
        requests[1].messages
    );
}

/// Verifies terminal provider/controller failures get one response-only
/// characterization pass. The summary request exposes only `say`, which lets
/// the model explain the failure without recursively requesting tools or
/// capabilities after the controller has already failed the turn.
#[test]
fn turn_runner_summarizes_terminal_provider_failure_with_say_only_request() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider schema rejected request",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary".to_string(),
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
                    id: "say-1".to_string(),
                    rationale: "summarize the controller failure".to_string(),
                    payload: AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Progress,
                        text: "The provider request failed before an action could run.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
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
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let summary_batch = execution.response.action_batch.as_ref().unwrap();
    assert!(summary_batch.final_turn);
    match &summary_batch.actions[0].payload {
        AgentActionPayload::Say { status, .. } => {
            assert_eq!(*status, crate::agent::SayStatus::Final)
        }
        payload => panic!("unexpected payload: {payload:?}"),
    }
    assert!(execution.response.raw_text.contains("provider_error"));
    assert!(
        execution
            .response
            .raw_text
            .contains("controller_failure_summary")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].allowed_actions.action_type_names(),
        vec!["say"]
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .find(|message| message.content.contains("[controller failure summary]"))
            .unwrap()
            .content
            .contains("[controller failure summary]"),
        "{:?}",
        requests[1].messages
    );
}

/// Verifies failure-summary provider calls retry transient transport failures.
///
/// The final failure summary is best-effort, but the summary request is still a
/// provider interaction. A transient transport failure while asking for the
/// summary should use the same retry classification instead of immediately
/// collapsing to the unsummarized terminal provider error.
#[test]
fn turn_runner_retries_retryable_failure_summary_provider_call() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider schema rejected request",
        )),
        Err(crate::MezError::invalid_state(
            "provider HTTP response read failed: error decoding response body",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary after retry".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "The provider failed before any action ran.")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
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
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.response.raw_text.contains("controller_failure_summary"));
    assert_eq!(provider.requests().len(), 3);
}

/// Verifies malformed failure-summary MAAP responses get one repair attempt.
///
/// The summary request is constrained to response-only `say` actions. If the
/// model returns malformed MAAP for that response, the existing MAAP repair
/// prompt should give it a bounded chance to emit the valid final say batch
/// rather than silently dropping the summary.
#[tokio::test]
async fn turn_runner_repairs_malformed_failure_summary_response() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider schema rejected request",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "not a summary batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        }),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "repaired summary".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "The provider failed before any action ran.")],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        }),
    ]);
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
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.response.raw_text.contains("controller_failure_summary"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].interaction_kind,
        crate::agent::ModelInteractionKind::Repair
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .any(|message| message.content.contains("[ephemeral maap repair]")),
        "{:?}",
        requests[2].messages
    );
}

/// Verifies retryable provider transport failures are not converted into
/// terminal failure summaries.
///
/// The async runtime owns retry backoff for transient provider failures. If the
/// turn runner asks the provider for a failure-summary `say` first, a successful
/// summary turns the retryable failure into a terminal failed turn and prevents
/// the actor from scheduling the retry.
#[tokio::test]
async fn turn_runner_bubbles_retryable_provider_failure_to_runtime_retry() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "provider HTTP response read failed: error decoding response body",
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary that should not be requested".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "retry later")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
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

    let error = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("provider HTTP response read failed"),
        "{}",
        error.message()
    );
    assert_eq!(provider.requests().len(), 1);
}

/// Verifies provider context-limit failures are returned to runtime recovery
/// instead of being summarized by the same oversized request.
///
/// The runtime owns active-turn context compaction and retry scheduling. Asking
/// the provider for a terminal failure summary with the rejected context would
/// repeat the same oversized payload and hide the recoverable condition.
#[tokio::test]
async fn turn_runner_bubbles_context_limit_failure_to_runtime_recovery() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Err(crate::MezError::invalid_state(
            "OpenAI Responses API returned status 400: This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.",
        )
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        )),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary that should not be requested".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "retry later")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
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

    let error = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error.message().contains("maximum context length"),
        "{}",
        error.message()
    );
    assert_eq!(provider.requests().len(), 1);
}

/// Verifies provider/controller failures that explicitly invite retry are
/// surfaced to the runtime retry scheduler instead of being converted into a
/// terminal failure-summary exchange.
#[tokio::test]
async fn turn_runner_bubbles_provider_controller_retry_hint_to_runtime_retry() {
    let turn = turn();
    let retry_message = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID b331baf5-b254-46d7-8d3f-58b563ce7ee8 in your message.";
    let retry_error = crate::MezError::invalid_state(retry_message).with_provider_failure_json(
        serde_json::json!({
            "error": {
                "message": retry_message,
                "type": "server_error"
            }
        })
        .to_string(),
    );
    let provider = SequencedProvider::new(vec![
        Err(retry_error),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "summary that should not be requested".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "retry later")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
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

    let error = runner
        .run_turn_async(
            &mut ledger,
            turn,
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("You can retry your request"));
    assert_eq!(provider.requests().len(), 1);
}

/// Verifies that the controller grants a network fetch capability without an
/// active-context URL provenance check.
///
/// Action scoping decides whether `fetch_url` is exposed at all. The concrete
/// URL target is validated later by the parser, permission layer, executor byte
/// bounds, and network loop guard.
#[test]
fn turn_runner_grants_fetch_capability_without_context_url() {
    let turn = turn();
    let provider = SequencedProvider::new(vec![
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "request fetch capability".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![capability_action(
                    "capability-1",
                    AgentCapability::NetworkFetch,
                )],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
}),
        Ok(ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "fallback say".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", "hello")],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
}),
    ]);
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
                content: "hello".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::ActionExecution
    );
    let allowed_actions = requests[1].allowed_actions.action_type_names();
    assert!(allowed_actions.contains(&"fetch_url"));
    assert!(allowed_actions.contains(&"request_capability"));
    let decision_message = &requests[1]
        .messages
        .iter()
        .find(|message| message.content.contains("[capability granted]"))
        .unwrap()
        .content;
    assert!(decision_message.contains("[capability granted]"));
    assert!(decision_message.contains("capability is permitted"));
}

/// Verifies that a provider response without a MAAP action batch fails the turn
/// instead of silently converting malformed structured output into completion.
#[test]
fn turn_runner_fails_response_without_action_batch() {
    let turn = turn();
    let provider = BatchProvider {
        response: ModelResponse {
            provider: "batch".to_string(),
            model: "test".to_string(),
            raw_text: "plain text without maap".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
    };
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
                content: "summarize".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(execution.action_results.is_empty());
    assert_eq!(ledger.turns()[0].turn_id, turn.turn_id);
    assert_eq!(ledger.turns()[0].state, AgentTurnState::Failed);
}

/// Verifies openai responses request body maps context to responses api shape.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
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
    assert_eq!(
        value["tool_choice"]["name"],
        "submit_maap_action_batch"
    );
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
    assert!(capability_description.contains("request or use the relevant capability instead of asking the user"));
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
        15,
        "the canonical OpenAI tool exposes a stable non-MCP action superset"
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
    assert!(!action_types.contains(&"mcp_call".to_string()));
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
        serde_json::json!(crate::agent::AgentCapability::all_names())
    );
    let capability_description = capability_schema["properties"]["capability"]["description"]
        .as_str()
        .unwrap();
    assert!(
        capability_description.contains("Capability map: shell exposes shell_command and apply_patch"),
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
        capability_description.contains("issues exposes issue_add, issue_update, issue_query, and issue_delete"),
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

/// Verifies OpenAI request rendering keeps Mezzanine action results
/// provider-valid while marking them as executed evidence.
///
/// Responses input messages do not have a generic tool role for synthetic
/// Mezzanine action history, so the provider renderer must carry provenance in
/// the text instead of letting tool output look like a fresh user request.
#[test]
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
                .is_some_and(|text| text.starts_with("[executed result]"))
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
        action_text.starts_with("[executed result]\n"),
        "{action_text}"
    );
    assert!(
        action_text.contains("executed Mezzanine action output, not a new user request"),
        "{action_text}"
    );
    assert!(
        action_text.contains("[action_result action-1 shell_command succeeded]"),
        "{action_text}"
    );
}

/// Verifies OpenAI prompt-cache routing keys stay coarse enough to avoid
/// fragmenting identical static prefixes across interaction modes.
#[test]
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
    execution.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    execution.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Shell);

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

/// Verifies OpenAI prompt-cache routing keys include lineage and provider identity.
///
/// The local routing namespace should follow explicit lineage ids and survive
/// resume-like session-id changes when provider and lineage stay the same.
/// Same-provider OpenAI model switches should reuse one routing key so
/// auto-sizing does not fragment provider prompt-cache affinity, while different
/// provider compatibility targets must not share one routing key.
#[test]
fn openai_prompt_cache_key_uses_lineage_provider_and_model_namespace() {
    let context_for_session = |session_id: &str, lineage_id: Option<&str>| {
        let mut blocks = vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                label: "session identity".to_string(),
                content: format!("session_id={session_id} session_name=default"),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect the repo".to_string(),
            },
        ];
        if let Some(lineage_id) = lineage_id {
            blocks.insert(
                1,
                ContextBlock {
                    source: ContextSourceKind::Configuration,
                    label: "prompt cache lineage".to_string(),
                    content: lineage_id.to_string(),
                },
            );
        }
        AgentContext::new(blocks).unwrap()
    };
    let profile = |provider: &str, model: &str| ModelProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let inherited_lineage_openai = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-a", Some("lineage-parent")),
    )
    .unwrap();
    let inherited_lineage_same_provider_other_model = assemble_model_request(
        &profile("openai", "gpt-b"),
        &turn(),
        &context_for_session("session-a", Some("lineage-parent")),
    )
    .unwrap();
    let inherited_lineage_other_provider = assemble_model_request(
        &profile("deepseek", "deepseek-b"),
        &turn(),
        &context_for_session("session-a", Some("lineage-parent")),
    )
    .unwrap();
    let resumed_session_same_lineage = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b", Some("lineage-parent")),
    )
    .unwrap();
    let fresh_lineage = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b", Some("lineage-fresh")),
    )
    .unwrap();
    let lineage_fallback_session_a = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-a", None),
    )
    .unwrap();
    let lineage_fallback_session_b = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b", None),
    )
    .unwrap();

    let inherited_lineage_value: serde_json::Value = serde_json::from_str(
        &openai_responses_request_body(&inherited_lineage_openai).unwrap(),
    )
    .unwrap();
    let inherited_lineage_same_provider_other_model_value: serde_json::Value =
        serde_json::from_str(
            &openai_responses_request_body(&inherited_lineage_same_provider_other_model).unwrap(),
        )
        .unwrap();
    let inherited_lineage_other_provider_value: serde_json::Value = serde_json::from_str(
        &openai_responses_request_body(&inherited_lineage_other_provider).unwrap(),
    )
    .unwrap();
    let resumed_session_value: serde_json::Value = serde_json::from_str(
        &openai_responses_request_body(&resumed_session_same_lineage).unwrap(),
    )
    .unwrap();
    let fresh_lineage_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&fresh_lineage).unwrap()).unwrap();
    let fallback_a_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&lineage_fallback_session_a).unwrap())
            .unwrap();
    let fallback_b_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&lineage_fallback_session_b).unwrap())
            .unwrap();

    assert_eq!(
        inherited_lineage_value["prompt_cache_key"],
        inherited_lineage_same_provider_other_model_value["prompt_cache_key"]
    );
    assert_ne!(
        inherited_lineage_value["prompt_cache_key"],
        inherited_lineage_other_provider_value["prompt_cache_key"]
    );
    assert_eq!(
        inherited_lineage_value["prompt_cache_key"],
        resumed_session_value["prompt_cache_key"]
    );
    assert_ne!(
        inherited_lineage_value["prompt_cache_key"],
        fresh_lineage_value["prompt_cache_key"]
    );
    assert_eq!(
        fallback_a_value["prompt_cache_key"],
        fallback_b_value["prompt_cache_key"]
    );
}

/// Verifies OpenAI prompt-cache routing keys do not use live session fallback.
///
/// When no explicit lineage id is present, the key should use the stable unknown
/// lineage namespace plus provider identity instead of volatile session ids.
#[test]
fn openai_prompt_cache_key_uses_unknown_lineage_without_session_identity() {
    let context_for_session = |session_id: &str| {
        AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                label: "session identity".to_string(),
                content: format!("session_id={session_id} session_name=default"),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "inspect the repo".to_string(),
            },
        ])
        .unwrap()
    };
    let profile = |provider: &str, model: &str| ModelProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let session_a_openai = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-a"),
    )
    .unwrap();
    let session_a_other_model = assemble_model_request(
        &profile("deepseek", "deepseek-b"),
        &turn(),
        &context_for_session("session-a"),
    )
    .unwrap();
    let session_b_openai = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b"),
    )
    .unwrap();

    let session_a_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&session_a_openai).unwrap()).unwrap();
    let session_a_other_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&session_a_other_model).unwrap())
            .unwrap();
    let session_b_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&session_b_openai).unwrap()).unwrap();

    assert_ne!(
        session_a_value["prompt_cache_key"],
        session_a_other_value["prompt_cache_key"]
    );
    assert_eq!(
        session_a_value["prompt_cache_key"],
        session_b_value["prompt_cache_key"]
    );
}

/// Verifies OpenAI MAAP tool schemas track the current allowed action surface.
///
/// A single canonical function keeps action selection simple for the model, and
/// its schema carries the request's current allowed actions. The stable prompt
/// text can remain reusable while the provider request shape reflects the live
/// action schema.
#[test]
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
            label: "user".to_string(),
            content: "inspect the repo".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let mut execution = capability.clone();
    execution.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    execution.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Shell);

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
        capability_diagnostics.stable_prompt_prefix_sha256,
        execution_diagnostics.stable_prompt_prefix_sha256
    );
    assert_eq!(
        capability_diagnostics.cacheable_prefix_sha256,
        execution_diagnostics.cacheable_prefix_sha256
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

/// Verifies stable-prefix material changes when repo-scoped guidance changes,
/// while the OpenAI prompt-cache key remains a coarse routing namespace.
///
/// OpenAI already hashes the exact prompt prefix for correctness. Mezzanine's
/// explicit key should keep requests with related stable startup context routed
/// together rather than fragmenting on every prompt-prefix text change.
#[test]
fn openai_prompt_cache_key_uses_stable_namespace_not_rendered_prefix_hash() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let stable_a = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                label: "project guidance".to_string(),
                content: "use style a".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "first prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let stable_a_different_user = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                label: "project guidance".to_string(),
                content: "use style a".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "second prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let stable_b = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                label: "project guidance".to_string(),
                content: "use style b".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "first prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let stable_a_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&stable_a).unwrap()).unwrap();
    let stable_a_user_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&stable_a_different_user).unwrap())
            .unwrap();
    let stable_b_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&stable_b).unwrap()).unwrap();
    let stable_a_diagnostics = openai_prompt_cache_diagnostics_for_request(&stable_a).unwrap();
    let stable_b_diagnostics = openai_prompt_cache_diagnostics_for_request(&stable_b).unwrap();

    assert_eq!(
        openai_stable_prefix_material_for_request(&stable_a).unwrap(),
        openai_stable_prefix_material_for_request(&stable_a_different_user).unwrap()
    );
    assert_ne!(
        openai_stable_prefix_material_for_request(&stable_a).unwrap(),
        openai_stable_prefix_material_for_request(&stable_b).unwrap()
    );
    assert_eq!(
        stable_a_value["prompt_cache_key"],
        stable_a_user_value["prompt_cache_key"]
    );
    assert_eq!(
        stable_a_value["prompt_cache_key"],
        stable_b_value["prompt_cache_key"]
    );
    assert_ne!(
        stable_a_diagnostics.stable_prompt_prefix_sha256,
        stable_b_diagnostics.stable_prompt_prefix_sha256
    );
    assert_ne!(
        stable_a_diagnostics.cacheable_prefix_sha256,
        stable_b_diagnostics.cacheable_prefix_sha256
    );
}

/// Verifies MCP integration context remains in the OpenAI stable prefix.
///
/// MCP integration summaries are configuration guidance for the model rather
/// than late controller state. Keeping them stable-prefix eligible prevents the
/// block from prematurely closing reusable input before later durable transcript
/// content is rendered.
#[test]
fn openai_stable_prefix_keeps_mcp_integration_context() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            label: "project guidance".to_string(),
            content: "use stable project style".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::Configuration,
            label: "mcp integrations".to_string(),
            content: "available_servers=1 available_tools=0 unavailable_servers=0".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            label: "assistant".to_string(),
            content: "durable assistant context after mcp".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "inspect cache reuse".to_string(),
        },
    ])
    .unwrap();

    let request = assemble_model_request(&profile, &turn(), &context).unwrap();
    let (_, stable_input) = openai_test_stable_prefix_parts(&request);
    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    let stable_input_text = serde_json::to_string(&stable_input).unwrap();

    assert!(
        stable_input_text.contains("[mcp integrations]"),
        "{stable_input_text}"
    );
    assert!(
        stable_input_text.contains("durable assistant context after mcp"),
        "{stable_input_text}"
    );
    assert!(
        stable_input.len() >= 2,
        "expected MCP context and following transcript in stable input: {stable_input:?}"
    );
    assert!(diagnostics.stable_input_bytes > 2);
    assert!(diagnostics.volatile_input_bytes > 2);
}

/// Verifies long OpenAI sessions preserve append-only stable-prefix continuity
/// until an explicit compaction changes the sequence.
///
/// Prompt caching can only reuse the prior request when the next request keeps
/// the already-rendered stable context byte-identical and appends newly durable
/// transcript material after it. This regression simulates many user/model
/// turns, rebuilds each provider request from the growing transcript, and
/// asserts that every stable input item from the previous request remains the
/// byte-for-byte leading sequence of the next request.
///
/// The current user instruction is intentionally not part of the reusable
/// prefix for its own turn. It should become durable transcript context only
/// on the following request, alongside the assistant output it produced.
#[test]
fn openai_long_session_stable_prefix_is_append_only_until_compaction() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let mut transcript = Vec::new();
    let mut previous_instructions: Option<String> = None;
    let mut previous_stable_input: Option<Vec<serde_json::Value>> = None;
    let mut previous_diagnostics: Option<crate::agent::OpenAiPromptCacheDiagnostics> = None;
    let mut initial_stable_input_len = None;

    for turn_index in 0..32 {
        let mut turn = turn();
        turn.turn_id = format!("turn-{turn_index}");
        let current_user = format!(
            "current-user-turn-{turn_index}: investigate cache continuity {}",
            "with stable transcript replay ".repeat(8)
        );
        let mut blocks = vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                label: "session identity".to_string(),
                content: "session_id=session-cache-continuity session_name=cache-test"
                    .to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                label: "project guidance".to_string(),
                content: "keep provider request prefixes byte stable".to_string(),
            },
        ];
        blocks.extend(transcript.clone());
        blocks.push(ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: current_user.clone(),
        });

        let request = assemble_model_request(&profile, &turn, &AgentContext::new(blocks).unwrap())
            .unwrap();
        let (instructions, stable_input) = openai_test_stable_prefix_parts(&request);
        let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
        let initial_len = *initial_stable_input_len.get_or_insert(stable_input.len());
        assert_eq!(
            stable_input.len(),
            initial_len + turn_index * 2,
            "turn {turn_index} should contain the stable seed plus one durable user/assistant pair per completed turn"
        );

        if let Some(previous_instructions) = &previous_instructions {
            assert_eq!(
                previous_instructions, &instructions,
                "turn {turn_index} changed stable instructions"
            );
        }
        if let Some(previous_stable_input) = &previous_stable_input {
            assert_eq!(
                stable_input.len(),
                previous_stable_input.len() + 2,
                "turn {turn_index} should append one durable user/assistant pair"
            );
            for (message_index, previous_message) in previous_stable_input.iter().enumerate() {
                let previous_bytes = serde_json::to_vec(previous_message).unwrap();
                let current_bytes = serde_json::to_vec(&stable_input[message_index]).unwrap();
                assert_eq!(
                    previous_bytes, current_bytes,
                    "turn {turn_index} changed stable input message {message_index}"
                );
            }
        }
        if let Some(previous_diagnostics) = &previous_diagnostics {
            assert_eq!(
                previous_diagnostics.prompt_cache_key, diagnostics.prompt_cache_key,
                "turn {turn_index} changed prompt cache routing key"
            );
            assert_eq!(
                previous_diagnostics.instructions_sha256, diagnostics.instructions_sha256,
                "turn {turn_index} changed cached instructions"
            );
            assert_eq!(
                previous_diagnostics.tools_sha256, diagnostics.tools_sha256,
                "turn {turn_index} changed cached tool schema"
            );
            assert_eq!(
                previous_diagnostics.tool_choice_sha256, diagnostics.tool_choice_sha256,
                "turn {turn_index} changed tool choice"
            );
            assert!(
                diagnostics.stable_input_bytes >= previous_diagnostics.stable_input_bytes,
                "turn {turn_index} shrank stable input without compaction"
            );
        }

        let stable_material = openai_stable_prefix_material_for_request(&request).unwrap();
        assert!(
            !stable_material.contains(&current_user),
            "turn {turn_index} leaked current user input into its own stable prefix"
        );
        if turn_index > 0 {
            assert!(
                stable_material.contains(&format!("current-user-turn-{}", turn_index - 1)),
                "turn {turn_index} did not append the previous user input"
            );
            assert!(
                stable_material.contains(&format!("assistant-output-turn-{}", turn_index - 1)),
                "turn {turn_index} did not append the previous assistant output"
            );
        }

        previous_instructions = Some(instructions);
        previous_stable_input = Some(stable_input);
        previous_diagnostics = Some(diagnostics);
        transcript.push(ContextBlock {
            source: ContextSourceKind::TranscriptUser,
            label: "transcript user".to_string(),
            content: current_user,
        });
        transcript.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            label: "transcript assistant".to_string(),
            content: format!(
                "assistant-output-turn-{turn_index}: cache continuity finding {}",
                "previous bytes remain immutable ".repeat(16)
            ),
        });
    }
}

/// Returns the rendered OpenAI stable-prefix instructions and input messages
/// for request-shape tests.
fn openai_test_stable_prefix_parts(request: &ModelRequest) -> (String, Vec<serde_json::Value>) {
    let material = openai_stable_prefix_material_for_request(request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&material).unwrap();
    let instructions = value["instructions"].as_str().unwrap().to_string();
    let stable_input = value["stable_input"].as_array().unwrap().clone();
    (instructions, stable_input)
}
/// Verifies historical tool transcript entries replay as ordinary provider
/// input outside the reusable stable prefix.
///
/// Historical tool output should stay available as regular context so later
/// turns can reference exact prior command evidence without routing through a
/// generated summary layer.
#[test]
fn openai_historical_tool_results_replay_outside_stable_prefix() {
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
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptUser,
                label: "transcript user".to_string(),
                content: "previous request".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "transcript assistant".to_string(),
                content: "previous answer".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                label: "historical tool result".to_string(),
                content: "action_id=action-7\ncommand: rg cache\noutput: stable evidence"
                    .to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "first follow-up".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let second = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptUser,
                label: "transcript user".to_string(),
                content: "previous request".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "transcript assistant".to_string(),
                content: "previous answer".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                label: "historical tool result".to_string(),
                content: "action_id=action-7\ncommand: rg cache\noutput: stable evidence"
                    .to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "second follow-up".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let first_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&first).unwrap()).unwrap();
    let first_input = first_body["input"].as_array().unwrap();
    let historical_tool_text = first_input
        .iter()
        .find_map(|message| {
            let text = message["content"][0]["text"].as_str()?;
            text.contains("[executed result]").then_some(text)
        })
        .expect("historical tool result should replay as ordinary input");
    assert!(historical_tool_text.contains("stable evidence"));
    assert!(historical_tool_text.contains("command: rg cache"));
    assert!(first_input.iter().any(|message| {
        message["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("[executed result]"))
    }));
    let first_prefix = openai_stable_prefix_material_for_request(&first).unwrap();
    let second_prefix = openai_stable_prefix_material_for_request(&second).unwrap();
    assert!(first_prefix.contains("[executed result]"));
    assert!(first_prefix.contains("stable evidence"));
    assert_eq!(first_prefix, second_prefix);
    let first_diagnostics = openai_prompt_cache_diagnostics_for_request(&first).unwrap();
    let second_diagnostics = openai_prompt_cache_diagnostics_for_request(&second).unwrap();
    assert!(first_diagnostics.stable_input_bytes > 2);
    assert_eq!(
        first_diagnostics.stable_input_sha256,
        second_diagnostics.stable_input_sha256
    );
    assert_ne!(
        first_diagnostics.volatile_input_sha256,
        second_diagnostics.volatile_input_sha256
    );
}
/// Verifies current-turn action results remain after the latest user request
/// while historical tool transcript entries stay reusable stable prefix
/// context.
///
/// Execution evidence for the active instruction must stay in the volatile
/// suffix so the provider sees it after the latest user request and does not
/// reuse it as immutable prefix material.
#[test]
fn openai_current_action_results_remain_volatile_suffix() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                label: "historical tool result".to_string(),
                content: "action_id=action-3\noutput: cached evidence".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "use the prior output".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: "action result".to_string(),
                content: "action_id=action-4\noutput: fresh evidence".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let input = body["input"].as_array().unwrap();
    let user_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("use the prior output"))
        })
        .expect("user instruction should be rendered into input");
    let action_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("[executed result]") && text.contains("fresh evidence"))
        })
        .expect("current action result should be rendered into input");
    assert!(action_index > user_index);
    let prefix = openai_stable_prefix_material_for_request(&request).unwrap();
    assert!(prefix.contains("[executed result]"));
    assert!(prefix.contains("cached evidence"));
    assert!(!prefix.contains("fresh evidence"));
    assert!(input.iter().any(|message| {
        message["content"][0]["text"].as_str().is_some_and(|text| {
            text.contains("[executed result]") && text.contains("cached evidence")
        })
    }));
    assert!(input.iter().any(|message| {
        message["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("[executed result]") && text.contains("fresh evidence"))
    }));
    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    assert!(diagnostics.stable_input_bytes > 2);
    assert!(diagnostics.volatile_input_bytes > 2);
}

/// Verifies active-turn read/search action results replay directly into the
/// provider request instead of being replaced with a synthetic read-ledger
/// block.
#[test]
fn openai_replays_current_turn_read_results_without_synthetic_ledger() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "Patch the overlay style helper.".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: "action result read-1".to_string(),
                content: "[action_result read-1 shell_command succeeded]\ncommand: sed -n '300,420p' src/runtime/render/overlay.rs\noutput:\nowner body".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: "action result read-2".to_string(),
                content: "[action_result read-2 shell_command succeeded]\ncommand: sed -n '1148,1238p' src/runtime/render/overlay.rs\noutput:\nhelper body".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: "action result read-3".to_string(),
                content: "[action_result read-3 shell_command succeeded]\ncommand: rg -n \"overlay style\" \"docs/reference/issue backlog.md\"\nread_observation_json: {\"kind\":\"search\",\"target\":\"docs/reference/issue backlog.md\",\"ranges\":[],\"query\":\"overlay style\"}\noutput:\n120: overlay style".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let input = body["input"].as_array().unwrap();
    let raw_results = input
        .iter()
        .filter_map(|message| message["content"][0]["text"].as_str())
        .filter(|text| text.contains("[executed result]") && text.contains("[action_result read-"))
        .collect::<Vec<_>>();

    assert_eq!(raw_results.len(), 3, "{raw_results:#?}");
    assert!(raw_results.iter().any(|text| {
        text.contains("sed -n '300,420p' src/runtime/render/overlay.rs")
            && text.contains("owner body")
    }));
    assert!(raw_results.iter().any(|text| {
        text.contains("sed -n '1148,1238p' src/runtime/render/overlay.rs")
            && text.contains("helper body")
    }));
    assert!(raw_results.iter().any(|text| {
        text.contains("rg -n \"overlay style\" \"docs/reference/issue backlog.md\"")
            && text.contains("120: overlay style")
    }));
    let synthetic_summary = input
        .iter()
        .filter_map(|message| message["content"][0]["text"].as_str())
        .any(|text| {
            text.contains("Recent successful read/search coverage for this active turn.")
        });
    assert!(!synthetic_summary);
}

/// Verifies a long OpenAI session keeps already-observed action results raw
/// instead of rewriting them into committed summaries during ordinary request
/// assembly.
///
/// Ordinary continuation should preserve already-observed evidence byte for
/// byte. Compaction remains the only path that may rewrite old history.
#[test]
fn openai_long_session_keeps_observed_action_results_raw_without_committed_evidence() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let mut blocks = Vec::new();
    for index in 0..12 {
        let stable_seed = format!("provider-doc-{index}-title");
        blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptUser,
            label: format!("historical user {index}"),
            content: format!("Read provider document {index}."),
        });
        blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            label: format!("historical assistant plan {index}"),
            content: format!("I will fetch provider document {index}."),
        });
        blocks.push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: format!("fetch result {index}"),
            content: format!(
                "[action_result fetch-{index} fetch_url succeeded]\n\
                 content:\n\
                 summary_seed={stable_seed}; provider document {index} confirms tool-calling support. {}\n\
                 RAW_DETAIL_SHOULD_NOT_BE_REPLAYED_{index}",
                "stable filler ".repeat(30)
            ),
        });
        blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            label: format!("historical assistant observed {index}"),
            content: format!("I observed fetch-{index} and can use {stable_seed}."),
        });
    }
    blocks.push(ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "Compare the provider evidence and continue from the current fetch.".to_string(),
    });
    blocks.push(ContextBlock {
        source: ContextSourceKind::ActionResult,
        label: "current fetch result".to_string(),
        content: "[action_result fetch-current fetch_url succeeded]\ncontent:\nCURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE".to_string(),
    });

    let request =
        assemble_model_request(&profile, &turn(), &AgentContext::new(blocks).unwrap()).unwrap();

    assert!(!request
        .messages
        .iter()
        .any(|message| message.source == ContextSourceKind::CommittedEvidence));
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult && message.content.contains("fetch-0")
    }));
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|message| message.source == ContextSourceKind::ActionResult)
            .count(),
        13
    );
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("CURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE")
    }));

    let prefix = openai_stable_prefix_material_for_request(&request).unwrap();
    assert!(!prefix.contains("[committed_evidence]"));
    assert!(!prefix.contains("RAW_DETAIL_SHOULD_NOT_BE_REPLAYED_0"));

    let body_text = openai_responses_request_body(&request).unwrap();
    assert!(body_text.contains("CURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE"));
    assert!(body_text.contains("RAW_DETAIL_SHOULD_NOT_BE_REPLAYED_0"));
    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    let input = body["input"].as_array().unwrap();
    let user_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"].as_str().is_some_and(|text| {
                text.contains("Compare the provider evidence and continue from the current fetch.")
            })
        })
        .expect("current user instruction should be rendered into input");
    let current_result_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("CURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE"))
        })
        .expect("current action result should be rendered into input");
    assert!(current_result_index > user_index);

    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    assert!(diagnostics.stable_input_bytes > 2);
    assert!(diagnostics.volatile_input_bytes > 2);
}

/// Verifies volatile controller state remains out of OpenAI `instructions` and
/// out of the stable input prefix.
///
/// Dynamic capability decisions are authoritative controller context, but
/// rendering them at the front of the prompt would invalidate cache reuse for
/// otherwise identical follow-up requests. They should stay model-visible as
/// late developer input.
#[test]
fn openai_dynamic_controller_state_is_late_developer_input() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let mut request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "inspect the repo".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.messages.push(super::ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::DeveloperInstruction,
        content: "[capability shell]\ncapability=shell\nallowed_actions=say,shell_command"
            .to_string(),
    });

    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let instructions = body["instructions"].as_str().unwrap();
    assert!(!instructions.contains("[capability shell]"));
    let input = body["input"].as_array().unwrap();
    assert!(input.iter().any(|message| {
        message["role"] == "developer"
            && message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("[capability shell]"))
    }));

    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    assert!(diagnostics.volatile_input_bytes > 2);
    assert_eq!(diagnostics.stable_input_bytes, 2);
}

/// Verifies OpenAI prompt-cache diagnostics expose request fingerprints without
/// adding any diagnostic text to model-visible context.
///
/// Trace and status surfaces can use these hashes to explain cache misses while
/// preserving the exact provider prompt shape sent for inference.
#[test]
fn openai_prompt_cache_diagnostics_fingerprint_provider_prefix_parts() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                label: "active repository instructions".to_string(),
                content: "run just test".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "fix cache hits".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();

    assert!(
        body["instructions"]
            .as_str()
            .unwrap()
            .contains("run just test")
    );
    assert!(!body["input"].as_array().unwrap().iter().any(|message| {
        message["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("run just test"))
    }));
    assert!(diagnostics.prompt_cache_key.starts_with("mez-"));
    assert_eq!(diagnostics.prompt_cache_key.len(), "mez-".len() + 32);
    assert!(diagnostics.instructions_bytes > 1024);
    assert_eq!(diagnostics.instructions_sha256.len(), 64);
    assert!(diagnostics.response_format_bytes > 0);
    assert_eq!(diagnostics.response_format_sha256.len(), 64);
    assert!(diagnostics.tools_bytes > 2);
    assert_eq!(diagnostics.tools_sha256.len(), 64);
    assert_eq!(diagnostics.stable_input_bytes, 2);
    assert_eq!(diagnostics.stable_input_sha256.len(), 64);
    assert!(diagnostics.volatile_input_bytes > 2);
    assert_eq!(diagnostics.volatile_input_sha256.len(), 64);
    assert!(diagnostics.stable_prompt_prefix_bytes > diagnostics.instructions_bytes);
    assert_eq!(diagnostics.stable_prompt_prefix_sha256.len(), 64);
    assert_eq!(
        diagnostics.cacheable_prefix_bytes,
        diagnostics.stable_prompt_prefix_bytes
    );
    assert_eq!(
        diagnostics.cacheable_prefix_sha256,
        diagnostics.stable_prompt_prefix_sha256
    );
    assert!(diagnostics.provider_request_shape_bytes > diagnostics.tools_bytes);
    assert_eq!(diagnostics.provider_request_shape_sha256.len(), 64);
    assert!(diagnostics.cacheable_prefix_bytes > diagnostics.instructions_bytes);
    assert_eq!(diagnostics.cacheable_prefix_sha256.len(), 64);
}

/// Verifies a representative OpenAI Responses request has stable canonical
/// request-shape fixture values for cache diagnostics.
///
/// This covers the provider-visible request pieces that affect cache affinity:
/// instructions, prompt-cache routing key, stable prefix material, tools,
/// forced tool choice, response-format shape, and the aggregate provider
/// request-shape fingerprint. Exact values are intentionally pinned so schema
/// or request-shape drift is reviewed instead of silently fragmenting cache
/// reuse.
#[test]
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
    assert_eq!(body["prompt_cache_retention"], "24h");
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], false);
    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(body["tool_choice"]["name"], "submit_maap_action_batch");
    assert!(body["text"]["format"].is_null());
    assert!(body["instructions"]
        .as_str()
        .unwrap()
        .contains("Prefer deterministic request shapes."));
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

    assert_eq!(diagnostics.prompt_cache_key, "mez-fcc0c3076055b2040cb8727ead0dbe7c");
    assert_eq!(diagnostics.instructions_bytes, 44_554);
    assert_eq!(diagnostics.instructions_sha256, "de520b823e239da6ef5ba16e2175afeb7341961c1cc1137a81374ac313d0e937");
    assert_eq!(diagnostics.response_format_bytes, 4);
    assert_eq!(diagnostics.response_format_sha256, "74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b");
    assert_eq!(diagnostics.tools_bytes, 27_281);
    assert_eq!(diagnostics.tools_sha256, "3fe8c23aa136005b8114ec893aa89f1f9cff6cf39689dc0a7654096fa4dbfff0");
    assert_eq!(diagnostics.tool_choice_bytes, 53);
    assert_eq!(diagnostics.tool_choice_sha256, "6667323a2b74449448aad3d609d98e5288910331b10d71e6f482da3e076eab4e");
    assert_eq!(diagnostics.stable_prompt_prefix_bytes, 44_715);
    assert_eq!(diagnostics.stable_prompt_prefix_sha256, "cbc04cc7d90997b2c8c8c8a0d4e2f05d87e466ed0d47c4edefa68d3eb582b07f");
    assert_eq!(diagnostics.provider_request_shape_bytes, 27_573);
    assert_eq!(diagnostics.provider_request_shape_sha256, "2f89d651ae06b554c8185872372a0a3a78d01bc49244cc5db2ba4a77b8e70189");
}

/// Verifies OpenAI Responses request bodies carry the selected reasoning effort
/// through the provider-specific `reasoning` field. This protects automatic
/// reasoning and explicit model picker selections from silently dropping the
/// configured reasoning level.
#[test]
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
    assert_eq!(value["prompt_cache_retention"], "24h");
}

/// Verifies OpenAI Responses request bodies do not serialize the configured
/// output-token cap even when retries raise `ModelRequest.max_output_tokens`.
/// OpenAI rejects the legacy wire field, so recovery must adjust provider
/// behavior without emitting `max_output_tokens` on the Responses path.
#[test]
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

/// Builds a minimal OpenAI request for prompt-cache retention tests.
fn openai_prompt_cache_retention_test_request(model: &str) -> ModelRequest {
    assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: model.to_string(),
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
            content: "debug this failing test".to_string(),
        }])
        .unwrap(),
    )
    .unwrap()
}

/// Verifies supported OpenAI models default to extended prompt-cache retention.
///
/// Omitting the field should still request `24h` for model families where the
/// provider supports extended prompt-cache retention so stable prefixes can be
/// reused across turns and sessions without profile boilerplate.
#[test]
fn openai_responses_request_body_defaults_supported_models_to_extended_retention() {
    let request = openai_prompt_cache_retention_test_request("gpt-5.4");

    let body = openai_responses_request_body(&request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(value["prompt_cache_retention"], "24h");
}

/// Verifies OpenAI prompt-cache diagnostics include implicit extended retention.
///
/// Diagnostics must fingerprint the emitted provider request shape, including
/// the provider-visible `24h` default for supported model families.
#[test]
fn openai_prompt_cache_diagnostics_include_implicit_extended_retention() {
    let implicit = openai_prompt_cache_retention_test_request("gpt-5.4");
    let explicit_unsupported = {
        let mut request = openai_prompt_cache_retention_test_request("gpt-5.4");
        request.prompt_cache_retention = Some("in_memory".to_string());
        request
    };

    let implicit_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&implicit).unwrap()).unwrap();
    assert_eq!(implicit_body["prompt_cache_retention"], "24h");
    assert!(openai_responses_request_body(&explicit_unsupported).is_err());

    let diagnostics = openai_prompt_cache_diagnostics_for_request(&implicit).unwrap();
    assert!(diagnostics.provider_request_shape_bytes > 2);
}

/// Verifies explicit in-memory prompt-cache retention is rejected for current
/// and future model families whose provider default is extended retention.
#[test]
fn openai_responses_request_body_rejects_unsupported_in_memory_prompt_cache_retention() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-5.5");
    request.prompt_cache_retention = Some("in_memory".to_string());

    let error = openai_responses_request_body(&request).unwrap_err();

    assert!(error.to_string().contains("in_memory"), "{error}");
    assert!(error.to_string().contains("gpt-5.5"), "{error}");
}

/// Verifies extended prompt-cache retention is accepted for current documented
/// OpenAI model families, including the built-in default model family.
#[test]
fn openai_responses_request_body_accepts_current_extended_prompt_cache_retention_models() {
    for model in [
        "gpt-5.5",
        "gpt-5.5-pro",
        "gpt-5.4",
        "gpt-5.2",
        "gpt-5.1-codex-max",
    ] {
        let mut request = openai_prompt_cache_retention_test_request(model);
        request.prompt_cache_retention = Some("24h".to_string());

        let body = openai_responses_request_body(&request).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(value["prompt_cache_retention"], "24h", "{model}");
    }
}

/// Verifies extended prompt-cache retention is rejected for model families
/// without documented support.
#[test]
fn openai_responses_request_body_rejects_unsupported_extended_prompt_cache_retention() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-5.4-mini");
    request.prompt_cache_retention = Some("24h".to_string());

    let error = openai_responses_request_body(&request).unwrap_err();

    assert!(error.to_string().contains("24h"), "{error}");
    assert!(error.to_string().contains("gpt-5.4-mini"), "{error}");
}

/// Verifies OpenAI prompt-cache retention is constrained to documented values.
#[test]
fn openai_responses_request_body_rejects_invalid_prompt_cache_retention() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-test");
    request.prompt_cache_retention = Some("forever".to_string());

    let error = openai_responses_request_body(&request).unwrap_err();

    assert!(
        error.to_string().contains("prompt_cache_retention"),
        "{error}"
    );
}

/// Verifies auto-sizing requests use a separate structured-output schema and
/// never expose normal action tools. The router response is an internal
/// decision object rather than a MAAP action batch.
#[test]
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
            label: "user".to_string(),
            content: "classify this task".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::AutoSizing;
    request.allowed_actions = crate::agent::AllowedActionSet::say_only();
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

/// Verifies assistant transcript context is serialized with an assistant role.
///
/// Prior assistant messages are not new user instructions. The Responses
/// request body must preserve their role so follow-up references resolve
/// against chat history instead of a flattened user transcript block.
#[test]
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

/// Verifies prior user transcript entries are marked as inactive history.
///
/// Large context windows can contain earlier user prompts that would be valid
/// standalone requests. The OpenAI renderer must keep those prompts available
/// for references while clearly separating them from the current active task.
#[test]
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
    assert!(historical_text.contains("earlier user prompts are historical context only"));
    assert!(historical_text.contains("Output a large multiline JSON object"));

    assert_eq!(input[1]["role"], "user");
    let current_text = input[1]["content"][0]["text"].as_str().unwrap();
    assert!(current_text.contains("[user prompt transcript entry]"));
    assert!(current_text.contains("The latest user prompt is the active task"));
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

/// Verifies openai responses request body exposes the current executable
/// action schema through one canonical tool.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
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
            label: "user".to_string(),
            content: "Create random test data".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Shell);

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
    let allowed_surface = value["input"].as_array().unwrap().last().unwrap()["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(allowed_surface.contains("[allowed action surface]"));
    assert!(allowed_surface.contains("interaction_kind=action_execution"));
    assert!(
        allowed_surface.contains("allowed_actions=say,request_capability,shell_command,apply_patch")
    );
    assert!(allowed_surface.contains("active_function_tool=submit_maap_action_batch"));
    assert!(allowed_surface.contains("Emit only action objects whose type appears"));
    assert!(
        !allowed_surface.contains("one canonical MAAP action-batch function"),
        "{allowed_surface}"
    );
    assert!(
        !allowed_surface.contains("required-function-call"),
        "{allowed_surface}"
    );
    assert!(
        !allowed_surface.contains("Model-selected skill lookup/loading is disabled"),
        "{allowed_surface}"
    );

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

/// Verifies uncommon composite capability grants still get provider-enforced
/// current-schema narrowing instead of falling back to an all-action MAAP
/// schema.
///
/// Multiple request_capability actions can be granted in one continuation. The
/// canonical function for this request must expose exactly the composite
/// surface.
#[test]
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
            label: "user".to_string(),
            content: "inspect locally and fetch a URL".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    let mut allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Shell);
    allowed_actions.extend_set(&crate::agent::AllowedActionSet::for_capability(
        crate::agent::AgentCapability::NetworkFetch,
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
    assert!(!action_types.contains(&"mcp_call".to_string()));
    assert!(action_types.contains(&"spawn_agent".to_string()));
}

/// Verifies available MCP tools keep the unified current action surface.
///
/// A matching MCP tool should be directly callable through the regular action
/// schema and manifest, but it must not remove memory as a usable feature.
/// Provider guidance and runtime guardrails handle placeholder memory behavior
/// without hiding the action.
#[test]
fn openai_available_mcp_keeps_memory_on_default_surface() {
    let mcp_tool = McpPromptTool {
        server_id: "githubcopilot".to_string(),
        tool_name: "list_ci_results".to_string(),
        description: "Read GitHub CI check results for a repository. User-configured non-authoritative server purpose: GitHub repository and CI operations.".to_string(),
        approval_required: false,
        input_schema_json: r#"{"type":"object","properties":{"repo":{"type":"string"}}}"#
            .to_string(),
    };
    let context = crate::agent::append_mcp_context(
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "use the githubcopilot mcp server to pull the latest CI results".to_string(),
        }])
        .unwrap(),
        &crate::mcp::McpPromptSummary {
            available_servers: vec![crate::mcp::McpPromptServer {
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
    crate::agent::apply_default_action_gates(&mut request, &[mcp_tool], true, false);

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
        description.contains("Available MCP servers callable with mcp_call: githubcopilot (GitHub repository and CI operations; tools: list_ci_results)"),
        "{description}"
    );
    assert!(
        description.contains("Available MCP tools callable with mcp_call: githubcopilot/list_ci_results"),
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

/// Verifies openai responses request body uses mcp tool argument schemas.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
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
            label: "user".to_string(),
            content: "read a file".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Mcp);
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
        description.contains("Available MCP servers callable with mcp_call: fs (tools: read_file); zeta (tools: later)"),
        "{description}"
    );
    assert!(
        description.contains("Available MCP tools callable with mcp_call: fs/read_file: Read file"),
        "{description}"
    );
    let action_schemas = openai_tool_action_schemas(mcp_tool);
    let mcp_schemas = action_schemas
        .iter()
        .filter(|schema| schema["properties"]["type"]["enum"][0] == "mcp_call")
        .collect::<Vec<_>>();

    assert_eq!(action_schemas.len(), 17);
    let action_types = openai_tool_action_types(mcp_tool);
    assert!(!action_types.contains(&"request_skills".to_string()));
    assert!(!action_types.contains(&"call_skill".to_string()));
    assert_eq!(mcp_schemas.len(), 2);
    assert_eq!(mcp_schemas[0]["properties"]["server"]["enum"][0], "fs");
    assert_eq!(mcp_schemas[0]["properties"]["tool"]["enum"][0], "read_file");
    assert!(
        mcp_schemas[0]["description"]
            .as_str()
            .unwrap()
            .contains("Call MCP tool fs/read_file. Description: Read file"),
        "{}",
        mcp_schemas[0]
    );
    assert!(
        mcp_schemas[0]["description"]
            .as_str()
            .unwrap()
            .contains("use this as a direct action"),
        "{}",
        mcp_schemas[0]
    );
    assert!(
        mcp_schemas[0]["properties"]["tool"]["description"]
            .as_str()
            .unwrap()
            .contains("Tool description: Read file"),
        "{}",
        mcp_schemas[0]
    );
    assert!(
        mcp_schemas[0]["properties"]["arguments"]["description"]
            .as_str()
            .unwrap()
            .contains(
                "Use this action when the task matches this tool description or the user named this MCP server/tool"
            ),
        "{}",
        mcp_schemas[0]
    );
    assert_eq!(mcp_schemas[1]["properties"]["server"]["enum"][0], "zeta");
    assert_eq!(mcp_schemas[1]["properties"]["tool"]["enum"][0], "later");
    assert_eq!(
        mcp_schemas[0]["properties"]["arguments"]["properties"]["path"]["type"],
        "string"
    );
    assert_eq!(
        mcp_schemas[0]["properties"]["arguments"]["required"][0],
        "path"
    );
    assert_eq!(
        mcp_schemas[0]["properties"]["arguments"]["additionalProperties"],
        false
    );
}

/// Verifies large MCP catalogs keep server-level routing context visible.
///
/// The OpenAI function-tool description is the first routing surface the model
/// sees for callable MCP integrations. When there are more callable tools than
/// the compact tool list can enumerate, the schema should still provide a
/// bounded server-level summary so overlapping tool names retain their server
/// purpose and routing context.
#[test]
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
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Mcp);
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

/// Verifies the provider-facing schema describes the patch formats accepted by
/// Mezzanine's shell-backed patch executor.
///
/// The JSON schema is the strongest action-specific hint available to models
/// using native function/tool calls, so it should tell them to emit the single
/// supported Mezzanine patch block format.
#[test]
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
            label: "user".to_string(),
            content: "edit a file".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::Shell);

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
        description.contains("Accepted file directives are exactly *** Add File, *** Update File, *** Delete File"),
        "{description}"
    );
    assert!(
        description.contains("there is no *** Replace File directive"),
        "{description}"
    );
    assert!(
        description.contains("For whole-file replacement, use an Update File hunk headed @@ replace whole file"),
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

/// Verifies the provider-facing config-change schema exposes live config
/// mutation guidance instead of leaving the model to guess free-form paths.
///
/// This matters because `config_change` applies privileged runtime settings,
/// so the model needs path patterns, value encoding, and operation constraints
/// before it can propose a valid mutation.
#[test]
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
            label: "user".to_string(),
            content: "change the active theme".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.interaction_kind = crate::agent::ModelInteractionKind::ActionExecution;
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::ConfigChange);

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
    assert!(
        path_description.contains("Supported patterns"),
        "{path_description}"
    );
    assert!(
        path_description.contains("theme.active"),
        "{path_description}"
    );
    assert!(
        path_description.contains("model_profiles.<name>.<key>"),
        "{path_description}"
    );
    assert!(
        path_description.contains("mcp_servers.<name>.<key>"),
        "{path_description}"
    );
    assert!(
        path_description.contains("mcp_servers.<name>.external_capability.usage_instructions"),
        "{path_description}"
    );
    assert!(
        path_description.contains("mcp_servers.<name>.external_capability.purpose"),
        "{path_description}"
    );
    assert!(
        path_description.contains("mutates_filesystem_outside_shell"),
        "{path_description}"
    );
    assert!(
        path_description.contains("Runtime validation still rejects secrets"),
        "{path_description}"
    );
    assert!(
        path_description.contains("Schema annotations"),
        "{path_description}"
    );
    assert!(
        path_description.contains("purpose=Switch the active"),
        "{path_description}"
    );
    assert!(
        path_description.contains("value_type=string"),
        "{path_description}"
    );
    assert!(
        path_description.contains("format=`<alias>` is an alias name"),
        "{path_description}"
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

/// Verifies openai provider posts responses request, parses output text, and
/// exposes provider token and quota usage metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn openai_provider_posts_responses_request_and_parses_output_text() {
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
            headers: std::collections::BTreeMap::from([
                ("x-ratelimit-limit-requests".to_string(), "100".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "75".to_string(),
                ),
                ("x-ratelimit-reset-requests".to_string(), "10s".to_string()),
                ("x-ratelimit-limit-tokens".to_string(), "200".to_string()),
                (
                    "x-ratelimit-remaining-tokens".to_string(),
                    "100".to_string(),
                ),
            ]),
            body: serde_json::json!({
                "model": "gpt-test",
                "usage": {
                    "input_tokens": 42,
                    "output_tokens": 11,
                    "input_tokens_details": {
                        "cached_tokens": 30
                    },
                    "output_tokens_details": {
                        "reasoning_tokens": 7
                    }
                },
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "hello back"
                    }]
                }]
            })
            .to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.provider, "openai");
    assert_eq!(response.model, "gpt-test");
    assert_eq!(response.raw_text, "hello back");
    assert_eq!(response.usage.input_tokens, 42);
    assert_eq!(response.usage.output_tokens, 11);
    assert_eq!(response.usage.reasoning_tokens, 7);
    assert_eq!(response.usage.cached_input_tokens, Some(30));
    assert_eq!(response.quota_usage.len(), 2);
    let requests_quota = response
        .quota_usage
        .iter()
        .find(|quota| quota.name == "requests")
        .unwrap();
    assert_eq!(requests_quota.used_percent_display(), "25.00%");
    assert_eq!(requests_quota.reset.as_deref(), Some("10s"));
    let tokens_quota = response
        .quota_usage
        .iter()
        .find(|quota| quota.name == "tokens")
        .unwrap();
    assert_eq!(tokens_quota.used_percent_display(), "50.00%");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].url, "https://example.test/responses");
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer test-key")
    );
}

/// Verifies cached-token accounting distinguishes omitted provider fields from
/// an explicit provider-reported zero.
#[test]
fn openai_response_parser_distinguishes_missing_and_zero_cached_tokens() {
    let missing_body = serde_json::json!({
        "model": "gpt-test",
        "usage": {
            "input_tokens": 42,
            "output_tokens": 11
        },
        "output_text": "ok"
    })
    .to_string();
    let zero_body = serde_json::json!({
        "model": "gpt-test",
        "usage": {
            "input_tokens": 42,
            "output_tokens": 11,
            "input_tokens_details": {
                "cached_tokens": 0
            }
        },
        "output_text": "ok"
    })
    .to_string();
    let prompt_details_body = serde_json::json!({
        "model": "gpt-test",
        "usage": {
            "prompt_tokens": 42,
            "completion_tokens": 11,
            "prompt_tokens_details": {
                "cached_tokens": 24
            }
        },
        "output_text": "ok"
    })
    .to_string();
    let controller_alias_body = serde_json::json!({
        "model": "gpt-test",
        "usage": {
            "input_tokens": 42,
            "output_tokens": 11,
            "cached_tokens": 0,
            "cached_input_tokens": 36
        },
        "output_text": "ok"
    })
    .to_string();
    let multi_cached_body = serde_json::json!({
        "model": "gpt-test",
        "usage": {
            "input_tokens": 42,
            "output_tokens": 11,
            "input_tokens_details": {
                "cached_tokens": 12
            },
            "prompt_tokens_details": {
                "cached_tokens": 8
            },
            "cached_input_tokens": 5
        },
        "output_text": "ok"
    })
    .to_string();
    let stream_body = format!(
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
            "response": {
                "id": "resp_1",
                "model": "gpt-test",
                "usage": {
                    "input_tokens": 42,
                    "output_tokens": 11,
                    "input_tokens_details": {
                        "cached_tokens": 12
                    }
                }
            }
        })
    );

    let (_, _, missing_usage) =
        parse_openai_responses_http_body(&missing_body, "gpt-test").unwrap();
    let (_, _, zero_usage) = parse_openai_responses_http_body(&zero_body, "gpt-test").unwrap();
    let (_, _, prompt_details_usage) =
        parse_openai_responses_http_body(&prompt_details_body, "gpt-test").unwrap();
    let (_, _, controller_alias_usage) =
        parse_openai_responses_http_body(&controller_alias_body, "gpt-test").unwrap();
    let (_, _, multi_cached_usage) =
        parse_openai_responses_http_body(&multi_cached_body, "gpt-test").unwrap();
    let (_, _, stream_usage) =
        super::provider::parse_openai_responses_stream_body(&stream_body, "gpt-test").unwrap();

    assert_eq!(missing_usage.cached_input_tokens, None);
    assert_eq!(missing_usage.cached_input_tokens_display(), "unknown");
    assert_eq!(missing_usage.cached_input_hit_ratio_display(), "unknown");
    assert_eq!(zero_usage.cached_input_tokens, Some(0));
    assert_eq!(zero_usage.cached_input_tokens_display(), "0");
    assert_eq!(zero_usage.cached_input_hit_ratio_display(), "0.00%");
    assert_eq!(prompt_details_usage.cached_input_tokens, Some(24));
    assert_eq!(
        prompt_details_usage.cached_input_hit_ratio_display(),
        "57.14%"
    );
    assert_eq!(controller_alias_usage.cached_input_tokens, Some(36));
    assert_eq!(multi_cached_usage.cached_input_tokens, Some(12));
    assert_eq!(stream_usage.cached_input_tokens, Some(12));
}

/// Verifies that the async OpenAI provider path issues the same Responses API
/// request shape while awaiting the async HTTP transport instead of using the
/// blocking transport trait.
#[tokio::test]
async fn openai_provider_async_posts_responses_request_and_parses_output_text() {
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
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"model":"gpt-test","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello async"}]}]}"#
                .to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let response = provider.send_request_async(&request).await.unwrap();

    assert_eq!(response.provider, "openai");
    assert_eq!(response.model, "gpt-test");
    assert_eq!(response.raw_text, "hello async");
    let sent = provider.transport.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].url, "https://example.test/responses");
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer test-key")
    );
}

/// Verifies that the fallback parser extracts the one required fenced
/// `mezzanine-action-json` block and maps its JSON schema into MAAP structs.
#[test]
fn fenced_maap_parser_extracts_shell_action_batch() {
    let raw_text = r#"I will inspect the workspace.
```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "test action batch rationale",
  "actions": [
    {
      "id": "a1",
      "type": "shell_command",
      "rationale": "List files",
      "summary": "List files in the current directory",
      "command": "ls",
      "interactive": false,
      "stateful": false,
      "timeout_ms": null
    }
  ],
  "final": false
}
```
"#;

    let batch = parse_fenced_maap_action_batch(raw_text).unwrap().unwrap();

    assert_eq!(batch.protocol, "maap/1");
    assert_eq!(batch.turn_id, "turn-1");
    assert!(!batch.final_turn);
    assert_eq!(batch.actions.len(), 1);
    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand {
            command,
            timeout_ms,
            ..
        } => {
            assert_eq!(command, "ls");
            assert_eq!(*timeout_ms, None);
        }
        payload => panic!("unexpected payload: {payload:?}"),
    }
}

/// Verifies that fallback model output is rejected when it contains multiple
/// action blocks, since the spec requires exactly one fenced MAAP batch.
#[test]
fn fenced_maap_parser_rejects_multiple_action_blocks() {
    let raw_text = "```mezzanine-action-json\n{}\n```\n```mezzanine-action-json\n{}\n```";

    let error = parse_fenced_maap_action_batch(raw_text).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("exactly one"),
        "{}",
        error.message()
    );
}

/// Verifies that fallback parsing still rejects action objects missing the
/// compact common MAAP fields instead of inventing action types for the model.
#[test]
fn fenced_maap_parser_rejects_missing_required_action_fields() {
    let raw_text = r#"```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "test action batch rationale",
  "actions": [
    {
      "id": "say-1",
      "text": "hello"
    }
  ],
  "final": true
}
```"#;

    let error = parse_fenced_maap_action_batch(raw_text).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("type"), "{}", error.message());
}

/// Verifies that the OpenAI text adapter preserves the raw text while also
/// parsing a fenced MAAP fallback block into the response action batch.
#[test]
fn openai_provider_parses_fenced_maap_action_batch_from_text() {
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let raw_text = r#"```mezzanine-action-json
{
  "protocol": "maap/1",
  "turn_id": "turn-1",
  "agent_id": "agent-1",
  "rationale": "test action batch rationale",
  "actions": [
    {
      "id": "say-1",
      "type": "say",
      "status": "final",
      "rationale": "Reply",
      "text": "hello"
    }
  ],
  "final": true
}
```"#;
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "gpt-test",
                "output_text": raw_text
            })
            .to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.raw_text, raw_text);
    let batch = response.action_batch.unwrap();
    assert!(batch.final_turn);
    assert_eq!(batch.actions[0].id, "action-1");
    assert!(matches!(
        batch.actions[0].payload,
        AgentActionPayload::Say { .. }
    ));
}

/// Verifies that provider-native Responses structured output is parsed
/// directly as a MAAP action batch before the fenced fallback path is needed.
#[test]
fn openai_provider_parses_native_structured_maap_action_batch() {
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let raw_text = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "type": "say",
                "status": "final",
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
                "model": "gpt-test",
                "output_text": raw_text
            })
            .to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let response = provider.send_request(&request).unwrap();

    let batch = response.action_batch.unwrap();
    assert_eq!(batch.protocol, "maap/1");
    assert_eq!(batch.turn_id, "turn-1");
    assert_eq!(batch.agent_id, "agent-1");
    assert!(batch.final_turn);
    assert_eq!(batch.actions[0].id, "action-1");
    assert!(matches!(
        batch.actions[0].payload,
        AgentActionPayload::Say { .. }
    ));
}

/// Verifies that the OpenAI Responses function-calling path is treated as the
/// primary executable-action transport. The model returns function-call
/// `arguments` as a JSON string, and Mezzanine parses those arguments as the
/// MAAP batch instead of waiting for assistant text output.
#[test]
fn openai_provider_parses_maap_function_call_arguments() {
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
            content: "list files".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let arguments = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "type": "shell_command",
                "summary": "List files in the current directory",
                "command": "ls"
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
                "model": "gpt-test",
                "output": [
                    {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": "submit_maap_action_batch",
                        "arguments": arguments
                    }
                ]
            })
            .to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    let response = provider.send_request(&request).unwrap();

    let batch = response.action_batch.unwrap();
    assert_eq!(batch.protocol, "maap/1");
    assert_eq!(batch.turn_id, "turn-1");
    assert_eq!(batch.agent_id, "agent-1");
    assert!(!batch.final_turn);
    assert_eq!(batch.actions.len(), 1);
    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand {
            command,
            interactive,
            stateful,
            timeout_ms,
            ..
        } => {
            assert_eq!(command, "ls");
            assert!(!interactive);
            assert!(!stateful);
            assert_eq!(*timeout_ms, None);
        }
        payload => panic!("unexpected payload: {payload:?}"),
    }
}

/// Verifies that ChatGPT-backed streaming Responses function-call events are
/// normalized into the same MAAP batch shape as non-streaming API responses.
/// The stream parser needs to aggregate argument deltas, because browser/device
/// auth routes through the streaming Codex backend.
#[test]
fn openai_provider_stream_parses_maap_function_call_arguments() {
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
            content: "list files".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let arguments = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "type": "shell_command",
                "summary": "List files in the current directory",
                "command": "ls"
            }
        ]
    })
    .to_string();
    let split_at = arguments.len() / 2;
    let first = &arguments[..split_at];
    let second = &arguments[split_at..];
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: format!(
                "event: response.output_item.added\ndata: {}\n\nevent: response.function_call_arguments.delta\ndata: {}\n\nevent: response.function_call_arguments.delta\ndata: {}\n\nevent: response.function_call_arguments.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                        "arguments": ""
                    }
                }),
                serde_json::json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": 0,
                    "delta": first
                }),
                serde_json::json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": 0,
                    "delta": second
                }),
                serde_json::json!({
                    "type": "response.function_call_arguments.done",
                    "output_index": 0,
                    "item": {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                        "arguments": arguments
                    }
                }),
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "resp_1", "model": "gpt-test"}
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

    let response = provider.send_request(&request).unwrap();

    let batch = response.action_batch.unwrap();
    assert!(!batch.final_turn);
    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand { command, .. } => assert_eq!(command, "ls"),
        payload => panic!("unexpected payload: {payload:?}"),
    }
}

/// Verifies cumulative streaming function-call argument snapshots replace the
/// previous buffer instead of appending forever.
///
/// Some ChatGPT-backed streaming paths send the complete argument prefix in
/// each `delta` event. Treating those as true append-only deltas can grow
/// memory indefinitely and eventually produce invalid duplicated MAAP JSON.
#[test]
fn openai_provider_stream_replaces_cumulative_function_call_argument_snapshots() {
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
            content: "list files".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let arguments = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "type": "shell_command",
                "summary": "List files in the current directory",
                "command": "ls"
            }
        ]
    })
    .to_string();
    let prefix = &arguments[..arguments.len() / 2];
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: format!(
                "event: response.output_item.added\ndata: {}\n\nevent: response.function_call_arguments.delta\ndata: {}\n\nevent: response.function_call_arguments.delta\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                        "arguments": ""
                    }
                }),
                serde_json::json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": 0,
                    "delta": prefix
                }),
                serde_json::json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": 0,
                    "delta": arguments
                }),
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "resp_1", "model": "gpt-test"}
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

    let response = provider.send_request(&request).unwrap();

    let batch = response.action_batch.unwrap();
    assert_eq!(batch.actions.len(), 1);
    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand { command, .. } => assert_eq!(command, "ls"),
        payload => panic!("unexpected payload: {payload:?}"),
    }
}

/// Verifies that malformed provider-native structured MAAP output is rejected
/// rather than being silently treated as ordinary assistant prose.
#[test]
fn openai_provider_rejects_malformed_native_structured_maap_action_batch() {
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "gpt-test",
                "output_text": "{\"protocol\":\"maap/1\",\"actions\":[]}"
            })
            .to_string(),
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

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert_eq!(
        error.provider_raw_text(),
        Some("{\"protocol\":\"maap/1\",\"actions\":[]}")
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["type"], "malformed_model_output");
    assert_eq!(failure_json["output"]["format"], "json");
    let keys = failure_json["output"]["top_level_keys"].as_array().unwrap();
    assert!(keys.contains(&serde_json::json!("actions")));
    assert!(keys.contains(&serde_json::json!("protocol")));
    assert!(
        error
            .message()
            .contains("provider MAAP output is malformed"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("at least one action"),
        "{}",
        error.message()
    );
}

/// Verifies that action-like JSON which is not a MAAP batch produces a specific
/// diagnostic. This covers models or provider endpoints that return a bare
/// command object instead of using the negotiated MAAP function-call or
/// structured-output envelope.
#[test]
fn openai_provider_diagnoses_bare_command_json_as_malformed_model_output() {
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
            content: "list files".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: serde_json::json!({
                "model": "gpt-test",
                "output_text": "{\"command\":\"ls\"}"
            })
            .to_string(),
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

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("bare command object"),
        "{}",
        error.message()
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["type"], "malformed_model_output");
    assert_eq!(failure_json["output"]["bare_command_object"], true);
}

/// Verifies that a batch-shaped response with incomplete command actions is
/// diagnosed as malformed model output. This is the common failure shape when a
/// model returns `{"rationale":"test action batch rationale","actions":[{"command":"ls"}]}` instead of a complete MAAP
/// action batch.
#[test]
fn openai_provider_diagnoses_bare_command_actions_as_malformed_model_output() {
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
            content: "list files".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let malformed = serde_json::json!({
        "rationale": "test action batch rationale",
        "actions": [
            {
                "command": "ls"
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
                "model": "gpt-test",
                "output_text": malformed
            })
            .to_string(),
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

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error
            .message()
            .contains("bare command objects inside actions"),
        "{}",
        error.message()
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["type"], "malformed_model_output");
    assert_eq!(failure_json["output"]["bare_command_actions"], true);
}

/// Verifies openai provider can be constructed from auth store secret reference.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn openai_provider_can_be_constructed_from_auth_store_secret_reference() {
    let root = std::env::temp_dir().join(format!("mez-agent-provider-auth-{}", std::process::id()));
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

    let provider = openai_provider_from_auth_store_with_transport(&auth_store, transport).unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer sk-provider-test")
    );
    let metadata = std::fs::read_to_string(auth_store.paths().auth_file()).unwrap();
    assert!(!metadata.contains("sk-provider-test"));
    let _ = std::fs::remove_dir_all(root);
}
