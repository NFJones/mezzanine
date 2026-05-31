//! DeepSeek Chat Completions request and response helpers.
//!
//! This module owns DeepSeek-specific request body construction, MAAP tool
//! strategy handling, response parsing, and model-list request construction.
//! Provider dispatch remains in the parent module so shared trait wiring stays
//! centralized.

use super::errors::provider_maap_parse_error;
use super::schema::maap_action_batch_schema;
use super::{
    AgentCapability, AllowedActionSet, DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME,
    DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME, DEEPSEEK_MODELS_ENDPOINT,
    DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME, DeepSeekMaapRequestStrategy, MaapBatch,
    McpPromptTool, MezError, ModelInteractionKind, ModelMessageRole, ModelRequest, ModelResponse,
    ModelTokenUsage, OPENAI_MAAP_FUNCTION_TOOL_NAME, ProviderCapabilities, ProviderHttpRequest,
    ProviderHttpResponse, ProviderTranscriptEvent, Result, parse_fenced_maap_action_batch_for_turn,
    parse_maap_action_batch_json_for_turn, provider_quota_usage_from_headers, validate_non_empty,
};
use std::collections::BTreeMap;

/// DeepSeek-facing MAAP shim function selected for one provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeepSeekMaapShimKind {
    /// Capability-routing function with direct capability fields.
    CapabilityDecision,
    /// Response-only function with direct say fields.
    RespondOnly,
    /// Executable/action function carrying a canonical MAAP batch.
    ActionDispatch,
}

impl DeepSeekMaapShimKind {
    /// Selects the DeepSeek-facing shim surface for the active request.
    fn for_request(request: &ModelRequest) -> Self {
        if request.allowed_actions == AllowedActionSet::capability_decision() {
            return Self::CapabilityDecision;
        }
        if request.allowed_actions == AllowedActionSet::say_only()
            || request.allowed_actions
                == AllowedActionSet::for_capability(AgentCapability::RespondOnly)
        {
            return Self::RespondOnly;
        }
        Self::ActionDispatch
    }

    /// Returns the provider-facing function name for this shim surface.
    fn tool_name(self) -> &'static str {
        match self {
            Self::CapabilityDecision => DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME,
            Self::RespondOnly => DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME,
            Self::ActionDispatch => DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME,
        }
    }

    /// Parses a DeepSeek provider-facing function name into a shim surface.
    fn from_tool_name(name: &str) -> Option<Self> {
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

/// Builds a DeepSeek Chat Completions HTTP request.
pub fn build_deepseek_chat_completions_http_request(
    request: &ModelRequest,
    api_key: &str,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    build_deepseek_chat_completions_http_request_with_strategy(
        request,
        Some(api_key),
        endpoint,
        stream,
        timeout_ms,
        deepseek_maap_request_strategy(request),
    )
}

/// Derives the DeepSeek Chat Completions endpoint from a configured base URL.
pub(super) fn deepseek_chat_completions_endpoint_for_base_url(base_url: &str) -> Result<String> {
    validate_non_empty("DeepSeek provider base URL", base_url)?;
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/chat/completions") {
        return Ok(base_url.to_string());
    }
    if let Some(prefix) = base_url.strip_suffix("/models") {
        return Ok(format!("{prefix}/chat/completions"));
    }
    Ok(format!("{base_url}/chat/completions"))
}

