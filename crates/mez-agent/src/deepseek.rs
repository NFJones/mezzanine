//! Provider-independent DeepSeek endpoint and protocol policy.
//!
//! This module owns deterministic DeepSeek endpoint derivation, request
//! strategy, thinking controls, native transcript replay, shim schemas, and
//! JSON request construction. Product adapters retain credentials, HTTP
//! metadata, transport, quota attachment, and error projection; deterministic
//! response parsing lives in the sibling `deepseek_response` module.

use crate::provider_continuity::{
    ProviderNativeRequestContinuity, prepare_provider_native_request_prefix_extension,
};
use crate::{
    AllowedActionSet, MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME, McpPromptTool,
    ModelCapabilities, ModelCapabilityMetadataPolicy, ModelInteractionKind, ModelMessageRole,
    ModelRequest, ProviderApiCompatibility, ProviderEndpointError, ProviderEndpointResult,
    ProviderRequestAssemblyError, ProviderRequestAssemblyResult, ProviderTranscriptEvent,
    maap_action_batch_schema,
};

/// Default DeepSeek Chat Completions API endpoint.
pub const DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT: &str = "https://api.deepseek.com/chat/completions";
/// Default DeepSeek models listing endpoint.
pub const DEEPSEEK_MODELS_ENDPOINT: &str = "https://api.deepseek.com/models";
/// DeepSeek shim function tool name used for capability routing turns.
pub const DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME: &str = "mez_decide_capability";
/// DeepSeek shim function tool name used for response-only turns.
pub const DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME: &str = "mez_respond";
/// DeepSeek shim function tool name used for executable action turns.
pub const DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME: &str = "mez_take_actions";

type MezError = ProviderRequestAssemblyError;
type Result<T> = ProviderRequestAssemblyResult<T>;

/// Derives the DeepSeek Chat Completions endpoint from a configured base URL.
pub fn deepseek_chat_completions_endpoint_for_base_url(
    base_url: &str,
) -> ProviderEndpointResult<String> {
    let base_url = deepseek_base_url(base_url)?;
    if base_url.ends_with("/chat/completions") {
        return Ok(base_url);
    }
    if let Some(prefix) = base_url.strip_suffix("/models") {
        return Ok(format!("{prefix}/chat/completions"));
    }
    Ok(format!("{base_url}/chat/completions"))
}

/// Derives the DeepSeek Models endpoint from a configured base URL or Chat
/// Completions endpoint.
pub fn deepseek_models_endpoint_for_base_url(base_url: &str) -> ProviderEndpointResult<String> {
    let chat_endpoint = deepseek_chat_completions_endpoint_for_base_url(base_url)?;
    Ok(chat_endpoint.replace("/chat/completions", "/models"))
}

fn deepseek_base_url(base_url: &str) -> ProviderEndpointResult<String> {
    if base_url.trim().is_empty() {
        return Err(ProviderEndpointError::invalid_args(
            "DeepSeek provider base URL must not be empty",
        ));
    }
    Ok(base_url.trim().trim_end_matches('/').to_string())
}

/// DeepSeek request strategy for provider-native MAAP transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepSeekMaapRequestStrategy {
    /// No MAAP tool is needed for this provider request.
    NoTool,
    /// Use DeepSeek thinking mode and let the model choose the MAAP tool.
    AutoToolThinking,
    /// Disable thinking and force the MAAP tool with `tool_choice`.
    ForcedToolNonThinking,
}

/// Fully validated DeepSeek wire preparation for one transport attempt.
///
/// The preparation is the single source of truth for the selected MAAP
/// strategy, capability-limited streaming mode, and exact serialized body.
/// Transport, continuity, retry, and accounting callers must consume this
/// result rather than independently recomputing any of those values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekRequestPreparation {
    /// Effective provider-native MAAP strategy.
    pub strategy: DeepSeekMaapRequestStrategy,
    /// Effective streaming mode after applying model capability policy.
    pub effective_stream: bool,
    /// Exact canonical JSON body to submit to DeepSeek.
    pub body: String,
}

/// DeepSeek-facing MAAP shim function selected for one provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeepSeekMaapShimKind {
    /// Capability-routing function with direct capability fields.
    CapabilityDecision,
    /// Response-only function with direct say fields.
    RespondOnly,
    /// Executable/action function carrying a canonical MAAP batch.
    ActionDispatch,
}

impl DeepSeekMaapShimKind {
    /// Selects the DeepSeek-facing shim surface for the active request.
    fn for_request(_request: &ModelRequest) -> Self {
        Self::ActionDispatch
    }

