//! OpenAI request rendering and prompt-cache diagnostics.
//!
//! This module owns the OpenAI-specific conversion from Mezzanine model
//! messages into Responses API `instructions` and `input` material. It also
//! computes non-model-visible prompt-cache fingerprints used for diagnostics.

use super::schema::{openai_maap_action_batch_tools, openai_maap_tool_surface_for_request};
use super::validate_non_empty;
use crate::agent::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, ContextSourceKind,
    ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest, ProviderTranscriptEvent,
};
use crate::error::{MezError, Result};
use sha2::Digest;

/// Prefix used by local provider-context compaction summaries.
const OPENAI_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";

/// Non-model-visible fingerprints for diagnosing provider prompt-cache reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiPromptCacheDiagnostics {
    /// Stable routing key sent to the OpenAI Responses API.
    pub prompt_cache_key: String,
    /// Bytes in the front-loaded OpenAI `instructions` field.
    pub instructions_bytes: usize,
    /// SHA-256 of the front-loaded OpenAI `instructions` field.
    pub instructions_sha256: String,
    /// Bytes in the OpenAI structured response format schema.
    pub response_format_bytes: usize,
    /// SHA-256 of the OpenAI structured response format schema.
    pub response_format_sha256: String,
    /// Bytes in the OpenAI `tools` list.
    pub tools_bytes: usize,
    /// SHA-256 of the OpenAI `tools` list.
    pub tools_sha256: String,
    /// Bytes in the OpenAI request-level `tool_choice` value.
    pub tool_choice_bytes: usize,
    /// SHA-256 of the OpenAI request-level `tool_choice` value.
    pub tool_choice_sha256: String,
    /// Bytes in the stable input prefix following instructions/tools/schema.
    pub stable_input_bytes: usize,
    /// SHA-256 of the stable input prefix following instructions/tools/schema.
    pub stable_input_sha256: String,
    /// Bytes in volatile input suffix material.
    pub volatile_input_bytes: usize,
    /// SHA-256 of volatile input suffix material.
    pub volatile_input_sha256: String,
    /// Bytes in the complete cacheable prefix material Mezzanine can observe.
    pub cacheable_prefix_bytes: usize,
    /// SHA-256 of the complete cacheable prefix material Mezzanine can observe.
    pub cacheable_prefix_sha256: String,
}

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
) -> Result<OpenAiRenderedMessages> {
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
        return Err(MezError::invalid_args(
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
                    "text": message.content
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

/// Renders Mezzanine tool/action evidence through an OpenAI-supported message
/// role while preserving its provenance in-band.
fn openai_tool_result_input_text(message: &ModelMessage) -> String {
    let marker = match message.source {
        ContextSourceKind::ActionResult => "[current action result]",
        ContextSourceKind::TranscriptTool => "[historical tool result]",
        _ => "[tool result]",
    };
    format!(
        "{marker}\n\
         This is executed Mezzanine action output, not a new user request.\n\
         {}",
        message.content
    )
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
        | ContextSourceKind::TranscriptAssistant => true,
        ContextSourceKind::Policy => !message.content.starts_with("[scheduler state]\n"),
        ContextSourceKind::UserInstruction
        | ContextSourceKind::LocalMessage
        | ContextSourceKind::TranscriptTool
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
    let selected_tool = openai_maap_tool_surface_for_request(request).tool_name();
    Some(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::LocalMessage,
        content: format!(
            "[allowed action surface]\n\
             interaction_kind={}\n\
             allowed_actions={allowed_actions}\n\
             active_function_tool={selected_tool}\n\
             This controller state is authoritative for action eligibility. \
             OpenAI may receive a cache-stable list of inactive MAAP tools, but tool_choice selects only active_function_tool for this request. \
             Emit only action objects whose type appears in allowed_actions and is present in the selected function schema. \
             Treat [current action result] and [action_result ...] messages as current execution evidence. If they already satisfy the task, emit say with status final instead of requesting capability or rerunning actions to reconfirm them. \
             Model-selected skill lookup/loading is disabled; do not emit request_skills or call_skill. Users can still invoke skills explicitly with $<skill-name> syntax before this request is built. \
             If the needed action type is absent and request_capability appears in allowed_actions, emit request_capability immediately for the needed coarse capability; do not spend the response on a plan or progress message. \
             If no listed action can make progress, emit say with status blocked or final. \
             Disallowed action types are rejected by Mezzanine and waste a recovery attempt.",
            request.interaction_kind.as_str(),
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

/// Builds the OpenAI structured-output schema for internal auto-sizing
/// decisions.
fn openai_auto_sizing_response_format() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "name": "mezzanine_auto_sizing_decision",
        "description": "Internal Mezzanine turn model and reasoning sizing decision.",
        "strict": true,
        "schema": {
            "type": "object",
            "properties": {
                "version": { "type": "integer", "enum": [1] },
                "size": { "type": "string", "enum": ["small", "medium", "large"] },
                "reasoning_effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "xhigh"]
                },
                "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "rationale": {
                    "type": "string",
                    "description": "Short non-secret explanation suitable for an agent status log."
                }
            },
            "required": ["version", "size", "reasoning_effort", "confidence", "rationale"],
            "additionalProperties": false
        }
    })
}

/// Returns the OpenAI response-format field for special request modes.
pub(super) fn openai_response_format(request: &ModelRequest) -> Option<serde_json::Value> {
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        return Some(openai_auto_sizing_response_format());
    }
    None
}