/// Builds a DeepSeek Chat Completions HTTP request with an explicit MAAP strategy.
pub(super) fn build_deepseek_chat_completions_http_request_with_strategy(
    request: &ModelRequest,
    api_key: Option<&str>,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("DeepSeek provider bearer credential", api_key)?;
    }
    validate_non_empty("DeepSeek Chat Completions endpoint", endpoint)?;
    if timeout_ms == 0 {
        return Err(MezError::invalid_args(
            "DeepSeek provider timeout must be greater than zero",
        ));
    }
    let stream = deepseek_effective_stream(stream, strategy);
    let body = deepseek_chat_completions_request_body_with_strategy(request, stream, strategy)?;
    let mut headers = BTreeMap::new();
    headers.insert(
        "Accept".to_string(),
        if stream {
            "text/event-stream".to_string()
        } else {
            "application/json".to_string()
        },
    );
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if let Some(api_key) = api_key {
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    }
    Ok(ProviderHttpRequest {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers,
        body,
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Builds the JSON body for a DeepSeek request with an explicit MAAP strategy.
fn deepseek_chat_completions_request_body_with_strategy(
    request: &ModelRequest,
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<String> {
    let capabilities =
        ProviderCapabilities::for_api(super::ProviderApiCompatibility::DeepSeekChatCompletions);
    let mut messages = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        if let Some(event) = ProviderTranscriptEvent::from_transcript_content(&message.content) {
            messages.push(deepseek_provider_transcript_event_message(&event));
            continue;
        }
        let role = match message.role {
            ModelMessageRole::System => "system",
            ModelMessageRole::User => "user",
            ModelMessageRole::Assistant => "assistant",
            _ => "user",
        };
        messages.push(serde_json::json!({
            "role": role,
            "content": message.content
        }));
    }
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": messages,
        "stream": stream,
    });
    if let Some(max_output_tokens) = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .filter(|_| capabilities.supports_max_output_tokens)
    {
        body["max_tokens"] = serde_json::json!(max_output_tokens);
    }
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
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
    if strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking
        || request.thinking_enabled == Some(false)
    {
        body["thinking"] = serde_json::json!({"type": "disabled"});
    } else if deepseek_thinking_enabled_for_request(request) {
        body["thinking"] = serde_json::json!({"type": "enabled"});
        if let Some(reasoning_effort) = request
            .reasoning_effort
            .as_deref()
            .filter(|effort| !effort.is_empty())
        {
            let deepseek_effort = deepseek_reasoning_effort(reasoning_effort);
            body["reasoning_effort"] = serde_json::json!(deepseek_effort);
        }
    }
    if capabilities.supports_tool_calls && strategy != DeepSeekMaapRequestStrategy::NoTool {
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

/// Renders a hidden provider transcript event as a DeepSeek-native message.
fn deepseek_provider_transcript_event_message(
    event: &ProviderTranscriptEvent,
) -> serde_json::Value {
    match event {
        ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content,
            reasoning_content,
            tool_calls,
        } => {
            let mut message = serde_json::json!({
                "role": "assistant",
                "content": content,
                "tool_calls": tool_calls,
            });
            if let Some(reasoning_content) =
                reasoning_content.as_deref().filter(|text| !text.is_empty())
            {
                message["reasoning_content"] = serde_json::json!(reasoning_content);
            }
            message
        }
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
pub(super) fn deepseek_maap_request_strategy(
    request: &ModelRequest,
) -> DeepSeekMaapRequestStrategy {
    if request.interaction_kind == ModelInteractionKind::AutoSizing
        || request.allowed_actions.actions.is_empty()
    {
        return DeepSeekMaapRequestStrategy::NoTool;
    }
    if request.interaction_kind == ModelInteractionKind::CapabilityDecision
        || request.allowed_actions == AllowedActionSet::say_only()
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
fn deepseek_thinking_enabled_for_request(request: &ModelRequest) -> bool {
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
pub(super) fn deepseek_effective_stream(
    stream: bool,
    _strategy: DeepSeekMaapRequestStrategy,
) -> bool {
    stream
}

/// Reports whether a DeepSeek thinking request should retry strict MAAP.
pub(super) fn deepseek_should_retry_with_forced_maap(
    request: &ModelRequest,
    strategy: DeepSeekMaapRequestStrategy,
    response: &ModelResponse,
) -> bool {
    strategy == DeepSeekMaapRequestStrategy::AutoToolThinking
        && request.interaction_kind != ModelInteractionKind::AutoSizing
        && !request.allowed_actions.actions.is_empty()
        && response.action_batch.is_none()
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
    request: &ModelRequest,
    shim_kind: DeepSeekMaapShimKind,
) -> String {
    let capability_map = "Capability map: shell=local files, rg/sed/cat, git, builds, tests, shell_command, and apply_patch; network_search=web_search; network_fetch=fetch_url; mcp=mcp_call; subagent=send_message or spawn_agent; config_change=config_change; respond_only=final text only.";
    let anti_examples = "Wrong: say(blocked, \"Need shell capability\"). Right: request_capability(capability=\"shell\", reason=\"Need to inspect repository files\"). Wrong: say(blocked, \"Shell capability is absent\") or say describing what is missing. Right: request_capability for the missing capability immediately. Wrong: *** Replace File. Right: *** Update File with anchored hunks. Wrong: inferred apply_patch old context. Right: copy old/context lines verbatim from read file evidence.";
    let routing_rule = "CRITICAL ROUTING RULE: When request_capability is in the allowed action types and an executable action type is needed but absent, request_capability is the ONLY correct response. Never emit a say action describing, diagnosing, or lamenting the absence of a capability. request_capability IS how you obtain the missing capability within the same turn; emit it immediately without any preceding say.";
    match shim_kind {
        DeepSeekMaapShimKind::CapabilityDecision => format!(
            "Decide the next Mezzanine capability through this function. Return a function call, not prose. The arguments are translated into one internal MAAP/1 request_capability batch unless no external action capability is needed. {routing_rule} If any local or external action would help, choose request_capability via the capability and reason fields only; missing shell, patch, web, MCP, messaging, subagent, or config action surface is not a blocker. {capability_map} {anti_examples}"
        ),
        DeepSeekMaapShimKind::RespondOnly => {
            "Submit one user-facing response through this function. Return a function call, not prose. The arguments are translated into one internal MAAP/1 say action. Only progress, final, or blocked say text is valid; do not request tools or capabilities from this response-only surface.".to_string()
        }
        DeepSeekMaapShimKind::ActionDispatch => format!(
            "Submit exactly one MAAP/1 action batch through this function. Return a function call, not prose. Current allowed action types: {}. Use only the action objects in this function schema. {routing_rule} If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. {capability_map} {anti_examples}",
            request.allowed_actions.action_type_names().join(","),
        ),
    }
}

/// Builds the DeepSeek shim argument schema for the selected function.
fn deepseek_maap_tool_schema(
    request: &ModelRequest,
    shim_kind: DeepSeekMaapShimKind,
) -> serde_json::Value {
    match shim_kind {
        DeepSeekMaapShimKind::CapabilityDecision => deepseek_capability_decision_schema(),
        DeepSeekMaapShimKind::RespondOnly => deepseek_respond_schema(),
        DeepSeekMaapShimKind::ActionDispatch => deepseek_maap_action_batch_schema(
            &request.allowed_actions,
            &request.available_mcp_tools,
        ),
    }
}

/// Builds the compact DeepSeek capability-decision shim schema.
fn deepseek_capability_decision_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "rationale": {
                "type": "string",
                "description": "Terse reason this capability decision is next."
            },
            "capability": {
                "type": "string",
                "enum": AgentCapability::all_names(),
                "description": "Coarse Mezzanine capability to expose next. Use shell for local files, commands, builds, tests, and apply_patch; respond_only only for final text."
            },
            "reason": {
                "type": "string",
                "description": "Brief task-specific reason naming the next concrete action or evidence needed."
            }
        },
        "required": ["rationale", "capability", "reason"],
        "additionalProperties": false
    })
}

/// Builds the compact DeepSeek respond-only shim schema.
fn deepseek_respond_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "rationale": {
                "type": "string",
                "description": "Terse reason this visible response is next."
            },
            "status": {
                "type": "string",
                "enum": ["progress", "final", "blocked"],
                "description": "progress for nonterminal updates, final when complete, blocked when user or external input is required."
            },
            "content_type": {
                "type": "string",
                "enum": ["text/plain; charset=utf-8", "text/markdown; charset=utf-8", "text/x-diff; charset=utf-8"],
                "description": "HTTP-style media type for text."
            },
            "text": {
                "type": "string",
                "description": "User-visible text. Commands and patch blocks here are display-only."
            }
        },
        "required": ["rationale", "status", "text"],
        "additionalProperties": false
    })
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
///
/// DeepSeek strict tool schema enforcement is beta-only, and the default
/// endpoint treats function parameters as guidance that the model may still
/// violate. The shared OpenAI schema advertises the optional `thought` field
/// as required for strict-schema compliance, but that extra nullable field
/// makes DeepSeek more likely to spend output budget on non-action prose or
/// emit a partially cut JSON object. DeepSeek still accepts `thought` if it is
/// returned, but the provider-specific schema keeps the advertised surface to
/// the fields needed to execute the next action batch.
fn deepseek_maap_action_batch_schema(
    allowed_actions: &AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
) -> serde_json::Value {
    let mut schema = maap_action_batch_schema(allowed_actions, available_mcp_tools);
    if let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        properties.remove("thought");
    }
    if let Some(required) = schema
        .get_mut("required")
        .and_then(serde_json::Value::as_array_mut)
    {
        required.retain(|field| field.as_str() != Some("thought"));
    }
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
                "Mezzanine patch text. Must start with \"*** Begin Patch\" and end with \"*** End Patch\". File directives: \"*** Update File: <path>\", \"*** Add File: <path>\", \"*** Delete File: <path>\". Use relative paths only (no absolute, no ..). Hunks begin with \"@@\" headers, optionally with anchors like \"@@ fn name\". Hunk lines use exact prefixes: space for context, - for removed, + for added. Copy old/context lines verbatim from current file content; never infer code. Example valid patch: *** Begin Patch\\n*** Update File: src/lib.rs\\n@@ fn main\\n let x = 1;\\n+let y = 2;\\n*** End Patch\\n. WRONG: --- a/file or +++ b/file headers, diff --git format, or raw unified diffs."
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
            object.remove("minLength");
            object.remove("maxLength");
            object.remove("minItems");
            object.remove("maxItems");
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

