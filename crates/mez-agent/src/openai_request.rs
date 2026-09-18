//! OpenAI Responses request construction.
//!
//! This module owns provider request-body construction for OpenAI Responses.
//! It depends on sibling modules for message rendering, cache keys, response
//! formatting, and MAAP schema selection while remaining independent of
//! credentials and transport orchestration.

use crate::model_capabilities::{OpenAiPromptCacheGeneration, OpenAiPromptCacheMode};
use crate::openai_cache::{
    openai_prompt_cache_key, openai_render_request_messages, openai_response_format,
};
use crate::openai_schema::openai_maap_action_batch_tools;
use crate::{
    MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME, ModelRequest,
    ProviderRequestAssemblyError, ProviderRequestAssemblyResult, openai_request_options,
    validate_provider_request_required,
};

/// Resolves an OpenAI Responses cache generation from one canonical model id.
pub(crate) fn inferred_openai_prompt_cache_generation(
    model: &str,
) -> Option<OpenAiPromptCacheGeneration> {
    let model = model.trim().to_ascii_lowercase();
    if let Some(suffix) = model.strip_prefix("gpt-") {
        let major = suffix
            .split(['.', '-'])
            .next()
            .and_then(|major| major.parse::<u32>().ok());
        if major.is_some_and(|major| major >= 6) {
            return Some(OpenAiPromptCacheGeneration::Gpt56OrNewer);
        }
    }
    if let Some(suffix) = model.strip_prefix("gpt-5.") {
        match suffix
            .split('-')
            .next()
            .and_then(|minor| minor.parse::<u32>().ok())
        {
            Some(5) => Some(OpenAiPromptCacheGeneration::Gpt55),
            Some(minor) if minor >= 6 => Some(OpenAiPromptCacheGeneration::Gpt56OrNewer),
            Some(_) => Some(OpenAiPromptCacheGeneration::Earlier),
            None => None,
        }
    } else if model == "gpt-4.5" || model.starts_with("gpt-4.5-") {
        Some(OpenAiPromptCacheGeneration::Gpt45)
    } else if ["gpt-4.1", "gpt-5"]
        .iter()
        .any(|family| model == *family || model.starts_with(&format!("{family}-")))
    {
        Some(OpenAiPromptCacheGeneration::Earlier)
    } else {
        None
    }
}