    /// Returns the provider-facing function name for this shim surface.
    pub(crate) fn tool_name(self) -> &'static str {
        match self {
            Self::CapabilityDecision => DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME,
            Self::RespondOnly => DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME,
            Self::ActionDispatch => DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME,
        }
    }

    /// Parses a DeepSeek provider-facing function name into a shim surface.
    pub(crate) fn from_tool_name(name: &str) -> Option<Self> {
        match name {
            DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME => Some(Self::CapabilityDecision),
            DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME => Some(Self::RespondOnly),
            DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME | OPENAI_MAAP_FUNCTION_TOOL_NAME => {
                Some(Self::ActionDispatch)
            }
            _ => None,
        }
    }
}

/// Validates and prepares one DeepSeek request using its effective strategy.
pub fn prepare_deepseek_chat_completions_request(
    request: &ModelRequest,
    stream: bool,
) -> Result<DeepSeekRequestPreparation> {
    prepare_deepseek_chat_completions_request_with_strategy(
        request,
        stream,
        deepseek_maap_request_strategy(request),
    )
}

/// Validates and prepares one DeepSeek request with an explicit MAAP strategy.
///
/// Explicit strategies are used by the strict MAAP retry. The request's model
/// capabilities still govern whether that strategy is legal and which
/// optional controls may reach the wire.
pub fn prepare_deepseek_chat_completions_request_with_strategy(
    request: &ModelRequest,
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<DeepSeekRequestPreparation> {
    let capabilities = request
        .model_capabilities
        .resolved_for_api(ProviderApiCompatibility::DeepSeekChatCompletions);
    let conservative_unknown =
        capabilities.metadata_policy == ModelCapabilityMetadataPolicy::ConservativeUnknown;
    if strategy != DeepSeekMaapRequestStrategy::NoTool && !capabilities.function_tools {
        return Err(MezError::invalid_args(format!(
            "DeepSeek model `{}` does not support function tools required by this request",
            request.model
        )));
    }
    if strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking
        && !capabilities.forced_tool_choice
    {
        return Err(MezError::invalid_args(format!(
            "DeepSeek model `{}` does not support forced tool choice required by this request",
            request.model
        )));
    }
    if request.max_output_tokens.is_some_and(|tokens| tokens > 0) && !capabilities.max_output_tokens
    {
        return Err(MezError::invalid_args(format!(
            "DeepSeek model `{}` does not support max output-token control",
            request.model
        )));
    }

    let requested_thinking = strategy != DeepSeekMaapRequestStrategy::ForcedToolNonThinking
        && deepseek_thinking_enabled_for_request(request);
    if requested_thinking && !capabilities.native_thinking && !conservative_unknown {
        return Err(MezError::invalid_args(format!(
            "DeepSeek model `{}` does not support native thinking requested by this request",
            request.model
        )));
    }
    let effective_thinking = requested_thinking && capabilities.native_thinking;
    let effective_reasoning_effort = if effective_thinking {
        request
            .reasoning_effort
            .as_deref()
            .filter(|effort| !effort.is_empty())
    } else {
        None
    };
    if let Some(reasoning_effort) = effective_reasoning_effort
        && !capabilities.supports_reasoning_effort(reasoning_effort)
    {
        return Err(MezError::invalid_args(format!(
            "DeepSeek model `{}` does not support reasoning effort `{reasoning_effort}`",
            request.model
        )));
    }
    if stream && !capabilities.streaming && !conservative_unknown {
        return Err(MezError::invalid_args(format!(
            "DeepSeek model `{}` does not support streaming requested by this request",
            request.model
        )));
    }
    let effective_stream = stream && capabilities.streaming;
    let body = deepseek_chat_completions_request_body(
        request,
        effective_stream,
        strategy,
        &capabilities,
        effective_thinking,
        effective_reasoning_effort,
    )?;
    Ok(DeepSeekRequestPreparation {
        strategy,
        effective_stream,
        body,
    })
}

/// Builds the JSON body for a DeepSeek request with an explicit MAAP strategy.
pub fn deepseek_chat_completions_request_body_with_strategy(
    request: &ModelRequest,
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<String> {
    Ok(prepare_deepseek_chat_completions_request_with_strategy(request, stream, strategy)?.body)
}

/// Renders one already-validated DeepSeek request body.
fn deepseek_chat_completions_request_body(
    request: &ModelRequest,
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
    capabilities: &ModelCapabilities,
    thinking_enabled: bool,
    reasoning_effort: Option<&str>,
) -> Result<String> {
    let mut messages = Vec::with_capacity(request.messages.len());
    let mut pending_tool_call_ids = Vec::new();
    for message in &request.messages {
        if let Some(event) = ProviderTranscriptEvent::from_transcript_content(&message.content) {
            if event.provider_id() == "deepseek" {
                match &event {
                    ProviderTranscriptEvent::DeepSeekAssistantToolCall { .. } => {
                        complete_pending_deepseek_tool_calls(
                            &mut messages,
                            &mut pending_tool_call_ids,
                        );
                        pending_tool_call_ids = event.deepseek_tool_call_ids();
                        messages.push(deepseek_provider_transcript_event_message(&event));
                    }
                    ProviderTranscriptEvent::DeepSeekToolResult { tool_call_id, .. } => {
                        let Some(index) = pending_tool_call_ids
                            .iter()
                            .position(|pending_id| pending_id == tool_call_id)
                        else {
                            continue;
                        };
                        pending_tool_call_ids.remove(index);
                        messages.push(deepseek_provider_transcript_event_message(&event));
                    }
                    ProviderTranscriptEvent::OpenAiResponseOutput { .. }
                    | ProviderTranscriptEvent::OpenAiFunctionCallOutput { .. } => {}
                }
            }
            continue;
        }
        complete_pending_deepseek_tool_calls(&mut messages, &mut pending_tool_call_ids);
        let (role, content) = match message.role {
            ModelMessageRole::System => ("system", message.content.clone()),
            ModelMessageRole::User => ("user", message.content.clone()),
            ModelMessageRole::Assistant => ("assistant", message.content.clone()),
            ModelMessageRole::Developer | ModelMessageRole::Context | ModelMessageRole::Tool => (
                "user",
                format!(
                    "[Mezzanine context; not user-authored]\n{}",
                    message.content
                ),
            ),
        };
        messages.push(serde_json::json!({
            "role": role,
            "content": content
        }));
    }
    complete_pending_deepseek_tool_calls(&mut messages, &mut pending_tool_call_ids);
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": messages,
        "stream": stream,
    });
    if let Some(max_output_tokens) = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .filter(|_| capabilities.max_output_tokens)
    {
        body["max_tokens"] = serde_json::json!(max_output_tokens);
    }
    if request.interaction_kind.expects_structured_json() {
        body["response_format"] = serde_json::json!({"type": "json_object"});
    }
    if let Some(temperature) = request
        .temperature
        .as_deref()
        .and_then(|t| t.parse::<f64>().ok())
        .filter(|t| t.is_finite())
    {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(stop) = request.stop.as_ref().filter(|s| !s.is_empty()) {
        body["stop"] = serde_json::json!(stop);
    }
    if capabilities.native_thinking
        && (strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking
            || request.thinking_enabled == Some(false))
    {
        body["thinking"] = serde_json::json!({"type": "disabled"});
    } else if thinking_enabled {
        body["thinking"] = serde_json::json!({"type": "enabled"});
        if let Some(reasoning_effort) = reasoning_effort {
            let deepseek_effort = deepseek_reasoning_effort(reasoning_effort);
            body["reasoning_effort"] = serde_json::json!(deepseek_effort);
        }
    }
    if capabilities.function_tools && strategy != DeepSeekMaapRequestStrategy::NoTool {
        let shim_kind = DeepSeekMaapShimKind::for_request(request);
        if strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking {
            body["tool_choice"] = deepseek_maap_tool_choice(shim_kind);
        }
        let maap_tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": shim_kind.tool_name(),
                "description": chat_completions_maap_tool_description(request, shim_kind),
                "parameters": deepseek_maap_tool_schema(request, shim_kind),
                "strict": false
            }
        });
        body["tools"] = serde_json::json!([maap_tool]);
    }
    serde_json::to_string(&body).map_err(|error| {
        MezError::invalid_state(format!(
            "DeepSeek Chat Completions request encoding failed: {error}"
        ))
    })
}