/// Parses one successful DeepSeek HTTP response into a model response.
pub(super) fn parse_deepseek_chat_completions_http_response(
    response: ProviderHttpResponse,
    request: &ModelRequest,
    provider_id: &str,
    stream: bool,
) -> Result<ModelResponse> {
    let ProviderHttpResponse { headers, body, .. } = response;
    if stream {
        let mut parsed = parse_deepseek_chat_completions_stream_body(&body, request)?;
        parsed.provider = provider_id.to_string();
        parsed.quota_usage = provider_quota_usage_from_headers(&headers);
        return Ok(parsed);
    }
    let mut parsed = parse_deepseek_chat_completions_response_body(&body, request)?;
    parsed.provider = provider_id.to_string();
    parsed.quota_usage = provider_quota_usage_from_headers(&headers);
    Ok(parsed)
}

/// Parses a DeepSeek Chat Completions non-streaming response body.
fn parse_deepseek_chat_completions_response_body(
    body: &str,
    request: &ModelRequest,
) -> Result<ModelResponse> {
    let root: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!(
            "DeepSeek Chat Completions response body is invalid JSON: {error}"
        ))
    })?;
    let model = root
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&request.model)
        .to_string();
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            MezError::invalid_state("DeepSeek Chat Completions response has no choices array")
        })?;
    let first_choice = choices.first().ok_or_else(|| {
        MezError::invalid_state("DeepSeek Chat Completions response has empty choices array")
    })?;
    let finish_reason = first_choice
        .get("finish_reason")
        .and_then(serde_json::Value::as_str);
    let message = first_choice.get("message").ok_or_else(|| {
        MezError::invalid_state("DeepSeek Chat Completions choice has no message")
    })?;
    let raw_text = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let raw_text = if deepseek_thinking_enabled_for_request(request)
        || request.model.to_ascii_lowercase().contains("r1")
    {
        strip_think_tags(&raw_text)
    } else {
        raw_text
    };
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let raw_text = if raw_text.is_empty() {
        if message.get("tool_calls").is_some() {
            "executing".to_string()
        } else {
            "(empty)".to_string()
        }
    } else {
        raw_text
    };
    let provider_transcript_events =
        deepseek_provider_transcript_events_for_message(message, reasoning_content);
    let action_batch = if deepseek_request_requires_maap(request) {
        match parse_deepseek_maap_action_batch(message, &raw_text, request) {
            Ok(action_batch) => action_batch,
            Err(error) => {
                return Err(deepseek_completion_finish_reason_error(
                    finish_reason,
                    &raw_text,
                    Some(&error),
                    request,
                )
                .unwrap_or(error));
            }
        }
    } else {
        None
    };
    if action_batch.is_none()
        && let Some(error) =
            deepseek_completion_finish_reason_error(finish_reason, &raw_text, None, request)
    {
        return Err(error);
    }
    let usage = root
        .get("usage")
        .map(parse_deepseek_usage)
        .unwrap_or_default();
    Ok(ModelResponse {
        provider: request.provider.clone(),
        model,
        raw_text,
        usage,
        latest_request_usage: None,
        quota_usage: Vec::new(),
        action_batch,
        provider_transcript_events,
    })
}