/// Adds the generation-compatible OpenAI cache control for one request.
fn apply_openai_prompt_cache_policy(
    body: &mut serde_json::Value,
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<()> {
    let retention = request.prompt_cache_retention.as_deref();
    match request
        .model_capabilities
        .openai_prompt_cache_generation
        .or_else(|| inferred_openai_prompt_cache_generation(&request.model))
    {
        Some(OpenAiPromptCacheGeneration::Earlier) => {
            if let Some(retention) = retention {
                if !matches!(retention, "in_memory" | "24h") {
                    return Err(ProviderRequestAssemblyError::invalid_args(format!(
                        "OpenAI model {:?} supports prompt_cache_retention values in_memory or 24h, got {retention:?}",
                        request.model
                    )));
                }
                body["prompt_cache_retention"] = serde_json::json!(retention);
            }
        }
        Some(OpenAiPromptCacheGeneration::Gpt45) => {
            if let Some(retention) = retention {
                if retention != "in_memory" {
                    return Err(ProviderRequestAssemblyError::invalid_args(format!(
                        "OpenAI model {:?} supports only prompt_cache_retention=in_memory, got {retention:?}",
                        request.model
                    )));
                }
                body["prompt_cache_retention"] = serde_json::json!(retention);
            }
        }
        Some(OpenAiPromptCacheGeneration::Gpt55) => {
            if let Some(retention) = retention {
                if retention != "24h" {
                    return Err(ProviderRequestAssemblyError::invalid_args(format!(
                        "OpenAI model {:?} supports only prompt_cache_retention=24h, got {retention:?}",
                        request.model
                    )));
                }
                body["prompt_cache_retention"] = serde_json::json!(retention);
            }
        }
        Some(OpenAiPromptCacheGeneration::Gpt56OrNewer) => {
            if let Some(retention) = retention
                && retention != "30m"
            {
                return Err(ProviderRequestAssemblyError::invalid_args(format!(
                    "OpenAI model {:?} uses prompt_cache_options.ttl=30m instead of prompt_cache_retention={retention:?}",
                    request.model
                )));
            }
            body["prompt_cache_options"] = serde_json::json!({ "ttl": "30m" });
            if request.model_capabilities.openai_prompt_cache_mode
                == OpenAiPromptCacheMode::Explicit
            {
                body["prompt_cache_options"]["mode"] = serde_json::json!("explicit");
            }
        }
        None => {
            if request.model_capabilities.openai_prompt_cache_mode
                == OpenAiPromptCacheMode::Explicit
            {
                return Err(ProviderRequestAssemblyError::invalid_args(format!(
                    "OpenAI model {:?} has no verified explicit prompt-cache support",
                    request.model
                )));
            }
            if let Some(retention) = retention {
                return Err(ProviderRequestAssemblyError::invalid_args(format!(
                    "OpenAI model {:?} has no verified prompt-cache retention support; refusing prompt_cache_retention={retention:?}",
                    request.model
                )));
            }
        }
    }
    Ok(())
}

/// Marks the explicit GPT-5.6+ cache boundary at the end of the stable prefix.
///
/// Explicit mode allows exactly one breakpoint, and the boundary must not move as
/// chronology appends newer developer-role blocks: the target is therefore the
/// last `input_text` block of the last stable-prefix developer message, never the
/// newest developer-role message anywhere in `input`. Marking the volatile tail
/// writes a suffix the provider cannot reuse, and rebuilding the marker on the
/// next request moves the boundary away from the previously paid write.
///
/// Local diagnostics measure the wire bytes including this marker
/// (`openai_prompt_cache_diagnostics_for_request_with_stream`), while the
/// send-path append-only gate compares marker-free canonical renders: the two
/// agree on which boundary is cached precisely because this position is stable
/// across appends, so the gate never observes the marker moving.
pub(crate) fn apply_openai_prompt_cache_breakpoint(
    request: &ModelRequest,
    stable_input_positions: &[usize],
    input: &mut [serde_json::Value],
) -> ProviderRequestAssemblyResult<()> {
    if request.model_capabilities.openai_prompt_cache_mode != OpenAiPromptCacheMode::Explicit {
        return Ok(());
    }
    let supported_generation = request
        .model_capabilities
        .openai_prompt_cache_generation
        .or_else(|| inferred_openai_prompt_cache_generation(&request.model))
        == Some(OpenAiPromptCacheGeneration::Gpt56OrNewer);
    if !supported_generation {
        return Err(ProviderRequestAssemblyError::invalid_args(format!(
            "OpenAI model {:?} has no verified explicit prompt-cache support",
            request.model
        )));
    }
    let block = input
        .stable_input_positions_marker_target(stable_input_positions)
        .ok_or_else(|| {
            ProviderRequestAssemblyError::invalid_args(
                "OpenAI explicit prompt-cache mode requires a stable-prefix developer input_text content block",
            )
        })?;
    block["prompt_cache_breakpoint"] = serde_json::json!({ "mode": "explicit" });
    Ok(())
}

/// Selects the last `input_text` block of the last stable developer message.
trait StableInputMarkerTarget {
    /// Returns the block the explicit breakpoint may attach to.
    fn stable_input_positions_marker_target(
        &mut self,
        stable_input_positions: &[usize],
    ) -> Option<&mut serde_json::Value>;
}

impl StableInputMarkerTarget for [serde_json::Value] {
    fn stable_input_positions_marker_target(
        &mut self,
        stable_input_positions: &[usize],
    ) -> Option<&mut serde_json::Value> {
        // Find the coordinates with immutable reads first: a mutable scan that
        // returns a reference cannot be written as a loop, because the returned
        // reference would outlive each iteration's borrow of the slice.
        let (message_position, block_index) =
            stable_input_positions.iter().rev().find_map(|position| {
                let message = self.get(*position)?;
                if message.get("role").and_then(serde_json::Value::as_str) != Some("developer") {
                    return None;
                }
                let content = message.get("content")?.as_array()?;
                let block_index = content.iter().rposition(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("input_text")
                })?;
                Some((*position, block_index))
            })?;
        self.get_mut(message_position)?
            .get_mut("content")?
            .as_array_mut()?
            .get_mut(block_index)
    }
}

