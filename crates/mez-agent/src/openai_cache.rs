//! OpenAI request rendering and prompt-cache diagnostics.
//!
//! This module owns the OpenAI-specific conversion from canonical model
//! messages into Responses API `instructions` and `input` material. It also
//! computes non-model-visible prompt-cache fingerprints used for diagnostics.

use crate::context::{ContextEpochIdentity, ContextEpochTransition, OpenAiInputChain};
use crate::openai_request::openai_responses_request_control_shape_with_stream;
use crate::openai_schema::openai_maap_action_batch_tools;
use crate::provider::MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME;
use crate::{
    ContextSourceKind, ModelInteractionKind, ModelRequest, OpenAiPromptCacheDiagnostics,
    OpenAiRenderedMessages, ProviderRequestAssemblyError, ProviderRequestAssemblyResult,
    openai_auto_sizing_response_format, openai_macro_judge_response_format,
    openai_prompt_cache_diagnostics, openai_prompt_cache_key as provider_prompt_cache_key,
    openai_render_messages, openai_routed_handoff_response_format,
    openai_sandbox_failure_assessment_response_format, openai_stable_projection_material,
    validate_provider_request_required,
};
#[cfg(test)]
use crate::{ModelMessage, ModelMessageRole};
use sha2::{Digest, Sha256};

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

/// Renders the canonical durable request before any retained OpenAI wire-chain override.
fn openai_render_request_messages_without_chain(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiRenderedMessages> {
    openai_render_messages(&request.messages)
}

/// Prepares one exact append-only OpenAI input chain before a concrete send.
///
/// Ordinary requests in the same provider/model/lineage epoch must preserve
/// all cache-affecting envelope bytes and retain the prior complete canonical
/// input as an exact leading prefix. The current canonical input is rendered
/// solely from durable context; exceptional modes and explicit scope changes
/// start a new chain epoch.
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
    let current_epoch =
        openai_context_epoch_identity(request, &canonical, cache_namespace, stream)?;
    let Some(previous) = previous else {
        request.messages.set_openai_input_chain(openai_input_chain(
            canonical.input.clone(),
            cache_namespace,
            stream,
            current_epoch,
            ContextEpochTransition::Initial,
        ));
        return Ok(());
    };

    let previous_canonical = openai_render_request_messages_without_chain(previous)?;
    let previous_epoch = previous
        .messages
        .openai_input_chain()
        .map(|chain| chain.context_epoch.clone())
        .unwrap_or(openai_context_epoch_identity(
            previous,
            &previous_canonical,
            cache_namespace,
            stream,
        )?);
    if previous_epoch != current_epoch {
        let transition = ContextEpochTransition::Changed(
            previous_epoch
                .changed_component(&current_epoch)
                .expect("different context epochs must identify a changed component"),
        );
        request.messages.set_openai_input_chain(openai_input_chain(
            canonical.input,
            cache_namespace,
            stream,
            current_epoch,
            transition,
        ));
        return Ok(());
    }
    if !canonical.input.starts_with(&previous_canonical.input) {
        return Err(ProviderRequestAssemblyError::invalid_state(
            "OpenAI request chain rewrote canonical provider input inside one cache epoch",
        ));
    }
    request.messages.set_openai_input_chain(openai_input_chain(
        canonical.input,
        cache_namespace,
        stream,
        current_epoch,
        previous
            .messages
            .openai_input_chain()
            .map_or(ContextEpochTransition::Initial, |chain| {
                chain.epoch_transition
            }),
    ));
    Ok(())
}

/// Freezes canonical and effective OpenAI input for a request-chain generation.
fn openai_input_chain(
    effective_input: Vec<serde_json::Value>,
    cache_namespace: &str,
    stream: bool,
    context_epoch: ContextEpochIdentity,
    epoch_transition: ContextEpochTransition,
) -> OpenAiInputChain {
    OpenAiInputChain {
        effective_input: std::sync::Arc::new(effective_input),
        cache_namespace: cache_namespace.to_string(),
        stream,
        context_epoch,
        epoch_transition,
    }
}