/// Parses a DeepSeek MAAP action batch from either function-call arguments or
/// a content fallback.
///
/// DeepSeek should normally return the negotiated MAAP tool call. The content
/// fallbacks keep the adapter compatible with proxies or model variants that
/// return compact JSON or a fenced MAAP block despite being asked for a tool.
fn parse_deepseek_maap_action_batch(
    message: &serde_json::Value,
    raw_text: &str,
    request: &ModelRequest,
) -> Result<Option<MaapBatch>> {
    if let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .filter(|tool_calls| !tool_calls.is_empty())
    {
        return parse_deepseek_maap_tool_calls(tool_calls, request);
    }
    parse_deepseek_content_maap_action_batch(raw_text, request)
}

/// Parses a DeepSeek MAAP batch from provider-native function calls.
fn parse_deepseek_maap_tool_calls(
    tool_calls: &[serde_json::Value],
    request: &ModelRequest,
) -> Result<Option<MaapBatch>> {
    let Some((maap_call, shim_kind, tool_name)) = tool_calls.iter().find_map(|call| {
        let tool_name = call
            .pointer("/function/name")
            .and_then(serde_json::Value::as_str)?;
        Some((
            call,
            DeepSeekMaapShimKind::from_tool_name(tool_name)?,
            tool_name,
        ))
    }) else {
        return Ok(None);
    };
    let missing_arguments_raw_text = maap_call.to_string();
    let arguments = maap_call
        .pointer("/function/arguments")
        .and_then(serde_json::Value::as_str)
        .filter(|arguments| !arguments.trim().is_empty())
        .ok_or_else(|| {
            provider_maap_parse_error(
                MezError::invalid_args(format!(
                    "DeepSeek tool call {tool_name} did not include JSON arguments"
                )),
                &missing_arguments_raw_text,
            )
        })?;
    let batch_json = deepseek_shim_arguments_to_maap_json(arguments, shim_kind)
        .map_err(|error| provider_maap_parse_error(error, arguments))?;
    parse_maap_action_batch_json_for_turn(&batch_json, &request.turn_id, &request.agent_id)
        .map(Some)
        .map_err(|error| provider_maap_parse_error(error, &batch_json))
}