/// Builds a non-streaming OpenAI Responses request body.
///
/// The returned JSON includes the rendered prompt, prompt-cache routing key,
/// selected MAAP tool surface, response format, and provider-specific request
/// options derived from the model profile.
pub fn openai_responses_request_body(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<String> {
    openai_responses_request_body_with_stream(request, false)
}

/// Builds an OpenAI Responses request body with explicit stream selection.
///
/// The provider facade uses this helper for HTTP request construction so the
/// streaming and non-streaming request shapes remain identical except for the
/// `stream` field.
pub fn openai_responses_request_body_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> ProviderRequestAssemblyResult<String> {
    validate_provider_request_required("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let mut body = openai_responses_request_control_shape_with_stream(request, stream)?;
    body["instructions"] = serde_json::json!(rendered.instructions);
    let mut input = rendered.input;
    apply_openai_prompt_cache_breakpoint(request, &rendered.stable_input_positions, &mut input)?;
    body["input"] = serde_json::json!(input);
    body["prompt_cache_key"] = serde_json::json!(openai_prompt_cache_key(request));
    serde_json::to_string(&body).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI request encoding failed: {error}"
        ))
    })
}

/// Builds the canonical OpenAI request-control shape shared by request
/// emission and prompt-cache diagnostics.
pub(crate) fn openai_responses_request_control_shape_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> ProviderRequestAssemblyResult<serde_json::Value> {
    validate_provider_request_required("OpenAI model", &request.model)?;
    let mut body = serde_json::json!({
        "model": request.model,
        "parallel_tool_calls": false,
        "store": false,
        "stream": stream
    });
    if let Some(response_format) = openai_response_format(request) {
        body["text"] = serde_json::json!({
            "format": response_format
        });
    }
    let options = openai_request_options(
        request.reasoning_effort.as_deref(),
        request.latency_preference.as_deref(),
    )?;
    if let Some(effort) = options.reasoning_effort {
        body["reasoning"] = serde_json::json!({ "effort": effort });
    }
    if let Some(service_tier) = options.service_tier {
        body["service_tier"] = serde_json::json!(service_tier);
    }
    apply_openai_prompt_cache_policy(&mut body, request)?;
    if request.interaction_kind.expects_structured_json() {
        body["tool_choice"] = serde_json::json!("none");
    } else if request.interaction_kind.expects_maap_batch() {
        body["tools"] = serde_json::json!(openai_maap_action_batch_tools(request));
        body["tool_choice"] = serde_json::json!({
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
            "type": "function"
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowedActionSet, ModelInteractionKind};

    /// Verifies a compaction request retains its captured session action
    /// catalog while the Responses serializer emits no executable tools.
    #[test]
    fn openai_responses_compaction_omits_tools_for_session_catalog() {
        let request = ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            model_capabilities: Default::default(),
            max_input_tokens: None,
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: String::new(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: ModelInteractionKind::Compaction,
            allowed_actions: AllowedActionSet::all_enabled(),
            stop: None,
            messages: Vec::new().into(),
        };

        let body = openai_responses_request_control_shape_with_stream(&request, false).unwrap();

        assert!(body.get("tools").is_none(), "{body}");
        assert!(body.get("tool_choice").is_none(), "{body}");
    }
}
