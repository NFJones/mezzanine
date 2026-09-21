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
            model_capabilities: Default::default(),
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
            model_capabilities: Default::default(),
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
            model_capabilities: Default::default(),
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
            content: "say hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let raw_text = r#"```mezzanine-action-json
{
  "rationale": "test action batch rationale",
  "actions": [
    {
      "type": "say",
      "status": "final",
      "text": "hello"
    }
  ]
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
            model_capabilities: Default::default(),
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
            model_capabilities: Default::default(),
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
            model_capabilities: Default::default(),
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
                "output_text": "{\"rationale\":\"test empty batch\",\"actions\":[]}"
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
        Some("{\"rationale\":\"test empty batch\",\"actions\":[]}")
    );
    let failure_json: serde_json::Value =
        serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
    assert_eq!(failure_json["type"], "malformed_model_output");
    assert_eq!(failure_json["output"]["format"], "json");
    let keys = failure_json["output"]["top_level_keys"].as_array().unwrap();
    assert!(keys.contains(&serde_json::json!("actions")));
    assert!(keys.contains(&serde_json::json!("rationale")));
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
            model_capabilities: Default::default(),
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
    match &batch.actions[0].payload {
        AgentActionPayload::ShellCommand { command, .. } => assert_eq!(command, "ls"),
        payload => panic!("unexpected payload: {payload:?}"),
    }
}

#[tokio::test]
/// Verifies provider streaming forwards an ordered `say` event backlog larger
/// than the former bounded progress channel without dropping source text.
///
/// Each source character arrives in its own SSE event. The collected deltas
/// must reconstruct the exact validated action text and include both lifecycle
/// events even when more than 32 provider fragments are queued.
async fn openai_provider_stream_forwards_lossless_say_event_backlog() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            model_capabilities: Default::default(),
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
            content: "stream a long answer".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let text = (0..96)
        .map(|index| char::from(b'a' + (index % 26) as u8))
        .collect::<String>();
    let maap = serde_json::json!({
        "rationale": "stream the requested answer",
        "actions": [{
            "type": "say",
            "status": "final",
            "content_type": "text/plain; charset=utf-8",
            "text": text,
        }],
    })
    .to_string();
    let mut body = String::new();
    for character in maap.chars() {
        body.push_str("event: response.output_text.delta\n");
        body.push_str("data: ");
        body.push_str(
            &serde_json::json!({
                "type": "response.output_text.delta",
                "delta": character.to_string(),
            })
            .to_string(),
        );
        body.push_str("\n\n");
    }
    body.push_str("event: response.completed\n");
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp_1", "model": "gpt-test"},
        })
    ));
    let transport = AsyncFakeProviderHttpTransport {
        requests: std::sync::Mutex::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body,
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
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);

    let provider_task = tokio::spawn(async move {
        provider
            .send_request_async_with_progress(&request, Some(sender))
            .await
    });
    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
        events.push(event);
    }
    let response = provider_task.await.unwrap().unwrap();

    assert!(matches!(
        events.first(),
        Some(mez_agent::StreamingSayEvent::RationaleStarted)
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Final,
            content_type,
        } if content_type == mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
    )));
    assert!(matches!(
        events.last(),
        Some(mez_agent::StreamingSayEvent::TextComplete { action_index: 0 })
    ));
    let streamed_text = events
        .iter()
        .filter_map(|event| match event {
            mez_agent::StreamingSayEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(streamed_text, text);
    let streamed_rationale = events
        .iter()
        .filter_map(|event| match event {
            mez_agent::StreamingSayEvent::RationaleTextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(streamed_rationale, "stream the requested answer");
    assert!(events.len() > 32, "events={}", events.len());
    let batch = response.action_batch.unwrap();
    match &batch.actions[0].payload {
        AgentActionPayload::Say {
            text: validated, ..
        } => assert_eq!(validated, &text),
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
            model_capabilities: Default::default(),
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

/// Verifies only successful direct GPT-5.6 Responses replies establish a
/// same-lineage cache comparison baseline, and a failed attempt retains it.
#[test]
fn openai_provider_reuses_successful_same_lineage_cache_comparison_baseline() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-5.6");
    request.prompt_cache_lineage_id = Some("lineage-1".to_string());
    request.interaction_kind = mez_agent::ModelInteractionKind::Compaction;
    let transport = SequencedFakeProviderHttpTransport::new(vec![
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"id":"resp-first","model":"gpt-5.6","output_text":"ok"}"#.to_string(),
        },
        ProviderHttpResponse {
            status_code: 500,
            headers: Default::default(),
            body: r#"{"error":{"message":"transient"}}"#.to_string(),
        },
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"id":"resp-third","model":"gpt-5.6","output_text":"ok"}"#.to_string(),
        },
    ]);
    let provider = OpenAiResponsesProvider::with_endpoint_and_headers(
        "test-key",
        "https://example.test/responses",
        10,
        std::collections::BTreeMap::from([(
            "OpenAI-Organization".to_string(),
            "org-1".to_string(),
        )]),
        transport,
    )
    .unwrap();

    provider.send_request(&request).unwrap();
    assert!(provider.send_request(&request).is_err());
    provider.send_request(&request).unwrap();

    let sent = provider.transport.requests.borrow();
    let bodies = sent
        .iter()
        .map(|request| serde_json::from_str::<serde_json::Value>(&request.body).unwrap())
        .collect::<Vec<_>>();
    assert!(
        bodies[0]
            .pointer("/prompt_cache_options/comparison_response_id")
            .is_none()
    );
    assert_eq!(
        bodies[1].pointer("/prompt_cache_options/comparison_response_id"),
        Some(&serde_json::json!("resp-first"))
    );
    assert_eq!(
        bodies[2].pointer("/prompt_cache_options/comparison_response_id"),
        Some(&serde_json::json!("resp-first"))
    );
}

/// Verifies independently keyed lineage baselines survive a successful request
/// for another lineage and are restored only for their matching tuple.
#[test]
fn openai_provider_keeps_comparison_baselines_per_lineage_tuple() {
    let state = crate::integrations::agent::provider::OpenAiCacheComparisonLineage::default();
    let headers = std::collections::BTreeMap::from([(
        "OpenAI-Organization".to_string(),
        "org-1".to_string(),
    )]);
    let mut first_request = openai_prompt_cache_retention_test_request("gpt-5.6");
    first_request.prompt_cache_lineage_id = Some("lineage-a".to_string());
    first_request.interaction_kind = mez_agent::ModelInteractionKind::Compaction;
    let mut second_request = first_request.clone();
    second_request.prompt_cache_lineage_id = Some("lineage-b".to_string());

    let first = OpenAiResponsesProvider::with_endpoint_and_headers(
        "test-key",
        "https://example.test/responses",
        10,
        headers.clone(),
        FakeProviderHttpTransport {
            requests: RefCell::new(Vec::new()),
            response: ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: r#"{"id":"resp-a","model":"gpt-5.6","output_text":"ok"}"#.to_string(),
            },
        },
    )
    .unwrap()
    .with_cache_comparison_lineage(state.clone());
    first.send_request(&first_request).unwrap();

    let second = OpenAiResponsesProvider::with_endpoint_and_headers(
        "test-key",
        "https://example.test/responses",
        10,
        headers.clone(),
        FakeProviderHttpTransport {
            requests: RefCell::new(Vec::new()),
            response: ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: r#"{"id":"resp-b","model":"gpt-5.6","output_text":"ok"}"#.to_string(),
            },
        },
    )
    .unwrap()
    .with_cache_comparison_lineage(state.clone());
    second.send_request(&second_request).unwrap();

    let restored = OpenAiResponsesProvider::with_endpoint_and_headers(
        "test-key",
        "https://example.test/responses",
        10,
        headers,
        FakeProviderHttpTransport {
            requests: RefCell::new(Vec::new()),
            response: ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: r#"{"id":"resp-a-next","model":"gpt-5.6","output_text":"ok"}"#.to_string(),
            },
        },
    )
    .unwrap()
    .with_cache_comparison_lineage(state);
    restored.send_request(&first_request).unwrap();

    let request: serde_json::Value =
        serde_json::from_str(&restored.transport.requests.borrow()[0].body).unwrap();
    assert_eq!(
        request.pointer("/prompt_cache_options/comparison_response_id"),
        Some(&serde_json::json!("resp-a"))
    );
}

/// Verifies an unscoped direct API-key provider never emits a retained
/// comparison response ID because it lacks non-secret account routing.
#[test]
fn openai_provider_omits_cache_comparison_without_direct_account_scope() {
    let mut request = openai_prompt_cache_retention_test_request("gpt-5.6");
    request.prompt_cache_lineage_id = Some("lineage-1".to_string());
    request.interaction_kind = mez_agent::ModelInteractionKind::Compaction;
    let transport = SequencedFakeProviderHttpTransport::new(vec![
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"id":"resp-first","model":"gpt-5.6","output_text":"ok"}"#.to_string(),
        },
        ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"id":"resp-second","model":"gpt-5.6","output_text":"ok"}"#.to_string(),
        },
    ]);
    let provider = OpenAiResponsesProvider::with_endpoint(
        "test-key",
        "https://example.test/responses",
        10,
        transport,
    )
    .unwrap();

    provider.send_request(&request).unwrap();
    provider.send_request(&request).unwrap();

    let sent = provider.transport.requests.borrow();
    assert!(sent.iter().all(|request| {
        serde_json::from_str::<serde_json::Value>(&request.body)
            .unwrap()
            .pointer("/prompt_cache_options/comparison_response_id")
            .is_none()
    }));
}