/// Translates DeepSeek shim arguments into canonical compact MAAP batch JSON.
fn deepseek_shim_arguments_to_maap_json(
    arguments: &str,
    shim_kind: DeepSeekMaapShimKind,
) -> Result<String> {
    if shim_kind == DeepSeekMaapShimKind::ActionDispatch {
        return Ok(arguments.to_string());
    }
    let value = serde_json::from_str::<serde_json::Value>(arguments).map_err(|error| {
        MezError::invalid_args(format!("DeepSeek shim arguments are invalid JSON: {error}"))
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("DeepSeek shim arguments must be a JSON object"))?;
    let rationale = required_deepseek_shim_string(object, "rationale")?;
    let action = match shim_kind {
        DeepSeekMaapShimKind::CapabilityDecision => serde_json::json!({
            "type": "request_capability",
            "capability": required_deepseek_shim_string(object, "capability")?,
            "reason": required_deepseek_shim_string(object, "reason")?
        }),
        DeepSeekMaapShimKind::RespondOnly => serde_json::json!({
            "type": "say",
            "status": required_deepseek_shim_string(object, "status")?,
            "content_type": object
                .get("content_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("text/plain; charset=utf-8"),
            "text": required_deepseek_shim_string(object, "text")?
        }),
        DeepSeekMaapShimKind::ActionDispatch => unreachable!("handled above"),
    };
    Ok(serde_json::json!({
        "rationale": rationale,
        "actions": [action]
    })
    .to_string())
}

/// Returns one required DeepSeek shim string argument.
fn required_deepseek_shim_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| MezError::invalid_args(format!("DeepSeek shim field {field} is required")))
}

/// Parses a DeepSeek content fallback when no MAAP tool call is present.
fn parse_deepseek_content_maap_action_batch(
    raw_text: &str,
    request: &ModelRequest,
) -> Result<Option<MaapBatch>> {
    let trimmed = raw_text.trim();
    if trimmed.starts_with('{') {
        return parse_maap_action_batch_json_for_turn(trimmed, &request.turn_id, &request.agent_id)
            .map(Some)
            .map_err(|error| provider_maap_parse_error(error, raw_text));
    }
    parse_fenced_maap_action_batch_for_turn(raw_text, &request.turn_id, &request.agent_id)
        .map_err(|error| provider_maap_parse_error(error, raw_text))
}

/// Returns whether this DeepSeek request must produce a provider action batch.
fn deepseek_request_requires_maap(request: &ModelRequest) -> bool {
    request.interaction_kind != ModelInteractionKind::AutoSizing
        && !request.allowed_actions.actions.is_empty()
}

/// Converts terminal DeepSeek finish reasons into runtime-recoverable errors.
fn deepseek_completion_finish_reason_error(
    finish_reason: Option<&str>,
    raw_text: &str,
    parse_error: Option<&MezError>,
    request: &ModelRequest,
) -> Option<MezError> {
    if !deepseek_request_requires_maap(request) {
        return None;
    }
    if finish_reason != Some("length") {
        return None;
    }
    let detail = parse_error
        .map(|error| format!(": {}", error.message()))
        .unwrap_or_default();
    let provider_raw_text = parse_error
        .and_then(MezError::provider_raw_text)
        .unwrap_or(raw_text)
        .to_string();
    Some(
        MezError::invalid_state(format!(
            "DeepSeek Chat Completions response hit max_output_tokens before completing MAAP output{detail}"
        ))
        .with_provider_failure_json(
            serde_json::json!({
                "provider": "deepseek",
                "finish_reason": "length",
                "incomplete_details": {
                    "reason": "max_output_tokens"
                },
                "raw_text_bytes": provider_raw_text.len()
            })
            .to_string(),
        )
        .with_provider_raw_text(provider_raw_text),
    )
}

/// Captures DeepSeek-native assistant tool-call metadata for transcript replay.
fn deepseek_provider_transcript_events_for_message(
    message: &serde_json::Value,
    reasoning_content: Option<String>,
) -> Vec<ProviderTranscriptEvent> {
    let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .filter(|tool_calls| !tool_calls.is_empty())
    else {
        return Vec::new();
    };
    vec![ProviderTranscriptEvent::DeepSeekAssistantToolCall {
        content: message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        reasoning_content,
        tool_calls: tool_calls.clone(),
    }]
}

/// Parses usage statistics from a DeepSeek Chat Completions response.
fn parse_deepseek_usage(usage: &serde_json::Value) -> ModelTokenUsage {
    ModelTokenUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("completion_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: deepseek_reasoning_tokens_from_usage(usage),
        cached_input_tokens: usage
            .get("prompt_cache_hit_tokens")
            .and_then(serde_json::Value::as_u64),
    }
}