/// Builds a stable, non-secret OpenAI prompt-cache routing key for a request.
pub(super) fn openai_prompt_cache_key(request: &ModelRequest) -> String {
    let mut material = String::new();
    material.push_str("mezzanine\n");
    material.push_str("prompt_profile=");
    material.push_str(AGENT_PROMPT_PROFILE_NAME);
    material.push('\n');
    material.push_str("prompt_version=");
    material.push_str(&AGENT_PROMPT_PROFILE_VERSION.to_string());
    material.push('\n');
    material.push_str("session_id=");
    material.push_str(
        request
            .prompt_cache_session_id
            .as_deref()
            .unwrap_or("session-unknown"),
    );
    material.push('\n');
    material.push_str("cache_family=responses-routing-v4\n");
    format!("mez-{}", &sha256_hex(material.as_bytes())[..32])
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request.
pub fn openai_prompt_cache_diagnostics_for_request(
    request: &ModelRequest,
) -> Result<OpenAiPromptCacheDiagnostics> {
    validate_non_empty("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let response_format = openai_response_format(request).unwrap_or(serde_json::Value::Null);
    let response_format_text = serde_json::to_string(&response_format).map_err(|error| {
        MezError::invalid_state(format!(
            "OpenAI response-format diagnostics failed: {error}"
        ))
    })?;
    let tools = if request.interaction_kind == ModelInteractionKind::AutoSizing {
        serde_json::json!([])
    } else {
        serde_json::json!(openai_maap_action_batch_tools(request))
    };
    let tools_text = serde_json::to_string(&tools).map_err(|error| {
        MezError::invalid_state(format!("OpenAI tools diagnostics failed: {error}"))
    })?;
    let tool_choice = if request.interaction_kind == ModelInteractionKind::AutoSizing {
        serde_json::json!("none")
    } else {
        let surface = openai_maap_tool_surface_for_request(request);
        serde_json::json!({
            "type": "function",
            "name": surface.tool_name()
        })
    };
    let tool_choice_text = serde_json::to_string(&tool_choice).map_err(|error| {
        MezError::invalid_state(format!("OpenAI tool-choice diagnostics failed: {error}"))
    })?;
    let stable_input_text = serde_json::to_string(&rendered.stable_input).map_err(|error| {
        MezError::invalid_state(format!("OpenAI stable-input diagnostics failed: {error}"))
    })?;
    let volatile_input_text = serde_json::to_string(&rendered.volatile_input).map_err(|error| {
        MezError::invalid_state(format!("OpenAI volatile-input diagnostics failed: {error}"))
    })?;
    let cacheable_prefix = serde_json::to_string(&serde_json::json!({
        "cache_family": "responses-routing-v4",
        "instructions": rendered.instructions,
        "response_format": response_format,
        "tools": tools,
        "tool_choice": tool_choice,
        "stable_input": rendered.stable_input,
    }))
    .map_err(|error| {
        MezError::invalid_state(format!("OpenAI cache-prefix diagnostics failed: {error}"))
    })?;

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
        cacheable_prefix_bytes: cacheable_prefix.len(),
        cacheable_prefix_sha256: sha256_hex(cacheable_prefix.as_bytes()),
    })
}

/// Returns canonical OpenAI stable-prefix material for tests and diagnostics.
#[cfg(test)]
pub(crate) fn openai_stable_prefix_material_for_request(request: &ModelRequest) -> Result<String> {
    let rendered = openai_render_request_messages(request)?;
    openai_stable_prefix_material(&rendered.instructions, &rendered.stable_input).map_err(|error| {
        MezError::invalid_state(format!(
            "OpenAI stable prefix material encoding failed: {error}"
        ))
    })
}

/// Builds canonical provider-visible stable-prefix material.
#[cfg(test)]
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
            prompt_cache_session_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
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
