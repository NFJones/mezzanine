//! OpenAI request rendering and prompt-cache diagnostics.
//!
//! This module owns the OpenAI-specific conversion from canonical model
//! messages into Responses API `instructions` and `input` material. It also
//! computes non-model-visible prompt-cache fingerprints used for diagnostics.

use crate::context::OpenAiInputChain;
use crate::openai_request::openai_responses_request_control_shape_with_stream;
use crate::openai_schema::openai_maap_action_batch_tools;
use crate::provider::MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME;
use crate::{
    ContextSourceKind, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest,
    OpenAiPromptCacheDiagnostics, OpenAiRenderedMessages, ProviderRequestAssemblyError,
    ProviderRequestAssemblyResult, openai_auto_sizing_response_format,
    openai_macro_judge_response_format, openai_prompt_cache_diagnostics,
    openai_prompt_cache_key as provider_prompt_cache_key, openai_render_messages,
    openai_routed_handoff_response_format, openai_sandbox_failure_assessment_response_format,
    openai_stable_projection_material, validate_provider_request_required,
};

/// Renders request messages and captures canonical stable-prefix material.
pub(super) fn openai_render_request_messages(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiRenderedMessages> {
    let chain = request.messages.openai_input_chain().cloned();
    let mut rendered = openai_render_request_messages_without_chain(request)?;
    if let Some(chain) = chain {
        rendered.input = chain.effective_input.as_ref().clone();
    }
    Ok(rendered)
}

/// Renders the canonical request before any retained OpenAI wire-chain override.
fn openai_render_request_messages_without_chain(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiRenderedMessages> {
    let mut messages = request.messages.clone();
    let has_chronological_request_state = messages.iter().any(|message| {
        matches!(
            message.source,
            ContextSourceKind::RuntimeHint | ContextSourceKind::CommittedEvidence
        ) && message.placement == crate::ContextPlacement::ConversationAppend
            && message.content.starts_with("[Mezzanine request state]")
    });
    if request.interaction_kind.expects_maap_batch() && !has_chronological_request_state {
        messages.push(ModelMessage {
            role: ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::EphemeralTail,
            content: format!(
                "[OpenAI request state]\ninteraction_kind={}\nallowed_actions={}",
                request.interaction_kind.as_str(),
                request.allowed_actions.action_type_names().join(",")
            ),
        });
    }
    if let Some(recovery_input) = request
        .recovery_input
        .as_deref()
        .filter(|input| !input.is_empty())
    {
        messages.push(ModelMessage {
            role: ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::EphemeralTail,
            content: recovery_input.to_string(),
        });
    }
    openai_render_messages(&messages)
}

/// Prepares one exact append-only OpenAI input chain before a concrete send.
///
/// Ordinary requests in the same provider/model/lineage epoch must preserve
/// all cache-affecting envelope bytes, retain the prior complete input, and
/// append only new canonical chronology or superseding live state. Exceptional
/// modes and explicit scope changes start a new chain epoch.
pub fn prepare_openai_request_prefix_extension(
    request: &mut ModelRequest,
    previous: Option<&ModelRequest>,
) -> ProviderRequestAssemblyResult<()> {
    let cache_namespace = request.provider.clone();
    prepare_openai_request_prefix_extension_with_context(request, previous, &cache_namespace, false)
}

/// Prepares one exact OpenAI chain using the concrete routing and stream epoch.
pub fn prepare_openai_request_prefix_extension_with_context(
    request: &mut ModelRequest,
    previous: Option<&ModelRequest>,
    cache_namespace: &str,
    stream: bool,
) -> ProviderRequestAssemblyResult<()> {
    let canonical = openai_render_request_messages_without_chain(request)?;
    let Some(previous) = previous
        .filter(|previous| openai_same_cache_epoch(previous, request, cache_namespace, stream))
    else {
        request.messages.set_openai_input_chain(openai_input_chain(
            canonical.input.clone(),
            cache_namespace,
            stream,
        ));
        return Ok(());
    };

    let previous_canonical = openai_render_request_messages_without_chain(previous)?;
    validate_openai_request_envelope(previous, request, stream)?;
    if !canonical
        .stable_input
        .starts_with(&previous_canonical.stable_input)
    {
        return Err(ProviderRequestAssemblyError::invalid_state(
            "OpenAI request chain rewrote canonical stable input inside one cache epoch",
        ));
    }
    let previous_effective = previous
        .messages
        .openai_input_chain()
        .map(|chain| chain.effective_input.as_ref().clone())
        .unwrap_or_else(|| previous_canonical.input.clone());
    let mut effective = previous_effective;
    effective.extend_from_slice(&canonical.stable_input[previous_canonical.stable_input.len()..]);
    if canonical.volatile_input != previous_canonical.volatile_input {
        if canonical.volatile_input.len() < previous_canonical.volatile_input.len() {
            return Err(ProviderRequestAssemblyError::invalid_state(
                "OpenAI request chain removed canonical volatile input without superseding state",
            ));
        }
        effective.extend(
            canonical
                .volatile_input
                .iter()
                .enumerate()
                .filter(|(index, value)| {
                    previous_canonical.volatile_input.get(*index) != Some(value)
                })
                .map(|(_, value)| value.clone()),
        );
    }
    request
        .messages
        .set_openai_input_chain(openai_input_chain(effective, cache_namespace, stream));
    Ok(())
}

/// Returns whether two requests belong to one ordinary OpenAI cache epoch.
fn openai_same_cache_epoch(
    previous: &ModelRequest,
    current: &ModelRequest,
    cache_namespace: &str,
    stream: bool,
) -> bool {
    previous.provider == current.provider
        && previous.model == current.model
        && previous.prompt_cache_lineage_id == current.prompt_cache_lineage_id
        && openai_compaction_epoch(previous) == openai_compaction_epoch(current)
        && previous
            .messages
            .openai_input_chain()
            .is_none_or(|chain| chain.cache_namespace == cache_namespace && chain.stream == stream)
        && previous
            .interaction_kind
            .expected_cache_break_reason()
            .is_none()
        && current
            .interaction_kind
            .expected_cache_break_reason()
            .is_none()
}

/// Returns exact compaction markers that identify one rewritten context epoch.
fn openai_compaction_epoch(request: &ModelRequest) -> Vec<&str> {
    request
        .messages
        .iter()
        .filter(|message| {
            message.source == ContextSourceKind::Memory
                && (message
                    .content
                    .starts_with("[context compaction summary]\n")
                    || message
                        .content
                        .starts_with("[conversation compaction notice]\n")
                    || message.content.starts_with("[memory compact-"))
        })
        .map(|message| message.content.as_str())
        .collect()
}

/// Freezes canonical and effective OpenAI input for a request-chain generation.
fn openai_input_chain(
    effective_input: Vec<serde_json::Value>,
    cache_namespace: &str,
    stream: bool,
) -> OpenAiInputChain {
    OpenAiInputChain {
        effective_input: std::sync::Arc::new(effective_input),
        cache_namespace: cache_namespace.to_string(),
        stream,
    }
}

/// Rejects cache-affecting envelope changes inside one ordinary request epoch.
fn validate_openai_request_envelope(
    previous: &ModelRequest,
    current: &ModelRequest,
    stream: bool,
) -> ProviderRequestAssemblyResult<()> {
    let previous = openai_prompt_cache_diagnostics_for_request_with_stream(previous, stream)?;
    let current = openai_prompt_cache_diagnostics_for_request_with_stream(current, stream)?;
    for (name, previous, current) in [
        (
            "instructions",
            previous.instructions_sha256,
            current.instructions_sha256,
        ),
        (
            "response_format",
            previous.response_format_sha256,
            current.response_format_sha256,
        ),
        ("tools", previous.tools_sha256, current.tools_sha256),
        (
            "tool_choice",
            previous.tool_choice_sha256,
            current.tool_choice_sha256,
        ),
        (
            "prompt_cache_key",
            previous.prompt_cache_key,
            current.prompt_cache_key,
        ),
        (
            "request_control",
            previous.provider_request_shape_sha256,
            current.provider_request_shape_sha256,
        ),
    ] {
        if previous != current {
            return Err(ProviderRequestAssemblyError::invalid_state(format!(
                "OpenAI request chain changed {name} inside one cache epoch"
            )));
        }
    }
    Ok(())
}

/// Returns the OpenAI response-format field for special request modes.
pub(super) fn openai_response_format(request: &ModelRequest) -> Option<serde_json::Value> {
    match request.interaction_kind {
        ModelInteractionKind::AutoSizing => Some(openai_auto_sizing_response_format()),
        ModelInteractionKind::MacroJudge => Some(openai_macro_judge_response_format()),
        ModelInteractionKind::SandboxFailureAssessment => {
            Some(openai_sandbox_failure_assessment_response_format())
        }
        ModelInteractionKind::RoutedHandoff | ModelInteractionKind::RoutedHandoffRepair => {
            Some(openai_routed_handoff_response_format())
        }
        _ => None,
    }
}

/// Builds a stable, non-secret OpenAI prompt-cache routing key for a request.
pub(super) fn openai_prompt_cache_key(request: &ModelRequest) -> String {
    provider_prompt_cache_key(
        &request.provider,
        request.prompt_cache_lineage_id.as_deref(),
    )
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request.
pub fn openai_prompt_cache_diagnostics_for_request(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiPromptCacheDiagnostics> {
    openai_prompt_cache_diagnostics_for_request_with_stream(request, false)
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request and stream mode.
pub fn openai_prompt_cache_diagnostics_for_request_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> ProviderRequestAssemblyResult<OpenAiPromptCacheDiagnostics> {
    validate_provider_request_required("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let response_format = openai_response_format(request).unwrap_or(serde_json::Value::Null);
    let tools = if request.interaction_kind.expects_structured_json() {
        serde_json::json!([])
    } else {
        serde_json::json!(openai_maap_action_batch_tools(request))
    };
    let tool_choice = if request.interaction_kind.expects_structured_json() {
        serde_json::json!("none")
    } else {
        serde_json::json!({
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
            "type": "function"
        })
    };
    let provider_request_shape =
        openai_responses_request_control_shape_with_stream(request, stream)?;
    let prompt_cache_key = openai_prompt_cache_key(request);
    let mut complete_request = provider_request_shape.clone();
    complete_request["instructions"] = serde_json::json!(rendered.instructions);
    complete_request["input"] = serde_json::json!(rendered.input);
    complete_request["prompt_cache_key"] = serde_json::json!(prompt_cache_key);
    openai_prompt_cache_diagnostics(
        prompt_cache_key,
        &rendered,
        &response_format,
        &tools,
        &tool_choice,
        &provider_request_shape,
        &complete_request,
    )
}

/// Returns the local instructions-and-stable-input projection for a request.
pub fn openai_stable_projection_material_for_request(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<String> {
    let rendered = openai_render_request_messages(request)?;
    openai_stable_projection_material(&rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowedActionSet, ProviderTranscriptEvent};

    /// Builds one ordinary OpenAI request for exact request-chain tests.
    fn request_chain_fixture(messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_effort: Some("medium".to_string()),
            thinking_enabled: None,
            latency_preference: Some("default".to_string()),
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: Some("session-1".to_string()),
            prompt_cache_lineage_id: Some("lineage-1".to_string()),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::action_execution_base(),
            stop: None,
            recovery_input: None,
            messages: messages.into(),
        }
    }

    /// Verifies a rebuilt request retains every prior OpenAI input item before
    /// appending newly settled chronology, including a prior volatile tail.
    ///
    /// The provider caches the complete rendered input rather than Mezzanine's
    /// logical placement classes. This regression therefore compares actual
    /// request JSON and fails if a request-local message is relocated.
    #[test]
    fn openai_request_chain_preserves_complete_wire_prefix() {
        let system = ModelMessage {
            role: ModelMessageRole::System,
            source: ContextSourceKind::System,
            placement: crate::ContextPlacement::StablePrefix,
            content: "stable instructions".to_string(),
        };
        let user = ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "inspect the cache".to_string(),
        };
        let tail = ModelMessage {
            role: ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::EphemeralTail,
            content: "[mcp integrations]\navailable_tool=files/read".to_string(),
        };
        let mut first = request_chain_fixture(vec![system.clone(), user.clone(), tail.clone()]);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();
        let first_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&first).unwrap()).unwrap();

        let assistant = ModelMessage {
            role: ModelMessageRole::Assistant,
            source: ContextSourceKind::TranscriptAssistant,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "read the file".to_string(),
        };
        let mut second = request_chain_fixture(vec![system, user, assistant, tail]);
        prepare_openai_request_prefix_extension(&mut second, Some(&first)).unwrap();
        let second_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&second).unwrap()).unwrap();
        let first_input = first_body["input"].as_array().unwrap();
        let second_input = second_body["input"].as_array().unwrap();

        assert_eq!(first_input, &second_input[..first_input.len()]);
        assert_eq!(second_input.len(), first_input.len() + 1);
        let first_diagnostics = openai_prompt_cache_diagnostics_for_request(&first).unwrap();
        let second_diagnostics = openai_prompt_cache_diagnostics_for_request(&second).unwrap();
        let continuity = crate::compare_openai_request_continuity(
            &first_diagnostics.continuity_snapshot,
            &second_diagnostics.continuity_snapshot,
        );
        assert!(continuity.messages_append_only, "{continuity:#?}");
        assert_eq!(continuity.common_message_prefix, first_input.len());
    }

    /// Verifies an ordinary request chain appends superseding live state while
    /// rejecting envelope rewrites inside the same cache epoch.
    #[test]
    fn openai_request_chain_appends_live_state_and_rejects_envelope_rewrites() {
        let messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "stable instructions".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "inspect the cache".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::EphemeralTail,
                content: "state=first".to_string(),
            },
        ];
        let mut first = request_chain_fixture(messages.clone());
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();

        let mut changed_tail_messages = messages.clone();
        changed_tail_messages.last_mut().unwrap().content = "state=second".to_string();
        let mut changed_tail = request_chain_fixture(changed_tail_messages);
        prepare_openai_request_prefix_extension(&mut changed_tail, Some(&first)).unwrap();
        let first_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&first).unwrap()).unwrap();
        let changed_tail_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&changed_tail).unwrap())
                .unwrap();
        let first_input = first_body["input"].as_array().unwrap();
        let changed_tail_input = changed_tail_body["input"].as_array().unwrap();
        assert_eq!(first_input, &changed_tail_input[..first_input.len()]);
        assert_eq!(changed_tail_input.len(), first_input.len() + 1);

        let mut removed_tail = request_chain_fixture(messages[..2].to_vec());
        let error =
            prepare_openai_request_prefix_extension(&mut removed_tail, Some(&first)).unwrap_err();
        assert!(error.message().contains("without superseding state"));

        let mut changed_instructions = request_chain_fixture(messages);
        changed_instructions.messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "rewritten instructions".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "inspect the cache".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::EphemeralTail,
                content: "state=first".to_string(),
            },
        ]
        .into();
        let error =
            prepare_openai_request_prefix_extension(&mut changed_instructions, Some(&first))
                .unwrap_err();
        assert!(error.message().contains("changed instructions"));
    }

    /// Verifies OpenAI request rendering ignores hidden provider-native
    /// transcript events.
    ///
    /// DeepSeek can persist hidden replay metadata into shared transcript
    /// history. If a later request is routed through OpenAI, that metadata must
    /// not become an instruction or input message because OpenAI does not
    /// understand DeepSeek `reasoning_content` or Chat Completions tool-call
    /// replay fields.
    #[test]
    fn openai_rendering_omits_hidden_provider_transcript_events() {
        let event = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: "".to_string(),
            reasoning_content: Some("DeepSeek-only reasoning".to_string()),
            tool_calls: vec![serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }
            })],
        };
        let openai_output = ProviderTranscriptEvent::OpenAiResponseOutput {
            items: vec![
                serde_json::json!({
                    "type": "reasoning",
                    "id": "rs_openai",
                    "encrypted_content": "opaque-openai-ciphertext"
                }),
                serde_json::json!({
                    "type": "function_call",
                    "id": "fc_openai",
                    "call_id": "call_openai",
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }),
            ],
        };
        let openai_result = ProviderTranscriptEvent::OpenAiFunctionCallOutput {
            call_id: "call_openai".to_string(),
            output: "[action_result action-1 shell_command succeeded]\nexit_code: 0\noutput:\nopenai-live-output-sentinel"
                .to_string(),
        };
        let request = ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            recovery_input: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::CapabilityDecision,
            allowed_actions: AllowedActionSet::capability_decision(),
            messages: vec![
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::System,
                    placement: crate::ContextPlacement::StablePrefix,
                    content: "system prompt".to_string(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: event.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: openai_output.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: openai_result.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: "continue".to_string(),
                },
            ]
            .into(),
        };

        let rendered = openai_render_request_messages(&request).unwrap();
        let rendered_json = serde_json::to_string(&rendered.input).unwrap();

        assert_eq!(rendered.input.len(), 5);
        assert!(rendered.instructions.contains("system prompt"));
        assert!(rendered_json.contains("continue"));
        assert!(rendered_json.contains("[OpenAI request state]"));
        assert!(rendered_json.contains("opaque-openai-ciphertext"));
        assert!(rendered_json.contains("\"call_id\":\"call_openai\""));
        assert!(rendered_json.contains("function_call_output"));
        assert!(rendered_json.contains("openai-live-output-sentinel"));
        assert!(!rendered_json.contains("historical_output: omitted"));
        assert!(!rendered.instructions.contains("DeepSeek-only reasoning"));
        assert!(!rendered_json.contains("reasoning_content"));
        assert!(!rendered_json.contains("call_1"));
    }
}