/// Extracts DeepSeek reasoning token usage from the documented nested shape.
fn deepseek_reasoning_tokens_from_usage(usage: &serde_json::Value) -> u64 {
    usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .or_else(|| usage.get("reasoning_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// Accumulates one DeepSeek streaming tool-call delta across SSE events.
#[derive(Debug, Default)]
struct DeepSeekStreamToolCall {
    id: String,
    function_name: String,
    arguments: String,
}

/// Parses a DeepSeek Chat Completions streaming (SSE) response body.
///
/// Accumulates content text, reasoning content, and tool-call argument
/// deltas across SSE events. When the stream includes a MAAP function
/// call the accumulated arguments are parsed into an action batch.
fn parse_deepseek_chat_completions_stream_body(
    body: &str,
    request: &ModelRequest,
) -> Result<ModelResponse> {
    let strip_think = deepseek_thinking_enabled_for_request(request)
        || request.model.to_ascii_lowercase().contains("r1");
    let mut text_content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: BTreeMap<u64, DeepSeekStreamToolCall> = BTreeMap::new();
    let mut model: Option<String> = None;
    let mut usage = ModelTokenUsage::default();
    let mut finish_reason: Option<String> = None;

    for line in body.lines() {
        let data = line.strip_prefix("data: ").unwrap_or(line);
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };

        if model.is_none() {
            model = event
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
        if let Some(u) = event.get("usage") {
            usage = parse_deepseek_usage(u);
        }

        let Some(choices) = event.get("choices").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for choice in choices {
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(serde_json::Value::as_str)
            {
                finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(content) = delta.get("content").and_then(serde_json::Value::as_str) {
                text_content.push_str(content);
            }
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str)
            {
                reasoning_content.push_str(reasoning);
            }
            if let Some(tool_deltas) = delta
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
            {
                for tool_delta in tool_deltas {
                    let index = tool_delta
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let acc = tool_calls.entry(index).or_default();
                    if let Some(id) = tool_delta.get("id").and_then(serde_json::Value::as_str) {
                        acc.id = id.to_string();
                    }
                    if let Some(func) = tool_delta.get("function") {
                        if let Some(name) = func.get("name").and_then(serde_json::Value::as_str) {
                            acc.function_name = name.to_string();
                        }
                        if let Some(args) =
                            func.get("arguments").and_then(serde_json::Value::as_str)
                        {
                            acc.arguments.push_str(args);
                        }
                    }
                }
            }
        }
    }

    if strip_think {
        text_content = strip_think_tags(&text_content);
    }

    let model = model.unwrap_or_else(|| request.model.clone());

    let raw_text = if text_content.is_empty() {
        if !tool_calls.is_empty() {
            "executing".to_string()
        } else {
            "(empty)".to_string()
        }
    } else {
        text_content.clone()
    };

    let tool_calls_json: Vec<serde_json::Value> = tool_calls
        .values()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "type": "function",
                "function": {
                    "name": tc.function_name,
                    "arguments": tc.arguments
                }
            })
        })
        .collect();

    let message = serde_json::json!({
        "content": text_content,
        "tool_calls": tool_calls_json
    });

    let reasoning = if reasoning_content.is_empty() {
        None
    } else {
        Some(reasoning_content)
    };
    let provider_transcript_events =
        deepseek_provider_transcript_events_for_message(&message, reasoning);

    let action_batch = if deepseek_request_requires_maap(request) {
        match parse_deepseek_maap_action_batch(&message, &raw_text, request) {
            Ok(action_batch) => action_batch,
            Err(error) => {
                return Err(deepseek_completion_finish_reason_error(
                    finish_reason.as_deref(),
                    &raw_text,
                    Some(&error),
                    request,
                )
                .unwrap_or(error));
            }
        }
    } else {
        None
    };

    if action_batch.is_none()
        && let Some(error) = deepseek_completion_finish_reason_error(
            finish_reason.as_deref(),
            &raw_text,
            None,
            request,
        )
    {
        return Err(error);
    }

    Ok(ModelResponse {
        provider: request.provider.clone(),
        model,
        raw_text,
        usage,
        latest_request_usage: None,
        quota_usage: Vec::new(),
        action_batch,
        provider_transcript_events,
    })
}

/// Strips `<think>...</think>` tags and their content from a response string.
///
/// R1 reasoning variants wrap internal chain-of-thought in these tags. The
/// content between them is useful for verbose-mode logging but must not
/// appear in raw_text that feeds MAAP parsing or auto-sizing routing.
fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut depth = 0u32;
    let mut tag_buf = String::new();
    for ch in text.chars() {
        tag_buf.push(ch);
        if depth > 0 {
            if tag_buf.ends_with("</think>") {
                depth = depth.saturating_sub(1);
                tag_buf.clear();
            }
        } else {
            if tag_buf.ends_with("<think>") {
                result.push_str(&tag_buf[..tag_buf.len() - "<think>".len()]);
                tag_buf.clear();
                depth = 1;
            } else if tag_buf.len() > 32 {
                result.push_str(&tag_buf);
                tag_buf.clear();
            }
        }
    }
    if depth == 0 {
        result.push_str(&tag_buf);
    }
    result
}

