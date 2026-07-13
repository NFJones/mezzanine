//! OpenAI request rendering and prompt-cache diagnostics.
//!
//! This module owns the OpenAI-specific conversion from Mezzanine model
//! messages into Responses API `instructions` and `input` material. It also
//! computes non-model-visible prompt-cache fingerprints used for diagnostics.

use super::OPENAI_MAAP_FUNCTION_TOOL_NAME;
use super::openai_request::openai_responses_request_control_shape_with_stream;
use super::schema::openai_maap_action_batch_tools;
use crate::agent::{
    ContextSourceKind, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest,
    ProviderTranscriptEvent,
};
use mez_agent::{
    OpenAiPromptCacheDiagnostics, ProviderRequestAssemblyError, ProviderRequestAssemblyResult,
    openai_auto_sizing_response_format, openai_current_action_result_entry_text,
    openai_current_user_prompt_entry_text, openai_executed_result_entry_text,
    openai_historical_action_result_entry_text, openai_historical_user_prompt_entry_text,
    openai_macro_judge_response_format, openai_prompt_cache_key as provider_prompt_cache_key,
    validate_provider_request_required,
};
use sha2::Digest;

/// Prefix used by local provider-context compaction summaries.
const OPENAI_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";

/// Provider-specific rendering of Mezzanine model messages for OpenAI Responses.
#[derive(Debug, Clone)]
pub(super) struct OpenAiRenderedMessages {
    /// Joined Responses `instructions` value.
    pub(super) instructions: String,
    /// Responses `input` messages.
    pub(super) input: Vec<serde_json::Value>,
    /// Input messages included in the stable reusable prefix.
    stable_input: Vec<serde_json::Value>,
    /// Input messages that belong to the volatile suffix.
    volatile_input: Vec<serde_json::Value>,
}

/// Renders request messages and captures canonical stable-prefix material.
pub(super) fn openai_render_request_messages(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiRenderedMessages> {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let mut stable_input = Vec::new();
    let mut volatile_input = Vec::new();
    let mut stable_input_open = true;
    for message in &request.messages {
        if ProviderTranscriptEvent::from_transcript_content(&message.content).is_some() {
            continue;
        }
        if openai_message_belongs_in_instructions(message) {
            instructions.push(message.content.clone());
            continue;
        }

        openai_push_input_message(
            message,
            &mut input,
            &mut stable_input,
            &mut volatile_input,
            &mut stable_input_open,
        );
    }
    if let Some(message) = openai_allowed_action_surface_message(request) {
        openai_push_input_message(
            &message,
            &mut input,
            &mut stable_input,
            &mut volatile_input,
            &mut stable_input_open,
        );
    }
    if input.is_empty() {
        return Err(ProviderRequestAssemblyError::invalid_args(
            "OpenAI Responses request requires at least one user or tool input message",
        ));
    }
    let instructions = instructions.join("\n\n");
    Ok(OpenAiRenderedMessages {
        instructions,
        input,
        stable_input,
        volatile_input,
    })
}

/// Adds one rendered input message to both provider input and cache diagnostics.
fn openai_push_input_message(
    message: &ModelMessage,
    input: &mut Vec<serde_json::Value>,
    stable_input: &mut Vec<serde_json::Value>,
    volatile_input: &mut Vec<serde_json::Value>,
    stable_input_open: &mut bool,
) {
    let value = openai_input_message_value(message);
    if *stable_input_open && openai_message_stable_prefix_eligible(message) {
        stable_input.push(value.clone());
    } else {
        *stable_input_open = false;
        volatile_input.push(value.clone());
    }
    input.push(value);
}

/// Returns true when a message should be rendered into OpenAI `instructions`.
fn openai_message_belongs_in_instructions(message: &ModelMessage) -> bool {
    message.role == ModelMessageRole::System
}

/// Renders one non-instruction message into OpenAI Responses input shape.
fn openai_input_message_value(message: &ModelMessage) -> serde_json::Value {
    match message.role {
        ModelMessageRole::Assistant => serde_json::json!({
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": message.content
                }
            ]
        }),
        ModelMessageRole::Developer => serde_json::json!({
            "role": "developer",
            "content": [
                {
                    "type": "input_text",
                    "text": message.content
                }
            ]
        }),
        ModelMessageRole::System => serde_json::json!({
            "role": "system",
            "content": [
                {
                    "type": "input_text",
                    "text": message.content
                }
            ]
        }),
        ModelMessageRole::User => serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": openai_user_input_text(message)
                }
            ]
        }),
        ModelMessageRole::Tool => serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": openai_tool_result_input_text(message)
                }
            ]
        }),
    }
}