/// Checks native-message continuity without blocking valid DeepSeek requests.
///
/// Each body is reconstructed from durable chronology using the request's own
/// strategy and effective stream mode before the common prefix check runs.
pub fn prepare_deepseek_request_prefix_extension(
    request: &mut ModelRequest,
    previous: Option<&ModelRequest>,
    cache_namespace: &str,
    stream: bool,
) -> ProviderRequestAssemblyResult<()> {
    let current = prepare_deepseek_chat_completions_request(request, stream)?;
    let previous_preparation = previous
        .map(|previous| prepare_deepseek_chat_completions_request(previous, stream))
        .and_then(Result::ok);
    let previous_body = previous_preparation
        .as_ref()
        .map(|preparation| preparation.body.as_str());
    let previous_api_shape = previous_preparation.as_ref().map_or_else(
        || {
            format!(
                "deepseek-chat-completions;stream={};strategy={:?}",
                current.effective_stream, current.strategy
            )
        },
        |previous| {
            format!(
                "deepseek-chat-completions;stream={};strategy={:?}",
                previous.effective_stream, previous.strategy
            )
        },
    );
    prepare_provider_native_request_prefix_extension(
        request,
        previous,
        ProviderNativeRequestContinuity {
            cache_namespace,
            provider_label: "DeepSeek Chat Completions",
            current_api_shape: &format!(
                "deepseek-chat-completions;stream={};strategy={:?}",
                current.effective_stream, current.strategy
            ),
            previous_api_shape: &previous_api_shape,
            input_field: "messages",
            current_body: &current.body,
            previous_body,
        },
    )
}