/// Builds the exact non-model-visible identity for one OpenAI Responses epoch.
///
/// Every field is derived from canonical provider-bound state. A difference
/// starts a fresh epoch before dispatch rather than permitting a same-epoch
/// rewrite of provider input.
fn openai_context_epoch_identity(
    request: &ModelRequest,
    rendered: &OpenAiRenderedMessages,
    cache_namespace: &str,
    stream: bool,
) -> ProviderRequestAssemblyResult<ContextEpochIdentity> {
    let response_format = openai_response_format(request).unwrap_or(serde_json::Value::Null);
    let (tools, tool_choice) = if request.interaction_kind.expects_structured_json() {
        (serde_json::json!([]), serde_json::json!("none"))
    } else {
        (
            serde_json::json!(openai_maap_action_batch_tools(request)),
            serde_json::json!({
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "type": "function"
            }),
        )
    };
    let request_controls = openai_responses_request_control_shape_with_stream(request, stream)?;
    Ok(ContextEpochIdentity {
        provider_namespace: cache_namespace.to_string(),
        provider: request.provider.clone(),
        model: request.model.clone(),
        static_instructions_sha256: sha256_hex(rendered.instructions.as_bytes()),
        maap_schema_version: "maap/1".to_string(),
        response_format_sha256: canonical_json_sha256(&response_format)?,
        interaction_family: request.interaction_kind.as_str().to_string(),
        tool_schema_sha256: canonical_json_sha256(&tools)?,
        tool_choice_sha256: canonical_json_sha256(&tool_choice)?,
        request_controls_sha256: canonical_json_sha256(&request_controls)?,
        api_shape: format!("openai-responses;stream={stream}"),
        cache_lineage: request.prompt_cache_lineage_id.clone(),
        compaction_generation_sha256: sha256_hex(
            openai_compaction_generation_material(request).as_bytes(),
        ),
    })
}

/// Returns the exact durable compaction markers that identify one chronology epoch.
fn openai_compaction_generation_material(request: &ModelRequest) -> String {
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
        .map(|message| {
            format!(
                "{}:{}:{}",
                message.content.len(),
                message.source as u8,
                message.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns a deterministic SHA-256 digest for canonical JSON material.
fn canonical_json_sha256(value: &serde_json::Value) -> ProviderRequestAssemblyResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI context epoch JSON encoding failed: {error}"
        ))
    })?;
    Ok(sha256_hex(&bytes))
}

/// Returns lowercase SHA-256 text without retaining the hashed material.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
            messages: messages.into(),
        }
    }

    /// Verifies a rebuilt request retains every prior OpenAI input item before
    /// appending newly settled chronology.
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
            placement: crate::ContextPlacement::ConversationAppend,
            content: "[mcp integrations]\navailable_tool=files/read".to_string(),
        };
        let mut first = request_chain_fixture(vec![system.clone(), user.clone(), tail.clone()]);
        crate::append_request_state_transition(&mut first);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();
        let first_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&first).unwrap()).unwrap();

        let assistant = ModelMessage {
            role: ModelMessageRole::Assistant,
            source: ContextSourceKind::TranscriptAssistant,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "read the file".to_string(),
        };
        let mut second = first.clone();
        second.messages.push(assistant);
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

    /// Verifies an ordinary request chain appends new chronology while
    /// rejecting a rewrite of an already-sent chronological item.
    #[test]
    fn openai_request_chain_appends_chronology_and_rejects_rewrites() {
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
                placement: crate::ContextPlacement::ConversationAppend,
                content: "state=first".to_string(),
            },
        ];
        let mut first = request_chain_fixture(messages.clone());
        crate::append_request_state_transition(&mut first);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();

        let mut appended = first.clone();
        appended.messages.push(ModelMessage {
            role: ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "state=second".to_string(),
        });
        prepare_openai_request_prefix_extension(&mut appended, Some(&first)).unwrap();
        let first_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&first).unwrap()).unwrap();
        let appended_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&appended).unwrap())
                .unwrap();
        let first_input = first_body["input"].as_array().unwrap();
        let appended_input = appended_body["input"].as_array().unwrap();
        assert_eq!(first_input, &appended_input[..first_input.len()]);
        assert_eq!(appended_input.len(), first_input.len() + 1);

        let mut rewritten = first.clone();
        rewritten.messages = vec![
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
                placement: crate::ContextPlacement::ConversationAppend,
                content: "state=rewritten".to_string(),
            },
        ]
        .into();
        let error =
            prepare_openai_request_prefix_extension(&mut rewritten, Some(&first)).unwrap_err();
        assert!(error.message().contains("rewrote canonical provider input"));
    }

    /// Verifies a cache-affecting instruction change establishes a classified
    /// epoch instead of bypassing same-epoch canonical-input prefix checks.
    #[test]
    fn openai_instruction_change_starts_a_classified_context_epoch() {
        let user = ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "inspect the cache".to_string(),
        };
        let mut first = request_chain_fixture(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "first epoch instructions".to_string(),
            },
            user.clone(),
        ]);
        crate::append_request_state_transition(&mut first);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();

        let mut changed = request_chain_fixture(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "second epoch instructions".to_string(),
            },
            user,
        ]);
        crate::append_request_state_transition(&mut changed);
        prepare_openai_request_prefix_extension(&mut changed, Some(&first)).unwrap();

        assert!(matches!(
            changed
                .messages
                .openai_input_chain()
                .unwrap()
                .epoch_transition,
            ContextEpochTransition::Changed(crate::ContextEpochComponent::StaticInstructions)
        ));
        let body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&changed).unwrap()).unwrap();
        assert_eq!(body["instructions"], "second epoch instructions");
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

        assert_eq!(rendered.input.len(), 4);
        assert!(rendered.instructions.contains("system prompt"));
        assert!(rendered_json.contains("continue"));
        assert!(!rendered_json.contains("[OpenAI request state]"));
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
