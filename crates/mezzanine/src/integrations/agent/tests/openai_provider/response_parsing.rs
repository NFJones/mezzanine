//! Openai Provider tests for response parsing behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies that a batch-shaped response with incomplete command actions is
/// diagnosed as malformed model output. This is the common failure shape when a
/// model returns `{"rationale":"test action batch rationale","actions":[{"command":"ls"}]}` instead of a complete MAAP
/// action batch.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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

#[test]
/// Verifies that action-like JSON which is not a MAAP batch produces a specific
/// diagnostic. This covers models or provider endpoints that return a bare
/// command object instead of using the negotiated MAAP function-call or
/// structured-output envelope.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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

#[test]
/// Verifies that the OpenAI text adapter preserves the raw text while also
/// parsing a fenced MAAP fallback block into the response action batch.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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

#[test]
/// Verifies that the OpenAI Responses function-calling path is treated as the
/// primary executable-action transport. The model returns function-call
/// `arguments` as a JSON string, and Mezzanine parses those arguments as the
/// MAAP batch instead of waiting for assistant text output.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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

#[test]
/// Verifies that provider-native Responses structured output is parsed
/// directly as a MAAP action batch before the fenced fallback path is needed.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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

#[test]
/// Verifies that malformed provider-native structured MAAP output is rejected
/// rather than being silently treated as ordinary assistant prose.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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

#[test]
/// Verifies that ChatGPT-backed streaming Responses function-call events are
/// normalized into the same MAAP batch shape as non-streaming API responses.
/// The stream parser needs to aggregate argument deltas, because browser/device
/// auth routes through the streaming Codex backend.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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

#[test]
/// Verifies cumulative streaming function-call argument snapshots replace the
/// previous buffer instead of appending forever.
///
/// Some ChatGPT-backed streaming paths send the complete argument prefix in
/// each `delta` event. Treating those as true append-only deltas can grow
/// memory indefinitely and eventually produce invalid duplicated MAAP JSON.
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
            placement: mez_agent::ContextPlacement::EphemeralTail,
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