/// Renders user-role input with explicit current-turn or historical provenance.
fn openai_user_input_text(message: &ModelMessage) -> String {
    match message.source {
        ContextSourceKind::Transcript | ContextSourceKind::TranscriptUser => {
            openai_historical_user_prompt_entry_text(&message.content)
        }
        ContextSourceKind::UserInstruction => {
            openai_current_user_prompt_entry_text(&message.content)
        }
        _ => message.content.clone(),
    }
}

/// Renders Mezzanine tool/action evidence through an OpenAI-supported message
/// role with explicit current-turn or historical provenance.
fn openai_tool_result_input_text(message: &ModelMessage) -> String {
    match message.source {
        ContextSourceKind::ActionResult => {
            openai_current_action_result_entry_text(&message.content)
        }
        ContextSourceKind::TranscriptTool => {
            openai_historical_action_result_entry_text(&message.content)
        }
        _ => openai_executed_result_entry_text(&message.content),
    }
}

/// Returns whether a rendered input message belongs in the reusable prefix.
fn openai_message_stable_prefix_eligible(message: &ModelMessage) -> bool {
    if openai_message_is_volatile_controller_state(message) {
        return false;
    }
    match message.source {
        ContextSourceKind::System
        | ContextSourceKind::DeveloperInstruction
        | ContextSourceKind::Configuration
        | ContextSourceKind::ProjectGuidance
        | ContextSourceKind::Memory
        | ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser
        | ContextSourceKind::TranscriptAssistant
        | ContextSourceKind::TranscriptTool
        | ContextSourceKind::CommittedEvidence => true,
        ContextSourceKind::Policy => !message.content.starts_with("[scheduler state]\n"),
        ContextSourceKind::UserInstruction
        | ContextSourceKind::SkillInstruction
        | ContextSourceKind::LocalMessage
        | ContextSourceKind::RuntimeHint
        | ContextSourceKind::EvidenceLedger
        | ContextSourceKind::ActionResult => false,
    }
}

/// Builds the late controller instruction that makes the current executable
/// surface visible in model context.
fn openai_allowed_action_surface_message(request: &ModelRequest) -> Option<ModelMessage> {
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        return None;
    }
    let allowed_actions = request.allowed_actions.action_type_names().join(",");
    Some(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::RuntimeHint,
        content: format!(
            "[allowed action surface]\n\
             interaction_kind={}\n\
             allowed_actions={allowed_actions}\n\
             active_function_tool={}\n\
             Emit only action objects whose type appears in allowed_actions and is present in active_function_tool; disallowed action types are rejected.",
            request.interaction_kind.as_str(),
            OPENAI_MAAP_FUNCTION_TOOL_NAME,
        ),
    })
}

/// Returns true for late controller state that should never enter the stable prefix.
fn openai_message_is_volatile_controller_state(message: &ModelMessage) -> bool {
    if openai_message_is_volatile_configuration_state(message) {
        return true;
    }
    let content = message.content.trim_start();
    content.starts_with("[capability ")
        || content.starts_with("[capability decisions]")
        || content.starts_with("[controller failure summary]")
        || content.starts_with(OPENAI_CONTEXT_COMPACTED_PREFIX)
}

/// Returns true when a rendered configuration message carries volatile runtime
/// identity that context ordering already excludes from the reusable prefix.
fn openai_message_is_volatile_configuration_state(message: &ModelMessage) -> bool {
    if message.source != ContextSourceKind::Configuration {
        return false;
    }
    let content = message.content.trim_start();
    content.starts_with("[session identity]")
        || content.starts_with("[pane identity]")
        || content.starts_with("[provider output-limit retry guidance]")
        || content.starts_with("[environment signature for pane ")
}