/// Completes a DeepSeek assistant tool-call message before any later message.
///
/// DeepSeek requires every declared tool call to receive an immediately
/// following tool result. Restored or partially persisted history can contain
/// a late result, which is not sufficient even when its id exists elsewhere
/// in the request. Synthetic results close only the still-pending calls; late
/// unmatched results are ignored by the renderer.
fn complete_pending_deepseek_tool_calls(
    messages: &mut Vec<serde_json::Value>,
    pending_tool_call_ids: &mut Vec<String>,
) {
    for tool_call_id in pending_tool_call_ids.drain(..) {
        messages.push(serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": "The prior tool call has no recorded result. Continue from the available transcript and emit a new MAAP action batch if further work is needed."
        }));
    }
}

/// Renders a hidden provider transcript event as a DeepSeek-native message.
fn deepseek_provider_transcript_event_message(
    event: &ProviderTranscriptEvent,
) -> serde_json::Value {
    match event {
        ProviderTranscriptEvent::OpenAiResponseOutput { .. }
        | ProviderTranscriptEvent::OpenAiFunctionCallOutput { .. } => serde_json::Value::Null,
        ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content,
            reasoning_content,
            tool_calls,
        } => serde_json::json!({
            "role": "assistant",
            "content": content,
            "reasoning_content": reasoning_content.as_deref().unwrap_or_default(),
            "tool_calls": tool_calls,
        }),
        ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id,
            content,
        } => serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }),
    }
}

/// Returns the DeepSeek MAAP strategy for one model request.
///
/// Thinking mode can use function tools only when DeepSeek chooses the tool
/// itself. When reasoning is configured, Mezzanine therefore exposes the MAAP
/// tool without forcing `tool_choice` and falls back to strict non-thinking
/// mode only if DeepSeek returns prose instead of a MAAP batch.
///
/// Repair retries inherit the original request's thinking strategy. If the
/// original turn used thinking, the first repair attempt also uses thinking
/// so the model can reason about the validation error and emit a corrected
/// batch. The provider's internal `AutoToolThinking`→`ForcedToolNonThinking`
/// fallback still catches prose responses that decline the tool call.
pub fn deepseek_maap_request_strategy(request: &ModelRequest) -> DeepSeekMaapRequestStrategy {
    if request.interaction_kind.expects_structured_json()
        || request.allowed_actions.actions.is_empty()
    {
        return DeepSeekMaapRequestStrategy::NoTool;
    }
    if request.interaction_kind == ModelInteractionKind::CapabilityDecision
        || request.allowed_actions == AllowedActionSet::say_only()
    {
        return DeepSeekMaapRequestStrategy::ForcedToolNonThinking;
    }
    if request.model_capabilities.metadata_policy
        == ModelCapabilityMetadataPolicy::ConservativeUnknown
    {
        return DeepSeekMaapRequestStrategy::ForcedToolNonThinking;
    }
    if request.thinking_enabled == Some(false) {
        return DeepSeekMaapRequestStrategy::ForcedToolNonThinking;
    }
    if deepseek_thinking_enabled_for_request(request) {
        DeepSeekMaapRequestStrategy::AutoToolThinking
    } else {
        DeepSeekMaapRequestStrategy::ForcedToolNonThinking
    }
}

/// Returns whether DeepSeek thinking mode is active for this request.
pub(crate) fn deepseek_thinking_enabled_for_request(request: &ModelRequest) -> bool {
    request.thinking_enabled == Some(true)
        || (request.thinking_enabled != Some(false)
            && request
                .reasoning_effort
                .as_deref()
                .is_some_and(|effort| !effort.is_empty()))
}

/// Returns the DeepSeek stream flag after accounting for MAAP tool strategy.
///
/// The streaming parser now accumulates tool-call argument deltas, so MAAP
/// tool requests can use streaming when the provider object has it enabled.
pub fn deepseek_effective_stream(stream: bool, _strategy: DeepSeekMaapRequestStrategy) -> bool {
    stream
}

/// Reports whether a DeepSeek thinking request should retry with a forced MAAP
/// tool after returning no action batch.
pub fn deepseek_should_retry_with_forced_maap(
    request: &ModelRequest,
    strategy: DeepSeekMaapRequestStrategy,
    has_action_batch: bool,
) -> bool {
    strategy == DeepSeekMaapRequestStrategy::AutoToolThinking
        && !request.interaction_kind.expects_structured_json()
        && !request.allowed_actions.actions.is_empty()
        && !has_action_batch
}

