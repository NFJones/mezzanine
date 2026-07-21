//! Provider-independent DeepSeek endpoint and protocol policy.
//!
//! This module owns deterministic DeepSeek endpoint derivation, request
//! strategy, thinking controls, native transcript replay, shim schemas, and
//! JSON request construction. Product adapters retain credentials, HTTP
//! metadata, transport, quota attachment, and error projection; deterministic
//! response parsing lives in the sibling `deepseek_response` module.

use crate::{
    AgentCapability, AllowedAction, AllowedActionSet,
    MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME, McpPromptTool,
    ModelInteractionKind, ModelMessageRole, ModelRequest, ProviderApiCompatibility,
    ProviderCapabilities, ProviderEndpointError, ProviderEndpointResult,
    ProviderRequestAssemblyError, ProviderRequestAssemblyResult, ProviderTranscriptEvent,
    maap_action_batch_schema, mcp_tool_manifest_for_description,
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
    fn for_request(request: &ModelRequest) -> Self {
        if request.interaction_kind == ModelInteractionKind::CapabilityDecision
            && request.allowed_actions == AllowedActionSet::capability_decision()
        {
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

/// Builds the JSON body for a DeepSeek request with an explicit MAAP strategy.
pub fn deepseek_chat_completions_request_body_with_strategy(
    request: &ModelRequest,
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<String> {
    let capabilities =
        ProviderCapabilities::for_api(ProviderApiCompatibility::DeepSeekChatCompletions);
    let mut messages = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        if let Some(event) = ProviderTranscriptEvent::from_transcript_content(&message.content) {
            messages.push(deepseek_provider_transcript_event_message(&event));
            continue;
        }
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
    request: &ModelRequest,
    shim_kind: DeepSeekMaapShimKind,
) -> String {
    let capability_map = "Capability map: shell=local files, rg/sed/cat, git, builds, tests, shell_command, and apply_patch; network_search=web_search; network_fetch=fetch_url; mcp=mcp_call; subagent=send_message or spawn_agent; config_change=config_change; memory=memory_search or memory_store; issues=issue_add, issue_update, issue_query, or issue_delete; respond_only=final text only.";
    let anti_examples = "Wrong: say(blocked, \"Need shell capability\"). Right: request_capability(capability=\"shell\", reason=\"Need to inspect repository files\"). Wrong: say(blocked, \"Shell capability is absent\") or say describing what is missing. Right: request_capability for the missing capability immediately. Wrong: *** Replace File. Right: *** Update File with anchored hunks. Wrong: inferred apply_patch old context. Right: copy old/context lines verbatim from read file evidence.";
    let routing_rule = "CRITICAL ROUTING RULE: When request_capability is in the allowed action types and an executable action type is needed but absent, request_capability is the ONLY correct response. Never emit a say action describing, diagnosing, or lamenting the absence of a capability. request_capability IS how you obtain the missing capability within the same turn; emit it immediately without any preceding say. If missing information, parameters, or identifiers can be safely gathered from current context, local artifacts, web results, MCP results, or another requestable action, request the relevant capability instead of asking the user.";
    let action_batch_envelope_rule = "The function call is only the transport envelope for the action batch, not a prerequisite task step; do not emit a say-only or progress batch claiming that an initial or schema-valid batch is needed before the executable action, and do not put required-function-call compliance language in rationale or thought fields. If an executable action is available and useful, put that action in this function call now.";
    match shim_kind {
        DeepSeekMaapShimKind::CapabilityDecision => format!(
            "Decide the next Mezzanine capability through this function. Return a function call, not prose. The arguments are translated into one internal MAAP/1 request_capability batch unless no external action capability is needed. {routing_rule} If any local or external action would help, choose request_capability via the capability and reason fields only; a missing shell, network_search, network_fetch, mcp, subagent, config_change, memory, issues, or respond_only action surface is not a blocker. {capability_map} {anti_examples}"
        ),
        DeepSeekMaapShimKind::RespondOnly => {
            "Submit one user-facing response through this function. Return a function call, not prose. The arguments are translated into one internal MAAP/1 say action. Only progress, final, or blocked say text is valid; do not request tools or capabilities from this response-only surface.".to_string()
        }
        DeepSeekMaapShimKind::ActionDispatch => format!(
            "Submit exactly one MAAP/1 action batch through this function. Return a function call, not prose. {action_batch_envelope_rule} Current allowed action types: {}. Use only the action objects in this function schema. {} {} {routing_rule} If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. {capability_map} {anti_examples}",
            request.allowed_actions.action_type_names().join(","),
            deepseek_mcp_memory_routing_guidance(request),
            mcp_tool_manifest_for_description(&request.available_mcp_tools),
        ),
    }
}

/// Builds MCP-specific routing guidance for composite DeepSeek action surfaces.
fn deepseek_mcp_memory_routing_guidance(request: &ModelRequest) -> &'static str {
    if !request
        .allowed_actions
        .actions
        .contains(&AllowedAction::McpCall)
    {
        return "";
    }
    "If this schema includes mcp_call, the MCP server and tool names are visible in the mcp_call variants; use them directly when the user names a matching server or the task matches visible MCP metadata. Do not use memory_search, memory_store, shell preflight, or request_capability for shell, network_search, or network_fetch merely to set up a useful MCP call. If MCP arguments such as identifiers, URLs, paths, repo owner/name, branch, commit, issue/PR number, or CI target can be derived from current action results or safely gathered context, use those results directly; if they require another absent capability, request it. Do not use memory_search to decide whether visible MCP metadata, action descriptions, or current action results are sufficient. Memory is optional support here, not a prerequisite for acting. If a direct path is unclear, use current action results, adjust or broaden a direct integration query, inspect a direct artifact, or report a bounded blocker. If runtime skips or rejects a memory action, continue with current action results, MCP, shell, web, or a bounded report instead of searching memory again unless new evidence creates a specific durable prior-context gap. If memory_search is present, use it only for a concrete durable prior-context gap; use at most one focused search in ordinary turns, never more than two in one user turn, and never emit duplicate memory_search actions in one batch."
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

    use crate::{ContextSourceKind, ModelMessage, PROVIDER_TRANSCRIPT_EVENT_MARKER};

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
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::action_execution_base(),
            stop: None,
            messages,
        }
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
            DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME
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
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"], "continue");
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