/// Builds a DeepSeek models listing HTTP request.
pub(super) fn build_deepseek_models_http_request(
    api_key: Option<&str>,
    chat_endpoint: &str,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("DeepSeek model listing credential", api_key)?;
    }
    let chat_endpoint = deepseek_chat_completions_endpoint_for_base_url(chat_endpoint)?;
    let models_endpoint = chat_endpoint.replace("/chat/completions", "/models");
    let mut headers = BTreeMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    if let Some(api_key) = api_key {
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    }
    Ok(ProviderHttpRequest {
        method: "GET".to_string(),
        url: if models_endpoint == chat_endpoint {
            DEEPSEEK_MODELS_ENDPOINT.to_string()
        } else {
            models_endpoint
        },
        headers,
        body: String::new(),
        timeout_ms,
        max_response_bytes: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ContextSourceKind, ModelMessage, PROVIDER_TRANSCRIPT_EVENT_MARKER};

    /// Builds a minimal DeepSeek model request for provider-shape tests.
    fn deepseek_test_request(messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
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
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::action_execution_base(),
            stop: None,
            messages,
        }
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

        let models = build_deepseek_models_http_request(
            Some("deepseek-key"),
            "https://api.deepseek.com",
            1000,
        )
        .unwrap();
        assert_eq!(models.url, "https://api.deepseek.com/models");
    }

    /// Verifies DeepSeek usage parsing follows the documented nested reasoning
    /// token shape while retaining compatibility with older flat responses.
    ///
    /// DeepSeek reports prompt cache accounting directly in `usage` and
    /// reasoning tokens under `completion_tokens_details.reasoning_tokens`.
    /// Capturing both fields keeps runtime cost and cache metrics accurate for
    /// thinking-mode sessions.
    #[test]
    fn deepseek_usage_parses_nested_reasoning_and_prompt_cache_hits() {
        let usage = parse_deepseek_usage(&serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 30,
            "prompt_cache_hit_tokens": 75,
            "prompt_cache_miss_tokens": 25,
            "completion_tokens_details": {
                "reasoning_tokens": 12
            }
        }));

        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.reasoning_tokens, 12);
        assert_eq!(usage.cached_input_tokens, Some(75));

        let flat = parse_deepseek_usage(&serde_json::json!({
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "reasoning_tokens": 3
        }));
        assert_eq!(flat.reasoning_tokens, 3);
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
        request.interaction_kind = ModelInteractionKind::Repair;

        let http = build_deepseek_chat_completions_http_request(
            &request,
            "deepseek-key",
            "https://api.deepseek.com/chat/completions",
            true,
            1000,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&http.body).unwrap();

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
        request.interaction_kind = ModelInteractionKind::Repair;

        let http = build_deepseek_chat_completions_http_request(
            &request,
            "deepseek-key",
            "https://api.deepseek.com/chat/completions",
            true,
            1000,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&http.body).unwrap();

        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(
            body["tool_choice"]["function"]["name"],
            DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME
        );
    }

    /// Verifies DeepSeek content fallbacks can still produce a valid MAAP
    /// batch when a proxy or model variant ignores the advertised function
    /// tool but returns the compact JSON object in assistant content.
    #[test]
    fn deepseek_response_parses_content_json_maap_fallback() {
        let request = deepseek_test_request(Vec::new());
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": serde_json::json!({
                        "rationale": "content fallback still produced structured output",
                        "actions": [{
                            "type": "say",
                            "status": "final",
                            "text": "hello"
                        }]
                    }).to_string()
                }
            }]
        })
        .to_string();

        let response = parse_deepseek_chat_completions_response_body(&body, &request).unwrap();

        let batch = response.action_batch.unwrap();
        assert_eq!(
            batch.rationale,
            "content fallback still produced structured output"
        );
        assert!(batch.final_turn);
    }

    /// Verifies DeepSeek auto-sizing responses preserve raw JSON instead of
    /// entering MAAP fallback parsing.
    ///
    /// Auto-sizing requests deliberately have no MAAP tool surface; DeepSeek is
    /// asked for one JSON router decision in assistant content. A leading `{`
    /// in that response must not be treated as malformed MAAP because the
    /// runtime auto-sizing router parses the raw provider text itself.
    #[test]
    fn deepseek_auto_sizing_response_preserves_json_content_without_maap_parse() {
        let mut request = deepseek_test_request(Vec::new());
        request.interaction_kind = ModelInteractionKind::AutoSizing;
        request.allowed_actions = AllowedActionSet::from_actions([]);
        let router_json = serde_json::json!({
            "version": 1,
            "size": "medium",
            "reasoning_effort": "high",
            "confidence": 0.82,
            "rationale": "coding task needs a medium model"
        })
        .to_string();
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": router_json
                }
            }]
        })
        .to_string();

        let response = parse_deepseek_chat_completions_response_body(&body, &request).unwrap();

        assert_eq!(response.raw_text, router_json);
        assert!(response.action_batch.is_none());
    }

    /// Verifies DeepSeek `finish_reason=length` uses output-limit recovery
    /// instead of malformed-MAAP repair.
    ///
    /// DeepSeek can cut assistant content in the middle of JSON when the
    /// completion hits `max_tokens`. Retrying with the MAAP repair prompt just
    /// asks the model to reinterpret a truncated object; the runtime already
    /// has a better output-limit recovery path that raises `max_output_tokens`
    /// and asks for one compact complete batch.
    #[test]
    fn deepseek_length_finish_reason_is_output_limit_error_for_partial_maap() {
        let request = deepseek_test_request(Vec::new());
        let partial_json = r#"{"actions":[{"type":"say","status":"blocked","text":"Need shell"}],"rationale":"need capability","thought":"partial"#;
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "finish_reason": "length",
                "message": {
                    "role": "assistant",
                    "content": partial_json
                }
            }]
        })
        .to_string();

        let error = parse_deepseek_chat_completions_response_body(&body, &request).unwrap_err();

        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
        assert!(crate::agent::provider_error_is_output_limit_exceeded(
            error.message(),
            error.provider_failure_json()
        ));
        assert_eq!(error.provider_raw_text(), Some(partial_json));
        let failure_json: serde_json::Value =
            serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
        assert_eq!(failure_json["provider"], "deepseek");
        assert_eq!(failure_json["finish_reason"], "length");
        assert_eq!(
            failure_json["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    /// Verifies malformed DeepSeek MAAP tool arguments are rejected as
    /// repairable malformed provider output.
    ///
    /// The prior parser converted failed argument parsing into `None`, which
    /// let the runner surface a generic missing-batch failure and discarded the
    /// actual malformed arguments needed for a repair retry.
    #[test]
    fn deepseek_response_rejects_malformed_maap_tool_arguments() {
        let request = deepseek_test_request(Vec::new());
        let malformed_arguments = serde_json::json!({
            "rationale": "missing action content",
            "actions": []
        })
        .to_string();
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                            "arguments": malformed_arguments
                        }
                    }]
                }
            }]
        })
        .to_string();

        let error = parse_deepseek_chat_completions_response_body(&body, &request).unwrap_err();

        assert!(
            error
                .message()
                .contains("provider MAAP output is malformed"),
            "{}",
            error.message()
        );
        assert_eq!(
            error.provider_raw_text(),
            Some(malformed_arguments.as_str())
        );
    }

    /// Verifies DeepSeek thinking-mode tool-call responses retain provider
    /// native replay metadata.
    ///
    /// DeepSeek requires the assistant `reasoning_content` and `tool_calls`
    /// from thinking-mode tool-call turns to be sent again on later requests.
    /// The provider parser therefore captures that native assistant envelope
    /// alongside the MAAP batch instead of flattening it into visible text.
    #[test]
    fn deepseek_response_captures_thinking_tool_call_transcript_event() {
        let request = deepseek_test_request(Vec::new());
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "I need to inspect the workspace first.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                            "arguments": serde_json::json!({
                                "rationale": "inspect before editing",
                                "actions": [{
                                    "id": "a1",
                                    "type": "shell_command",
                                    "summary": "list files",
                                    "command": "ls",
                                    "rationale": "find project files"
                                }],
                                "final_turn": false
                            }).to_string()
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4
            }
        })
        .to_string();

        let response = parse_deepseek_chat_completions_response_body(&body, &request).unwrap();

        assert!(response.action_batch.is_some());
        assert_eq!(response.provider_transcript_events.len(), 1);
        let ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content,
            reasoning_content,
            tool_calls,
        } = &response.provider_transcript_events[0]
        else {
            panic!("expected DeepSeek assistant tool-call event");
        };
        assert_eq!(content, "");
        assert_eq!(
            reasoning_content.as_deref(),
            Some("I need to inspect the workspace first.")
        );
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(
            tool_calls[0]["function"]["name"],
            OPENAI_MAAP_FUNCTION_TOOL_NAME
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
            content: "action_id=a1 status=success".to_string(),
        };
        let request = deepseek_test_request(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                content: "system prompt".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::TranscriptUser,
                content: "previous request".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                content: assistant_event.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                content: tool_event.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "continue".to_string(),
            },
        ]);

        let http = build_deepseek_chat_completions_http_request(
            &request,
            "deepseek-key",
            "https://api.deepseek.com/chat/completions",
            true,
            1000,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&http.body).unwrap();
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(body["stream"], true);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("tool_choice").is_none());
        assert!(!http.body.contains(PROVIDER_TRANSCRIPT_EVENT_MARKER.trim()));
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
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"], "continue");
    }
}