/// Returns the DeepSeek tool choice that forces the MAAP function call.
///
/// # Behavior
/// DeepSeek's Chat Completions API defaults `tool_choice` to `auto` when tools
/// are present, which allows a prose answer instead of a MAAP action batch.
/// Mezzanine requires a structured action batch for every non-auto-sizing
/// provider turn. This helper is therefore reserved for strict fallback
/// requests with thinking disabled; thinking-mode MAAP requests omit
/// `tool_choice` and let DeepSeek choose the advertised MAAP tool.
fn deepseek_maap_tool_choice(shim_kind: DeepSeekMaapShimKind) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": shim_kind.tool_name()
        }
    })
}

/// Builds concise provider-facing guidance for chat-completions MAAP tools.
///
/// DeepSeek and named OpenAI-compatible chat-completions backends share the
/// same local action protocol surface, so they should receive the same
/// function-call discipline, capability-routing guidance, and anti-pattern
/// corrections. Provider-specific behavior such as thinking-mode tool-choice
/// strategy still lives outside this shared prompt text.
fn chat_completions_maap_tool_description(
    _request: &ModelRequest,
    _shim_kind: DeepSeekMaapShimKind,
) -> String {
    crate::schema::maap_cache_stable_action_batch_description()
}

/// Builds the DeepSeek shim argument schema for the selected function.
fn deepseek_maap_tool_schema(
    request: &ModelRequest,
    _shim_kind: DeepSeekMaapShimKind,
) -> serde_json::Value {
    deepseek_maap_action_batch_schema(&request.allowed_actions, &request.available_mcp_tools)
}

/// Maps Mezzanine reasoning effort levels to DeepSeek-supported values.
fn deepseek_reasoning_effort(effort: &str) -> &'static str {
    match effort {
        "low" | "medium" | "high" => "high",
        "xhigh" | "max" => "max",
        _ => "high",
    }
}

/// Builds the DeepSeek MAAP argument schema.
fn deepseek_maap_action_batch_schema(
    allowed_actions: &AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
) -> serde_json::Value {
    let mut schema = maap_action_batch_schema(allowed_actions, available_mcp_tools);
    deepseek_prune_unsupported_schema_keywords(&mut schema);
    deepseek_replace_apply_patch_description(&mut schema);
    schema
}

/// Replaces the shared long-form apply_patch `patch` field description with a
/// shorter, more explicit DeepSeek-optimized version. DeepSeek models parse
/// compact schema descriptions more reliably than long paragraphs.
fn deepseek_replace_apply_patch_description(schema: &mut serde_json::Value) {
    let Some(actions) = schema
        .pointer_mut("/properties/actions/items/anyOf")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for action in actions {
        let Some(props) = action
            .pointer("/properties/type/enum/0")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if props != "apply_patch" {
            continue;
        }
        if let Some(patch_desc) = action.pointer_mut("/properties/patch/description") {
            *patch_desc = serde_json::json!(
                "Mezzanine patch text. Must start with \"*** Begin Patch\" and end with \"*** End Patch\". File directives: \"*** Update File: <path>\", \"*** Add File: <path>\", \"*** Delete File: <path>\". Use relative paths only (no absolute, no ..). Hunks begin with \"@@\" headers, optionally with anchors like \"@@ fn name\"; whole-file replacement uses \"@@ replace whole file\" with only + lines. Hunk lines use exact prefixes: space for context, - for removed, + for added. Copy old/context lines verbatim from current file content; never infer code. Example valid patch: *** Begin Patch\\n*** Update File: src/lib.rs\\n@@ fn main\\n let x = 1;\\n+let y = 2;\\n*** End Patch\\n. WRONG: *** Replace File. Right: *** Update File with anchored hunks. WRONG: --- a/file or +++ b/file headers, diff --git format, or raw unified diffs."
            );
        }
        break;
    }
}