/// Returns the OpenAI response-format field for special request modes.
pub(super) fn openai_response_format(request: &ModelRequest) -> Option<serde_json::Value> {
    match request.interaction_kind {
        ModelInteractionKind::AutoSizing => Some(openai_auto_sizing_response_format()),
        ModelInteractionKind::MacroJudge => Some(openai_macro_judge_response_format()),
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
    let response_format_text = serde_json::to_string(&response_format).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI response-format diagnostics failed: {error}"
        ))
    })?;
    let tools = if request.interaction_kind.expects_structured_json() {
        serde_json::json!([])
    } else {
        serde_json::json!(openai_maap_action_batch_tools(request))
    };
    let tools_text = serde_json::to_string(&tools).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI tools diagnostics failed: {error}"
        ))
    })?;
    let tool_choice = if request.interaction_kind.expects_structured_json() {
        serde_json::json!("none")
    } else {
        serde_json::json!({
            "type": "function",
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME
        })
    };
    let tool_choice_text = serde_json::to_string(&tool_choice).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI tool-choice diagnostics failed: {error}"
        ))
    })?;
    let stable_input_text = serde_json::to_string(&rendered.stable_input).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI stable-input diagnostics failed: {error}"
        ))
    })?;
    let volatile_input_text = serde_json::to_string(&rendered.volatile_input).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI volatile-input diagnostics failed: {error}"
        ))
    })?;
    let stable_prompt_prefix =
        openai_stable_prefix_material(&rendered.instructions, &rendered.stable_input).map_err(
            |error| {
                ProviderRequestAssemblyError::invalid_state(format!(
                    "OpenAI stable prompt-prefix diagnostics failed: {error}"
                ))
            },
        )?;
    let provider_request_shape = serde_json::to_string(
        &openai_responses_request_control_shape_with_stream(request, stream)?,
    )
    .map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI request-shape diagnostics failed: {error}"
        ))
    })?;

    let stable_prompt_prefix_sha256 = sha256_hex(stable_prompt_prefix.as_bytes());
    let provider_request_shape_sha256 = sha256_hex(provider_request_shape.as_bytes());
    Ok(OpenAiPromptCacheDiagnostics {
        prompt_cache_key: openai_prompt_cache_key(request),
        instructions_bytes: rendered.instructions.len(),
        instructions_sha256: sha256_hex(rendered.instructions.as_bytes()),
        response_format_bytes: response_format_text.len(),
        response_format_sha256: sha256_hex(response_format_text.as_bytes()),
        tools_bytes: tools_text.len(),
        tools_sha256: sha256_hex(tools_text.as_bytes()),
        tool_choice_bytes: tool_choice_text.len(),
        tool_choice_sha256: sha256_hex(tool_choice_text.as_bytes()),
        stable_input_bytes: stable_input_text.len(),
        stable_input_sha256: sha256_hex(stable_input_text.as_bytes()),
        volatile_input_bytes: volatile_input_text.len(),
        volatile_input_sha256: sha256_hex(volatile_input_text.as_bytes()),
        stable_prompt_prefix_bytes: stable_prompt_prefix.len(),
        stable_prompt_prefix_sha256: stable_prompt_prefix_sha256.clone(),
        provider_request_shape_bytes: provider_request_shape.len(),
        provider_request_shape_sha256,
        cacheable_prefix_bytes: stable_prompt_prefix.len(),
        cacheable_prefix_sha256: stable_prompt_prefix_sha256,
    })
}

/// Returns canonical OpenAI stable-prefix material for tests and diagnostics.
#[cfg(test)]
pub(crate) fn openai_stable_prefix_material_for_request(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<String> {
    let rendered = openai_render_request_messages(request)?;
    openai_stable_prefix_material(&rendered.instructions, &rendered.stable_input).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI stable prefix material encoding failed: {error}"
        ))
    })
}

/// Builds canonical provider-visible stable-prefix material.
fn openai_stable_prefix_material(
    instructions: &str,
    stable_input: &[serde_json::Value],
) -> serde_json::Result<String> {
    serde_json::to_string(&serde_json::json!({
        "cache_family": "responses-prefix-v2",
        "instructions": instructions,
        "stable_input": stable_input,
    }))
}

/// Encodes bytes as lower-case SHA-256 hexadecimal text.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AllowedActionSet;

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
                    content: "system prompt".to_string(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    content: event.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    content: "continue".to_string(),
                },
            ],
        };

        let rendered = openai_render_request_messages(&request).unwrap();
        let rendered_json = serde_json::to_string(&rendered.input).unwrap();

        assert_eq!(rendered.input.len(), 2);
        assert!(rendered.instructions.contains("system prompt"));
        assert!(rendered_json.contains("continue"));
        assert!(!rendered.instructions.contains("DeepSeek-only reasoning"));
        assert!(!rendered_json.contains("reasoning_content"));
        assert!(!rendered_json.contains("call_1"));
    }
}