/// Removes JSON Schema hints that DeepSeek documents as unsupported by strict
/// function calling and that add noise even on the default non-strict endpoint.
fn deepseek_prune_unsupported_schema_keywords(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            let is_patch_field = object
                .get("description")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|desc| desc.contains("*** Begin Patch"));
            if !is_patch_field {
                object.remove("minLength");
                object.remove("maxLength");
                object.remove("minItems");
                object.remove("maxItems");
            }
            for child in object.values_mut() {
                deepseek_prune_unsupported_schema_keywords(child);
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                deepseek_prune_unsupported_schema_keywords(child);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        ContextSourceKind, ModelCapabilities, ModelMessage, PROVIDER_TRANSCRIPT_EVENT_MARKER,
    };

    /// Returns the complete model-declared DeepSeek capabilities shared by the
    /// known Pro and Flash catalog records.
    fn known_deepseek_capabilities() -> ModelCapabilities {
        ModelCapabilities {
            metadata_policy: ModelCapabilityMetadataPolicy::ModelMetadata,
            native_thinking: true,
            supported_reasoning_efforts: vec!["high".to_string(), "max".to_string()],
            reasoning_efforts_explicit: true,
            function_tools: true,
            forced_tool_choice: true,
            streaming: true,
            max_output_tokens: true,
        }
    }

    /// Builds a minimal DeepSeek model request for provider-shape tests.
    fn deepseek_test_request(messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            model_capabilities: Default::default(),
            max_input_tokens: None,
            reasoning_effort: Some("high".to_string()),
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
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

    /// Verifies the known DeepSeek Pro and Flash records produce the complete
    /// native thinking request shape from their typed model capabilities.
    ///
    /// Both catalog records advertise the same wire features even though they
    /// carry different token limits elsewhere. Preparation must preserve the
    /// requested stream mode, map `xhigh` to DeepSeek's `max`, include the
    /// explicit output cap, and advertise the MAAP tool without forcing it
    /// while thinking is active.
    #[test]
    fn known_deepseek_pro_and_flash_prepare_native_thinking_bodies() {
        for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
            let mut request = deepseek_test_request(Vec::new());
            request.model = model.to_string();
            request.model_capabilities = known_deepseek_capabilities();
            request.reasoning_effort = Some("xhigh".to_string());
            request.thinking_enabled = Some(true);
            request.max_output_tokens = Some(4096);

            let preparation = prepare_deepseek_chat_completions_request(&request, true).unwrap();
            let body: serde_json::Value = serde_json::from_str(&preparation.body).unwrap();

            assert_eq!(
                preparation.strategy,
                DeepSeekMaapRequestStrategy::AutoToolThinking,
                "{model}"
            );
            assert!(preparation.effective_stream, "{model}");
            assert_eq!(body["model"], model);
            assert_eq!(body["stream"], true);
            assert_eq!(body["thinking"]["type"], "enabled");
            assert_eq!(body["reasoning_effort"], "max");
            assert_eq!(body["max_tokens"], 4096);
            assert!(body.get("tool_choice").is_none(), "{model}: {body}");
            assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
        }
    }

    /// Verifies unknown DeepSeek models retain only the conservative MAAP and
    /// output-bound compatibility floor in their exact provider body.
    ///
    /// Untrusted model metadata must not leak requested thinking, reasoning,
    /// or streaming controls. It must still force the sole MAAP function and
    /// retain `max_tokens`, because those are required for deterministic
    /// action dispatch and bounded-output recovery on unknown DeepSeek models.
    #[test]
    fn conservative_unknown_deepseek_preparation_suppresses_optional_controls() {
        let mut request = deepseek_test_request(Vec::new());
        request.model = "deepseek-unlisted".to_string();
        request.model_capabilities = ModelCapabilities::conservative_unknown_deepseek();
        request.thinking_enabled = Some(true);
        request.reasoning_effort = Some("max".to_string());
        request.max_output_tokens = Some(2048);

        let preparation = prepare_deepseek_chat_completions_request(&request, true).unwrap();
        let body: serde_json::Value = serde_json::from_str(&preparation.body).unwrap();

        assert_eq!(
            preparation.strategy,
            DeepSeekMaapRequestStrategy::ForcedToolNonThinking
        );
        assert!(!preparation.effective_stream);
        assert_eq!(body["stream"], false);
        assert!(body.get("thinking").is_none(), "{body}");
        assert!(body.get("reasoning_effort").is_none(), "{body}");
        assert_eq!(body["max_tokens"], 2048);
        assert_eq!(
            body["tool_choice"]["function"]["name"],
            DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME
        );
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
    }

    /// Verifies model-declared capability gaps reject request combinations
    /// that cannot be represented safely before any transport owns the body.
    ///
    /// Function tools, named forced choice, native thinking, reasoning levels,
    /// and output-token controls are independently validated so adapters,
    /// continuity, retries, and accounting all fail with the same diagnostic.
    #[test]
    fn deepseek_preparation_rejects_unsupported_request_controls() {
        let mut missing_tools = deepseek_test_request(Vec::new());
        missing_tools.model_capabilities = known_deepseek_capabilities();
        missing_tools.model_capabilities.function_tools = false;
        assert!(
            prepare_deepseek_chat_completions_request(&missing_tools, false)
                .unwrap_err()
                .message()
                .contains("does not support function tools")
        );

        let mut missing_forced_choice = deepseek_test_request(Vec::new());
        missing_forced_choice.model_capabilities = known_deepseek_capabilities();
        missing_forced_choice.model_capabilities.forced_tool_choice = false;
        missing_forced_choice.thinking_enabled = Some(false);
        assert!(
            prepare_deepseek_chat_completions_request(&missing_forced_choice, false)
                .unwrap_err()
                .message()
                .contains("does not support forced tool choice")
        );

        let mut missing_thinking = deepseek_test_request(Vec::new());
        missing_thinking.model_capabilities = known_deepseek_capabilities();
        missing_thinking.model_capabilities.native_thinking = false;
        missing_thinking.thinking_enabled = Some(true);
        assert!(
            prepare_deepseek_chat_completions_request(&missing_thinking, false)
                .unwrap_err()
                .message()
                .contains("does not support native thinking")
        );

        let mut unsupported_effort = deepseek_test_request(Vec::new());
        unsupported_effort.model_capabilities = known_deepseek_capabilities();
        unsupported_effort
            .model_capabilities
            .supported_reasoning_efforts = vec!["high".into()];
        unsupported_effort.reasoning_effort = Some("max".to_string());
        assert!(
            prepare_deepseek_chat_completions_request(&unsupported_effort, false)
                .unwrap_err()
                .message()
                .contains("does not support reasoning effort `max`")
        );

        let mut missing_streaming = deepseek_test_request(Vec::new());
        missing_streaming.model_capabilities = known_deepseek_capabilities();
        missing_streaming.model_capabilities.streaming = false;
        assert!(
            prepare_deepseek_chat_completions_request(&missing_streaming, true)
                .unwrap_err()
                .message()
                .contains("does not support streaming")
        );

        let mut missing_output_control = deepseek_test_request(Vec::new());
        missing_output_control.model_capabilities = known_deepseek_capabilities();
        missing_output_control.model_capabilities.max_output_tokens = false;
        missing_output_control.max_output_tokens = Some(1024);
        assert!(
            prepare_deepseek_chat_completions_request(&missing_output_control, false)
                .unwrap_err()
                .message()
                .contains("does not support max output-token control")
        );
    }

    /// Verifies DeepSeek accepts append-only native messages and warns on a
    /// changed prior native item without blocking provider dispatch.
    #[test]
    fn deepseek_request_chain_requires_exact_native_message_prefix() {
        let messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "system prompt".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "inspect the repository".to_string(),
            },
        ];
        let mut first = deepseek_test_request(messages.clone());
        prepare_deepseek_request_prefix_extension(&mut first, None, "deepseek:test", false)
            .unwrap();

        let mut appended = first.clone();
        appended.messages.push(ModelMessage {
            role: ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "cwd=/repo".to_string(),
        });
        prepare_deepseek_request_prefix_extension(
            &mut appended,
            Some(&first),
            "deepseek:test",
            false,
        )
        .unwrap();
        let strategy = deepseek_maap_request_strategy(&first);
        let first_body: serde_json::Value = serde_json::from_str(
            &deepseek_chat_completions_request_body_with_strategy(&first, false, strategy).unwrap(),
        )
        .unwrap();
        let appended_strategy = deepseek_maap_request_strategy(&appended);
        let appended_body: serde_json::Value = serde_json::from_str(
            &deepseek_chat_completions_request_body_with_strategy(
                &appended,
                false,
                appended_strategy,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            first_body["messages"].as_array().unwrap().as_slice(),
            &appended_body["messages"].as_array().unwrap()[..2]
        );

        let mut rewritten = deepseek_test_request(vec![
            messages[0].clone(),
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "rewrite the repository request".to_string(),
            },
        ]);
        prepare_deepseek_request_prefix_extension(
            &mut rewritten,
            Some(&first),
            "deepseek:test",
            false,
        )
        .unwrap();
        assert_eq!(
            rewritten.messages.provider_continuity_warning(),
            Some("canonical_input_rewritten")
        );
        let mut next = rewritten.clone();
        prepare_deepseek_request_prefix_extension(
            &mut next,
            Some(&rewritten),
            "deepseek:test",
            false,
        )
        .unwrap();
        assert_eq!(next.messages.provider_continuity_warning(), None);
    }

    /// Verifies DeepSeek repair retries inherit the original request's thinking
    /// strategy so the model can reason about validation errors.
    ///
    /// When the original action-execution turn used thinking, the repair
    /// retry keeps thinking enabled and does not force `tool_choice`. The
    /// provider's internal `AutoToolThinking`→`ForcedToolNonThinking` fallback
    /// still catches a prose response that declines the tool call.
    #[test]
    fn deepseek_repair_request_inherits_thinking_strategy() {
        let mut request = deepseek_test_request(Vec::new());
        request.interaction_kind = ModelInteractionKind::MaapRepair;

        let strategy = deepseek_maap_request_strategy(&request);
        let body_text =
            deepseek_chat_completions_request_body_with_strategy(&request, true, strategy).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("tool_choice").is_none());
        assert!(
            body["tools"]
                .as_array()
                .is_some_and(|tools| tools.len() == 1)
        );
    }

    /// Verifies DeepSeek repair retries disable thinking when the original
    /// request did not use it.
    #[test]
    fn deepseek_repair_request_without_thinking_stays_disabled() {
        let mut request = deepseek_test_request(Vec::new());
        request.reasoning_effort = None;
        request.thinking_enabled = Some(false);
        request.interaction_kind = ModelInteractionKind::MaapRepair;

        let strategy = deepseek_maap_request_strategy(&request);
        let body_text =
            deepseek_chat_completions_request_body_with_strategy(&request, true, strategy).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();

        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(
            body["tool_choice"]["function"]["name"],
            DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME
        );
    }

    /// Verifies hidden provider transcript events replay as DeepSeek-native
    /// assistant and tool messages in their original context position.
    ///
    /// The request body must not leak Mezzanine's hidden transcript marker to
    /// DeepSeek. Instead, the adapter restores the documented message shape:
    /// assistant content, `reasoning_content`, `tool_calls`, then a matching
    /// `role: tool` result before the next user turn.
    #[test]
    fn deepseek_request_replays_hidden_provider_transcript_events_as_native_messages() {
        let tool_calls = vec![serde_json::json!({
            "id": "call_1",
            "type": "function",
            "function": {
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "arguments": "{\"actions\":[]}"
            }
        })];
        let assistant_event = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: "".to_string(),
            reasoning_content: Some("The prior turn needed a shell command.".to_string()),
            tool_calls,
        };
        let tool_event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call_1".to_string(),
            content: "[action_result a1 shell_command succeeded]\nexit_code: 0\noutput:\ndeepseek-live-output-sentinel"
                .to_string(),
        };
        let request = deepseek_test_request(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "system prompt".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::TranscriptUser,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "previous request".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                content: assistant_event.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                content: tool_event.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "continue".to_string(),
            },
        ]);

        let strategy = deepseek_maap_request_strategy(&request);
        let body_text =
            deepseek_chat_completions_request_body_with_strategy(&request, true, strategy).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("tool_choice").is_none());
        assert!(!body_text.contains(PROVIDER_TRANSCRIPT_EVENT_MARKER.trim()));
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(
            messages[2]["reasoning_content"],
            "The prior turn needed a shell command."
        );
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_1");
        assert_eq!(
            messages[3]["content"],
            "[action_result a1 shell_command succeeded]\nexit_code: 0\noutput:\ndeepseek-live-output-sentinel"
        );
        assert!(
            !messages[3]["content"]
                .as_str()
                .unwrap()
                .contains("historical_output: omitted")
        );
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"], "continue");
    }

    /// Verifies malformed restored DeepSeek history is made protocol-valid
    /// before the provider receives the first request.
    ///
    /// A matching tool result later in the transcript does not satisfy
    /// DeepSeek's adjacency requirement. The renderer must insert a synthetic
    /// result immediately after the assistant call, preserve the intervening
    /// user message, and omit the now-unmatched late result.
    #[test]
    fn deepseek_request_completes_non_adjacent_tool_call_results() {
        let assistant_event = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![serde_json::json!({
                "id": "call_late",
                "type": "function",
                "function": {
                    "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                    "arguments": "{}"
                }
            })],
        };
        let late_tool_event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call_late".to_string(),
            content: "late result that DeepSeek cannot accept".to_string(),
        };
        let request = deepseek_test_request(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                content: assistant_event.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::TranscriptUser,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "intervening user message".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                content: late_tool_event.to_transcript_content(),
            },
        ]);

        let strategy = deepseek_maap_request_strategy(&request);
        let body_text =
            deepseek_chat_completions_request_body_with_strategy(&request, true, strategy).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["reasoning_content"], "");
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_late");
        assert!(
            messages[1]["content"]
                .as_str()
                .is_some_and(|content| content.contains("no recorded result"))
        );
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"], "intervening user message");
        assert!(!body_text.contains("late result that DeepSeek cannot accept"));
    }

    /// Verifies configured DeepSeek base URLs expand to the current documented
    /// Chat Completions and Models endpoints.
    ///
    /// User-facing configuration names this setting `base_url`, so callers must
    /// be able to provide `https://api.deepseek.com` exactly as shown in the
    /// DeepSeek SDK examples. Existing endpoint URLs remain accepted so tests
    /// and advanced users can still target a proxy or explicit route.
    #[test]
    fn deepseek_base_url_derives_documented_chat_and_models_endpoints() {
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url("https://api.deepseek.com").unwrap(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url("https://api.deepseek.com/").unwrap(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url(
                "https://api.deepseek.com/chat/completions"
            )
            .unwrap(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url("https://proxy.example/models")
                .unwrap(),
            "https://proxy.example/chat/completions"
        );
        assert_eq!(
            deepseek_models_endpoint_for_base_url("https://api.deepseek.com").unwrap(),
            "https://api.deepseek.com/models"
        );
    }
}
